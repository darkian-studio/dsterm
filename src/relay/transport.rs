#![allow(dead_code)]

use crate::config::DstermConfig;
use crate::protocol::{is_plaintext_allowed, IncomingMsg};
use crate::relay::agents::RelayAgents;
use crate::relay::clients::{ApprovalDecision, ClientInfo, ClientStore};
use crate::relay::crypto::Secretbox;
use crate::relay::dispatch::dispatch;
use crate::relay::proxy_ws::RelayProxyWs;
use crate::relay::sysmon::RelaySysmon;
use crate::relay::terminal::RelayTerminals;
use crate::relay::wire::ClientCtx;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
fn http_client() -> reqwest::Client {
    HTTP_CLIENT.get_or_init(reqwest::Client::new).clone()
}

/// Run the relay client forever, reconnecting with a fixed backoff ladder.
pub async fn run(config: DstermConfig, secretbox: Secretbox, host_id: String, local_port: u16) {
    // Fail fast on an unhonorable server_url: no reconnect ladder spinning
    // over a config that can never connect.
    if let Err(e) = super::validated_relay_base(&config.relay.server_url) {
        tracing::error!("relay server_url rejected: {e}");
        return;
    }
    if config.relay.server_url == "https://localhost:3000" {
        tracing::warn!("relay server_url is default https://localhost:3000 — set relay.server_url in config for production");
    }
    let ladder = if config.relay.reconnect_secs.is_empty() {
        vec![1u64, 2, 5, 10, 30]
    } else {
        config.relay.reconnect_secs.clone()
    };
    let mut attempt = 0usize;
    // One nonce cache for the host process, not per connection: a captured
    // frame replayed into the NEXT reconnect must meet the same memory.
    let mut replay = ReplayGuard::new(REPLAY_CACHE_ENTRIES);
    loop {
        match connect_once(&config, &secretbox, &host_id, local_port, &mut replay).await {
            Ok(()) => {
                tracing::info!("relay connection closed");
                attempt = 0;
            }
            Err(e) => tracing::error!("relay connection error: {e}"),
        }
        let idx = attempt.min(ladder.len() - 1);
        let jitter = (attempt as u64) % 3;
        let delay = ladder[idx].saturating_add(jitter);
        attempt = attempt.saturating_add(1);
        tracing::info!("reconnecting to relay in {delay}s");
        tokio::time::sleep(Duration::from_secs(delay)).await;
    }
}

/// Frames are AEAD'd with one shared host key and the nonce roams free in
/// the envelope: a captured (nonce, ciphertext) pair re-sent verbatim
/// decrypts perfectly every time, and the read loop could not tell a
/// replay from a first delivery. This guard remembers decoded nonce bytes
/// and refuses duplicates, FIFO-evicting the oldest once the window is
/// full. First-delivery races against a frame-reordering attacker (the
/// relay itself) remain possible - transport hardening and real
/// per-client identity each shrink that - the guard's contract is to end
/// INDEFINITE replay after capture.
const REPLAY_CACHE_ENTRIES: usize = 65_536;

pub struct ReplayGuard {
    seen: std::collections::HashSet<Vec<u8>>,
    order: std::collections::VecDeque<Vec<u8>>,
    capacity: usize,
}

impl ReplayGuard {
    pub fn new(capacity: usize) -> Self {
        Self {
            seen: std::collections::HashSet::new(),
            order: std::collections::VecDeque::new(),
            capacity,
        }
    }

    /// Admit a decoded nonce when it is fresh. Returns false for a
    /// replay (nonce already seen inside the window).
    pub fn admit(&mut self, nonce: &[u8]) -> bool {
        if !self.seen.insert(nonce.to_vec()) {
            return false;
        }
        self.order.push_back(nonce.to_vec());
        while self.order.len() > self.capacity {
            if let Some(evicted) = self.order.pop_front() {
                self.seen.remove(&evicted);
            }
        }
        true
    }
}

