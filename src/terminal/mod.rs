mod handlers;
#[cfg(unix)]
mod pty_fallback;
mod scrollback;
#[cfg(not(windows))]
mod shell_integration;
mod types;

use axum::{
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
    agent_bridge, ai_bridge, ast_bridge, config::DstermConfig, dap_bridge, extension_host_bridge,
    fs, lsp_bridge, mcp_bridge, ports, process_bridge, proxy, sysmon,
};
use handlers::*;
use types::Sessions;

static DEFAULT_COMMAND: OnceLock<String> = OnceLock::new();
static CONFIG: OnceLock<DstermConfig> = OnceLock::new();

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

pub async fn start_server(host: Ipv4Addr, port: u16, allow_any_origin: bool) {
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
    let ast_registry = ast_bridge::new_registry();
    let ai_state = ai_bridge::AiState::new();
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
                // FIX-044: avoid silent skip on lock contention and future timestamps
                let to_evict: Vec<u32> = sessions
                    .iter()
                    .filter_map(|entry| {
                        let pid = *entry.key();
                        let last = match entry.value().last_accessed.try_lock() {
                            Ok(g) => *g,
                            Err(_) => {
                                tracing::debug!(pid = %pid, "eviction: last_accessed lock contended, skipping");
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
        // FIX-024: allow any localhost origin (http/https, any port) rather than single https://localhost
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

    let terminal_router = Router::new()
        .route("/", get(|| async { "Rust based DSTerm server" }))
        .route("/terminals", post(create_terminal).get(list_terminals))
        .route("/terminals/{pid}/resize", post(resize_terminal))
        .route("/terminals/{pid}", get(terminal_websocket))
        .route("/terminals/{pid}/terminate", post(terminate_terminal))
        .route("/execute-command", post(execute_command))
        .route("/silent-exec", post(silent_exec))
        .route("/silent-exec-stream", get(silent_exec_stream))
        .route(
            "/status",
            get(|| async {
                axum::Json(serde_json::json!({
                    "status": "OK",
                    "home": get_config().home.clone(),
                }))
            }),
        )
        .route("/metrics", get(get_metrics))
        .with_state(sessions);

    let app = Router::new()
        .merge(terminal_router)
        .merge(lsp_bridge::lsp_routes().with_state(lsp_registry.clone()))
        .merge(dap_bridge::dap_routes().with_state(dap_registry.clone()))
        .merge(
            extension_host_bridge::extension_host_routes()
                .with_state(extension_host_registry.clone()),
        )
        .merge(mcp_bridge::mcp_routes().with_state(mcp_registry.clone()))
        .merge(ast_bridge::ast_routes().with_state(ast_registry))
        .merge(agent_bridge::agent_routes().with_state(agent_registry.clone()))
        .merge(ai_bridge::ai_routes().with_state(ai_state))
        .merge(fs::fs_routes())
        .merge(sysmon::sysmon_routes())
        .merge(ports::ports_routes())
        .merge(proxy::proxy_routes())
        .merge(crate::web_routes::web_routes().with_state(web_provider))
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

            if let Err(e) = axum::serve(listener, app)
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
