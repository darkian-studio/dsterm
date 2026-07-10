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
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

/// Run the relay client forever, reconnecting with a fixed backoff ladder.
pub async fn run(config: DstermConfig, secretbox: Secretbox, host_id: String, local_port: u16) {
    let ladder = if config.relay.reconnect_secs.is_empty() {
        vec![1u64, 2, 5, 10, 30]
    } else {
        config.relay.reconnect_secs.clone()
    };
    let mut attempt = 0usize;
    loop {
        match connect_once(&config, &secretbox, &host_id, local_port).await {
            Ok(()) => {
                tracing::info!("relay connection closed");
                attempt = 0;
            }
            Err(e) => tracing::error!("relay connection error: {e}"),
        }
        let idx = attempt.min(ladder.len() - 1);
        let delay = ladder[idx];
        attempt = attempt.saturating_add(1);
        tracing::info!("reconnecting to relay in {delay}s");
        tokio::time::sleep(Duration::from_secs(delay)).await;
    }
}

async fn connect_once(
    config: &DstermConfig,
    secretbox: &Secretbox,
    host_id: &str,
    local_port: u16,
) -> anyhow::Result<()> {
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
    let hb_tx = out_tx.clone();
    let heartbeat = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(hb_secs));
        loop {
            tick.tick().await;
            if hb_tx
                .send(json!({ "type": "ping" }).to_string())
                .await
                .is_err()
            {
                break;
            }
        }
    });

    let http = reqwest::Client::new();
    let terminals = RelayTerminals::new();
    let agents = RelayAgents::new();
    let proxy_ws = RelayProxyWs::new();
    let sysmon = RelaySysmon::new();
    let mut approved: HashSet<String> = HashSet::new();

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
                handle_control(&msg_type, &value, config, &out_tx, &mut approved).await?;
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
                let plaintext = match secretbox.decrypt_parts(nonce, ciphertext) {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        tracing::warn!("decrypt failed: {e}");
                        continue;
                    }
                };
                let incoming: IncomingMsg = match serde_json::from_slice(&plaintext) {
                    Ok(msg) => msg,
                    Err(e) => {
                        tracing::warn!("invalid inner message: {e}");
                        continue;
                    }
                };
                let ctx = ClientCtx::new(client_id, secretbox.clone(), out_tx.clone());
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
) -> anyhow::Result<()> {
    match msg_type {
        "ping" => {
            let _ = out_tx.send(json!({ "type": "pong" }).to_string()).await;
        }
        "pong" => {}
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
            proxy_ws.open(ctx, id, url).await;
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