/// Which key decrypted an inbound frame — also the key host→client replies
/// for that client must be sealed under (#72).
#[derive(Clone)]
pub(crate) enum ResolvedKey {
    /// Pinned per-client identity key. No fallback to the master: a frame
    /// sealed with anything else is dropped, which is what makes the pin a
    /// boundary instead of a hint.
    Pinned(Secretbox),
    /// Pending pairing key bound to this client on this very frame.
    Pairing(Secretbox),
    /// Legacy shared master (a pairing from before #72).
    Shared(Secretbox),
}

impl ResolvedKey {
    fn secretbox(&self) -> &Secretbox {
        match self {
            ResolvedKey::Pinned(key) | ResolvedKey::Pairing(key) | ResolvedKey::Shared(key) => key,
        }
    }
}

/// Parse a decrypted frame: the v2 wrapper (`{clientId, msg}`) first, the
/// legacy bare message second. The `Option<String>` is the bound inner id.
pub(crate) fn parse_sealed(
    plaintext: &[u8],
) -> Option<(crate::protocol::IncomingMsg, Option<String>)> {
    if let Ok(sealed) = serde_json::from_slice::<crate::protocol::SealedInbound>(plaintext) {
        return Some((sealed.msg, Some(sealed.client_id)));
    }
    serde_json::from_slice::<crate::protocol::IncomingMsg>(plaintext)
        .ok()
        .map(|msg| (msg, None))
}

/// Resolve the sealing key for one inbound frame, in priority order: pinned
/// identity (closed to everything else), pending pairings (first-use
/// binding), shared master (legacy). Only approved clients reach this —
/// the approval gate runs first — so a pairing key only ever binds to a
/// client the operator admitted.
pub(crate) fn resolve_inbound(
    client_id: &str,
    nonce: &str,
    ciphertext: &str,
    master: &Secretbox,
    store: &mut ClientStore,
) -> Option<(ResolvedKey, Vec<u8>)> {
    if let Some(key_base64) = store.identity_key(client_id) {
        // Fail closed on a corrupt stored key: a pinned client must never
        // silently fall back to the shared master it was pinned away from.
        let key = Secretbox::from_base64_key(&key_base64).ok()?;
        let plaintext = key.decrypt_parts(nonce, ciphertext).ok()?;
        return Some((ResolvedKey::Pinned(key), plaintext));
    }
    for key_base64 in store.pending_keys() {
        let Ok(key) = Secretbox::from_base64_key(&key_base64) else {
            continue;
        };
        if let Ok(plaintext) = key.decrypt_parts(nonce, ciphertext) {
            match store.pin_identity(client_id, &key_base64) {
                Ok(true) => tracing::info!("client {client_id} bound to its pairing key (#72)"),
                Ok(false) => tracing::warn!(
                    "pairing key matched a frame for {client_id}, but it is already pinned; binding not taken"
                ),
                Err(e) => tracing::warn!("identity binding for {client_id} failed to persist: {e}"),
            }
            return Some((ResolvedKey::Pairing(key), plaintext));
        }
    }
    let plaintext = master.decrypt_parts(nonce, ciphertext).ok()?;
    Some((ResolvedKey::Shared(master.clone()), plaintext))
}

