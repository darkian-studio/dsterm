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

use crate::{dap_bridge, lsp_bridge, mcp_bridge};
use handlers::*;
use std::collections::HashMap;
use tokio::sync::RwLock;
use types::Sessions;

static DEFAULT_COMMAND: OnceLock<String> = OnceLock::new();

pub fn set_default_command(cmd: String) {
    let _ = DEFAULT_COMMAND.set(cmd);
}

pub fn get_default_command() -> Option<&'static str> {
    DEFAULT_COMMAND.get().map(|s| s.as_str())
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
    let mcp_registry: mcp_bridge::McpRegistry = Arc::new(RwLock::new(HashMap::new()));

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
        .route("/terminals", post(create_terminal))
        .route("/terminals/{pid}/resize", post(resize_terminal))
        .route("/terminals/{pid}", get(terminal_websocket))
        .route("/terminals/{pid}/terminate", post(terminate_terminal))
        .route("/execute-command", post(execute_command))
        .route("/silent-exec", post(silent_exec))
        .route("/silent-exec-stream", get(silent_exec_stream))
        .route("/status", get(|| async { "OK" }))
        .with_state(sessions);

    let app = Router::new()
        .merge(terminal_router)
        .merge(lsp_bridge::lsp_routes().with_state(lsp_registry))
        .merge(dap_bridge::dap_routes().with_state(dap_registry))
        .merge(mcp_bridge::mcp_routes().with_state(mcp_registry))
        .layer(cors)
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::default().include_headers(true)),
        );

    let addr: std::net::SocketAddr = (host, port).into();

    match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => {
            tracing::info!("listening on {}", listener.local_addr().unwrap());

            if let Err(e) = axum::serve(listener, app).await {
                tracing::error!("Server error: {}", e);
            }
        }
        Err(e) => {
            if e.kind() == ErrorKind::AddrInUse {
                tracing::error!("Port is already in use please kill all other instances of dsterm server or stop any other process or app that maybe be using port {}", port);
            } else {
                tracing::error!("Failed to bind: {}", e);
            }
        }
    }
}
