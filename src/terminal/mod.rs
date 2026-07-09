mod handlers;
// Not gated with #[cfg(target_os)] — this crate exclusively targets
// Linux/Android and will never be built for other platforms.
mod pty_fallback;
mod scrollback;
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
    ast_bridge, config::DstermConfig, dap_bridge, extension_host_bridge, fs, lsp_bridge,
    mcp_bridge, ports, sysmon,
};
use handlers::*;
use std::collections::HashMap;
use tokio::sync::RwLock;
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
pub fn init_config(config: DstermConfig) {
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
    let lsp_registry: lsp_bridge::LspRegistry = Arc::new(RwLock::new(HashMap::new()));
    let dap_registry: dap_bridge::DapRegistry = Arc::new(RwLock::new(HashMap::new()));
    let extension_host_registry: extension_host_bridge::ExtensionHostRegistry =
        Arc::new(RwLock::new(HashMap::new()));
    let mcp_registry: mcp_bridge::McpRegistry = Arc::new(RwLock::new(HashMap::new()));
    let ast_registry = ast_bridge::new_registry();

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
                        let last = *entry.value().last_accessed.try_lock().ok()?;
                        let elapsed = now.duration_since(last).unwrap_or_default();
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
        let localhost = "https://localhost"
            .parse::<HeaderValue>()
            .expect("valid origin");
        CorsLayer::new()
            .allow_origin(localhost)
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
        .route("/status", get(|| async { "OK" }))
        .route("/metrics", get(get_metrics))
        .with_state(sessions);

    let app = Router::new()
        .merge(terminal_router)
        .merge(lsp_bridge::lsp_routes().with_state(lsp_registry))
        .merge(dap_bridge::dap_routes().with_state(dap_registry))
        .merge(extension_host_bridge::extension_host_routes().with_state(extension_host_registry))
        .merge(mcp_bridge::mcp_routes().with_state(mcp_registry))
        .merge(ast_bridge::ast_routes().with_state(ast_registry))
        .merge(fs::fs_routes())
        .merge(sysmon::sysmon_routes())
        .merge(ports::ports_routes())
        .layer(cors)
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::default().include_headers(true)),
        );

    let addr: std::net::SocketAddr = (host, port).into();

    match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => {
            tracing::info!("listening on {}", listener.local_addr().unwrap());

            if let Err(e) = axum::serve(listener, app)
                .with_graceful_shutdown(shutdown_signal())
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