async fn connect_once(
    config: &DstermConfig,
    secretbox: &Secretbox,
    host_id: &str,
    local_port: u16,
    replay: &mut ReplayGuard,
) -> anyhow::Result<()> {
    // Re-validate per attempt too: run() fronts the common path, but
    // connect_once is also called by commands/tests that skip run().
    super::validated_relay_base(&config.relay.server_url)?;
    let url = cli_url(&config.relay.server_url, host_id);
    tracing::info!("connecting to relay {url}");
    let (ws, _) = tokio_tungstenite::connect_async(url.as_str()).await?;
    let (mut write, mut read) = ws.split();

    let host_msg = json!({
        "type": "session:host",
        "hostId": host_id,
        "machineId": crate::relay::register::machine_id(),
        "platform": std::env::consts::OS,
    });
    write.send(Message::Text(host_msg.to_string())).await?;

    let (out_tx, out_rx) = mpsc::channel::<String>(1024);
    let writer = tokio::spawn(async move {
        let mut write = write;
        let mut out_rx = out_rx;
        while let Some(frame) = out_rx.recv().await {
            if write.send(Message::Text(frame)).await.is_err() {
                break;
            }
        }
    });

    let hb_secs = if config.relay.heartbeat_secs == 0 {
        25
    } else {
        config.relay.heartbeat_secs
    };
    let last_pong = Arc::new(tokio::sync::Mutex::new(std::time::Instant::now()));
    let last_pong_hb = last_pong.clone();
    let hb_tx = out_tx.clone();
    let heartbeat = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(hb_secs));
        loop {
            tick.tick().await;
            if last_pong_hb.lock().await.elapsed() > Duration::from_secs(hb_secs * 2) {
                tracing::warn!("relay pong timeout, reconnecting");
                break;
            }
            if hb_tx
                .send(json!({ "type": "ping" }).to_string())
                .await
                .is_err()
            {
                break;
            }
        }
    });

    let http = http_client();
    let terminals = RelayTerminals::new();
    let agents = RelayAgents::new();
    let proxy_ws = RelayProxyWs::new();
    let sysmon = RelaySysmon::new();
    let mut approved: HashSet<String> = HashSet::new();
    // Per-connection identity cache (#72): the key each approved client's
    // frames actually decrypt under, resolved lazily and reused so replies
    // seal under the same key. Cleared on join/left so unpin and
    // re-pairing take effect on the next event instead of forever.
    let mut resolved: HashMap<String, ResolvedKey> = HashMap::new();
    // Clients that sent a v2 frame with the clientId bound inside the AEAD
    // payload; their replies are wrapped the same way (#72).
    let mut v2_speakers: HashSet<String> = HashSet::new();

    let loop_result: anyhow::Result<()> = async {
        while let Some(msg) = read.next().await {
            let msg = msg?;
            let text = match msg {
                Message::Text(text) => text.to_string(),
                Message::Binary(bytes) => match String::from_utf8(bytes.to_vec()) {
                    Ok(text) => text,
                    Err(_) => continue,
                },
                Message::Ping(_) | Message::Pong(_) => continue,
                Message::Close(_) => break,
                _ => continue,
            };
            let value: Value = match serde_json::from_str(&text) {
                Ok(value) => value,
                Err(_) => continue,
            };
            let msg_type = value
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string();

            if is_plaintext_allowed(&msg_type) {
                handle_control(
                    &msg_type,
                    &value,
                    config,
                    &out_tx,
                    &mut approved,
                    &mut resolved,
                    &last_pong,
                )
                .await?;
                continue;
            }

            if msg_type == "encrypted" {
                let client_id = value
                    .get("clientId")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string();
                if client_id.is_empty() {
                    continue;
                }
                if !approved.contains(&client_id) {
                    tracing::warn!("dropping message from unapproved client {client_id}");
                    continue;
                }
                let nonce = value.get("nonce").and_then(|n| n.as_str()).unwrap_or("");
                let ciphertext = value
                    .get("ciphertext")
                    .and_then(|c| c.as_str())
                    .unwrap_or("");
                // Replays die here: the nonce is remembered for the host
                // process lifetime (across reconnects) and a duplicate is
                // refused before the decryption costs anything.
                let decoded = match BASE64.decode(nonce) {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        tracing::warn!("relay frame with undecodable nonce rejected");
                        continue;
                    }
                };
                if !replay.admit(&decoded) {
                    tracing::warn!("dropping replayed relay frame (nonce seen before)");
                    continue;
                }
                // Key resolution is per-client (#72): try the connection
                // cache first, and on a miss or a stale key fall back to
                // the full priority walk (pinned -> pairing -> master).
                let cached_hit = resolved
                    .get(&client_id)
                    .cloned()
                    .and_then(|key| {
                        key.secretbox()
                            .decrypt_parts(nonce, ciphertext)
                            .ok()
                            .map(|plaintext| (key, plaintext))
                    });
                let (key, plaintext) = match cached_hit {
                    Some(hit) => hit,
                    None => {
                        let mut store = match ClientStore::load_or_default(
                            config.security.clients_file.as_deref(),
                        ) {
                            Ok(store) => store,
                            Err(e) => {
                                tracing::warn!("clients store unavailable, dropping frame: {e}");
                                continue;
                            }
                        };
                        match resolve_inbound(
                            &client_id,
                            nonce,
                            ciphertext,
                            secretbox,
                            &mut store,
                        ) {
                            Some(hit) => {
                                resolved.insert(client_id.clone(), hit.0.clone());
                                hit
                            }
                            None => {
                                tracing::warn!(
                                    "dropping relay frame for {client_id}: not sealed with any key it holds"
                                );
                                continue;
                            }
                        }
                    }
                };
                let (incoming, inner_id) = match parse_sealed(&plaintext) {
                    Some(parsed) => parsed,
                    None => {
                        tracing::warn!("invalid inner message");
                        continue;
                    }
                };
                if let Some(inner) = inner_id {
                    if !crate::terminal::constant_time_eq(inner.as_bytes(), client_id.as_bytes())
                    {
                        tracing::warn!(
                            "dropping relay frame for {client_id}: inner clientId differs from the outer routing label (relay tampering?)"
                        );
                        continue;
                    }
                    // This client speaks v2; keep its replies wrapped the
                    // same way, since a legacy parser would drop the wrapper.
                    v2_speakers.insert(client_id.clone());
                }
                let wrap_inner = v2_speakers.contains(&client_id);
                let ctx = ClientCtx::new(client_id, key.secretbox().clone(), out_tx.clone(), wrap_inner);
                route(
                    incoming, &ctx, &http, local_port, &terminals, &agents, &proxy_ws, &sysmon,
                )
                .await;
            }
        }
        Ok(())
    }
    .await;

    terminals.shutdown();
    agents.shutdown();
    proxy_ws.shutdown();
    sysmon.shutdown();
    heartbeat.abort();
    writer.abort();
    loop_result
}

async fn handle_control(
    msg_type: &str,
    value: &Value,
    config: &DstermConfig,
    out_tx: &mpsc::Sender<String>,
    approved: &mut HashSet<String>,
    resolved: &mut HashMap<String, ResolvedKey>,
    last_pong: &std::sync::Arc<tokio::sync::Mutex<std::time::Instant>>,
) -> anyhow::Result<()> {
    match msg_type {
        "ping" => {
            let _ = out_tx.send(json!({ "type": "pong" }).to_string()).await;
        }
        "pong" => {
            *last_pong.lock().await = std::time::Instant::now();
        }
        "session:hosted" => {
            let sid = value
                .get("sessionId")
                .and_then(|s| s.as_str())
                .unwrap_or("");
            tracing::info!("relay session established (sessionId={sid})");
        }
        "session:error" => {
            let err = value
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unknown");
            anyhow::bail!("relay session error: {err}");
        }
        "session:client-join" => {
            let client_id = value
                .get("clientId")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            if client_id.is_empty() {
                return Ok(());
            }
            let info = parse_client_info(value);
            let decision = decide_client(config, &client_id, info);
            let allow = matches!(decision, ApprovalDecision::Allow);
            if allow {
                approved.insert(client_id.clone());
            } else {
                approved.remove(&client_id);
                tracing::info!("client {client_id} not approved (decision: {decision:?})");
            }
            // A join is a fresh identity event: drop any cached key so
            // unpin/re-pair is picked up by this client's next frame.
            resolved.remove(&client_id);
            let reply = json!({
                "type": "session:client-approve",
                "clientId": client_id,
                "approved": allow,
            });
            let _ = out_tx.send(reply.to_string()).await;
        }
        "session:client-left" => {
            if let Some(client_id) = value.get("clientId").and_then(|c| c.as_str()) {
                approved.remove(client_id);
                resolved.remove(client_id);
            }
        }
        _ => {}
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn route(
    incoming: IncomingMsg,
    ctx: &ClientCtx,
    http: &reqwest::Client,
    local_port: u16,
    terminals: &RelayTerminals,
    agents: &RelayAgents,
    proxy_ws: &RelayProxyWs,
    sysmon: &RelaySysmon,
) {
    match incoming {
        IncomingMsg::TerminalCreate {
            id,
            cols,
            rows,
            cwd,
        } => {
            terminals
                .create(ctx, http, local_port, id, cols, rows, cwd)
                .await;
        }
        IncomingMsg::TerminalData {
            terminal_id, data, ..
        } => {
            terminals.data(ctx, &terminal_id, &data).await;
        }
        IncomingMsg::TerminalResize {
            id,
            terminal_id,
            cols,
            rows,
        } => {
            terminals
                .resize(ctx, http, local_port, id, &terminal_id, cols, rows)
                .await;
        }
        IncomingMsg::TerminalClose { id, terminal_id } => {
            terminals
                .close(ctx, http, local_port, id, &terminal_id)
                .await;
        }
        IncomingMsg::TerminalList { id } => {
            terminals.list(ctx, http, local_port, id).await;
        }
        IncomingMsg::TerminalAttach { id, terminal_id } => {
            terminals.attach(ctx, local_port, id, &terminal_id).await;
        }
        IncomingMsg::AgentsStart {
            id,
            command,
            args,
            cwd,
            env,
        } => {
            agents
                .start(ctx, http, local_port, id, command, args, cwd, env)
                .await;
        }
        IncomingMsg::AgentsInput { agent_id, data, .. } => {
            agents.input(ctx, &agent_id, &data).await;
        }
        IncomingMsg::AgentsKill { id, agent_id } => {
            agents.kill(ctx, http, local_port, id, &agent_id).await;
        }
        IncomingMsg::WsOpen { id, url } => {
            if !crate::proxy::is_localhost(&url) {
                ctx.send_error(id, "Only localhost ws targets allowed")
                    .await;
            } else {
                proxy_ws.open(ctx, id, url).await;
            }
        }
        IncomingMsg::WsData {
            ws_id,
            data,
            binary,
            ..
        } => {
            proxy_ws.data(ctx, &ws_id, &data, binary).await;
        }
        IncomingMsg::WsClose { ws_id, .. } => {
            proxy_ws.close(&ws_id).await;
        }
        IncomingMsg::SysmonSubscribe { id } => {
            sysmon.subscribe(ctx, http, local_port, id).await;
        }
        IncomingMsg::SysmonUnsubscribe { id } => {
            sysmon.unsubscribe(ctx, id).await;
        }
        other => dispatch(other, ctx, http, local_port).await,
    }
}

fn decide_client(config: &DstermConfig, client_id: &str, info: ClientInfo) -> ApprovalDecision {
    match ClientStore::load_or_default(config.security.clients_file.as_deref()) {
        Ok(mut store) => match store.decide(client_id, info, &config.security.unknown_clients) {
            Ok(decision) => decision,
            Err(e) => {
                tracing::error!("client decision failed: {e}");
                ApprovalDecision::Reject
            }
        },
        Err(e) => {
            tracing::error!("clients store load failed: {e}");
            ApprovalDecision::Reject
        }
    }
}

fn parse_client_info(value: &Value) -> ClientInfo {
    let info = value.get("clientInfo").cloned().unwrap_or(Value::Null);
    ClientInfo {
        platform: info
            .get("platform")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        app_version: info
            .get("appVersion")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        device: info
            .get("device")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    }
}

fn cli_url(server_url: &str, host_id: &str) -> String {
    let base = server_url.trim_end_matches('/');
    let ws_base = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base.to_string()
    };
    format!("{ws_base}/cli?hostId={host_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A captured frame's nonce is refused on the second delivery.
    #[test]
    fn replay_guard_refuses_duplicate_nonce() {
        let mut guard = ReplayGuard::new(4);
        assert!(guard.admit(b"nonce-one"));
        assert!(!guard.admit(b"nonce-one"));
        assert!(guard.admit(b"nonce-two"));
        assert!(!guard.admit(b"nonce-two"));
        assert!(guard.admit(b"nonce-three"));
    }

    #[test]
    fn replay_guard_window_evicts_fifo() {
        let mut guard = ReplayGuard::new(2);
        assert!(guard.admit(b"a"));
        assert!(guard.admit(b"b"));
        assert!(guard.admit(b"c")); // evicts `a`
                                    // `b` and `c` are still inside the window: duplicates dead.
        assert!(!guard.admit(b"b"));
        assert!(!guard.admit(b"c"));
        // `a` fell out of the window: accepted again (window semantics).
        assert!(guard.admit(b"a"));
    }

    #[test]
    fn parse_sealed_accepts_v2_wrapper_and_legacy_bare() {
        let bare = br#"{"type":"ping","id":"1"}"#;
        let (msg, inner) = parse_sealed(bare).unwrap();
        assert!(matches!(msg, crate::protocol::IncomingMsg::Ping { .. }));
        assert_eq!(inner, None);

        let wrapped = br#"{"clientId":"client-a","msg":{"type":"ping","id":"1"}}"#;
        let (msg, inner) = parse_sealed(wrapped).unwrap();
        assert!(matches!(msg, crate::protocol::IncomingMsg::Ping { .. }));
        assert_eq!(inner.as_deref(), Some("client-a"));
    }

    /// The full priority walk: pairing binds on first use, the pin then
    /// closes the master fallback for that client, and unpin reopens it.
    #[test]
    fn resolve_inbound_walks_pairing_then_pinned_then_master() {
        let path =
            std::env::temp_dir().join(format!("dsterm-resolve-{}.json", uuid::Uuid::new_v4()));
        let mut store = ClientStore::load_or_default(Some(path.to_str().unwrap())).unwrap();
        store
            .decide(
                "client-a",
                ClientInfo::default(),
                &crate::config::UnknownClientPolicy::RequiresApproval,
            )
            .unwrap();

        let master = Secretbox::from_key_bytes([7; 32]);
        let pairing = Secretbox::from_key_bytes([5; 32]);
        let stranger = Secretbox::from_key_bytes([9; 32]);
        store.add_pairing(pairing.key_base64()).unwrap();

        let frame_for = |key: &Secretbox| {
            let payload = key.encrypt(b"secret").unwrap();
            (payload.nonce, payload.ciphertext)
        };

        // A frame under the pending pairing key: accepted AND bound now.
        let (nonce, ciphertext) = frame_for(&pairing);
        let (key, plaintext) =
            resolve_inbound("client-a", &nonce, &ciphertext, &master, &mut store).unwrap();
        assert!(matches!(key, ResolvedKey::Pairing(_)));
        assert_eq!(plaintext, b"secret");
        assert_eq!(
            store.identity_key("client-a").as_deref(),
            Some(pairing.key_base64().as_str())
        );
        assert_eq!(store.pending_count(), 0);

        // Pinned client, correct key: still accepted, now as Pinned.
        let (nonce, ciphertext) = frame_for(&pairing);
        let (key, _) =
            resolve_inbound("client-a", &nonce, &ciphertext, &master, &mut store).unwrap();
        assert!(matches!(key, ResolvedKey::Pinned(_)));

        // Pinned client sealed with the shared master: rejected. The pin
        // must close the fallback, or it is not a boundary.
        let (nonce, ciphertext) = frame_for(&master);
        assert!(resolve_inbound("client-a", &nonce, &ciphertext, &master, &mut store).is_none());

        // A key nobody handed out: rejected outright.
        let (nonce, ciphertext) = frame_for(&stranger);
        assert!(resolve_inbound("client-a", &nonce, &ciphertext, &master, &mut store).is_none());

        // Unpin reopens the legacy path.
        store.unpin("client-a").unwrap();
        let (nonce, ciphertext) = frame_for(&master);
        let (key, _) =
            resolve_inbound("client-a", &nonce, &ciphertext, &master, &mut store).unwrap();
        assert!(matches!(key, ResolvedKey::Shared(_)));

        let _ = std::fs::remove_file(path);
    }
}
