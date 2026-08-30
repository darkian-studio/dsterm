//! Extension-host HTTP+WebSocket bridge — Node.js process lifecycle owned by dsterm.
//!
//! Unlike lsp/dap/mcp bridges, the stdio protocol here is newline-delimited JSON.
//! Wraps ProcessSession with custom handle_node_line for LSP-ready tracking.
use crate::process_bridge::{self};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::RwLock;

#[derive(Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct LspEndpoint {
    pub transport: String,
    pub host: String,
    pub port: u16,
}

pub struct ExtensionHostSession {
    pub inner: Arc<process_bridge::ProcessSession>,
    pub active_language_servers: Arc<RwLock<HashMap<String, LspEndpoint>>>,
}

pub type ExtensionHostRegistry = Arc<RwLock<HashMap<String, Arc<ExtensionHostSession>>>>;

pub fn new_registry() -> ExtensionHostRegistry {
    Arc::new(RwLock::new(HashMap::new()))
}

#[derive(Deserialize)]
pub struct ExtensionHostStartRequest {
    pub id: String,
    pub node_path: String,
    pub script_path: String,
    pub extensions_dir: String,
    pub workspace_root: String,
}

#[derive(Deserialize, Default)]
pub struct ExtensionHostKillRequest {
    pub id: Option<String>,
}

async fn start_handler(
    State(registry): State<ExtensionHostRegistry>,
    Json(req): Json<ExtensionHostStartRequest>,
) -> impl IntoResponse {
    {
        let registry = registry.read().await;
        if registry.contains_key(&req.id) {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({"error": "session exists", "id": req.id})),
            );
        }
    }

    let inner = match process_bridge::spawn_process(&process_bridge::SpawnConfig {
        command: req.node_path,
        args: vec![req.script_path],
        cwd: None,
        env: Some(
            [
                ("DS_EXTENSIONS_DIR".into(), req.extensions_dir),
                ("DS_WORKSPACE_ROOT".into(), req.workspace_root),
                ("DS_SESSION_ID".into(), req.id.clone()),
            ]
            .into(),
        ),
        stderr_target: "extension_host_stderr",
    })
    .await
    {
        Ok(s) => s,
        Err((code, msg)) => {
            return (
                StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(serde_json::json!({"error": msg, "id": req.id})),
            );
        }
    };

    let session = Arc::new(ExtensionHostSession {
        inner,
        active_language_servers: Arc::new(RwLock::new(HashMap::new())),
    });

    registry.write().await.insert(req.id.clone(), session);

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "id": req.id,
            "ws_path": format!("/extension-host/{}", req.id)
        })),
    )
}

async fn kill_handler(
    State(registry): State<ExtensionHostRegistry>,
    body: Option<Json<ExtensionHostKillRequest>>,
) -> impl IntoResponse {
    let req = body.map(|Json(b)| b).unwrap_or_default();
    let kill_timeout = crate::terminal::get_config().bridges.kill_timeout_secs;

    let target_ids = if let Some(id) = &req.id {
        vec![id.clone()]
    } else {
        registry.read().await.keys().cloned().collect()
    };

    let mut killed = Vec::new();
    for id in target_ids {
        let session = registry.write().await.remove(&id);
        if let Some(session) = session {
            process_bridge::kill_session(&session.inner, kill_timeout).await;
        }
        killed.push(id);
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({ "killed": killed })),
    )
}

async fn websocket_handler(
    ws: WebSocketUpgrade,
    Path(id): Path<String>,
    State(registry): State<ExtensionHostRegistry>,
) -> impl IntoResponse {
    let session = registry.read().await.get(&id).cloned();
    if session.is_none() {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    }
    let session = session.unwrap();
    let stdout = session.inner.stdout.lock().await.take();
    if stdout.is_none() {
        return (StatusCode::CONFLICT, "stdout already claimed").into_response();
    }
    let stdout = stdout.unwrap();

    ws.on_upgrade(move |socket| async move {
        extension_host_pump(socket, id, registry, session, stdout).await;
    })
}

async fn extension_host_pump(
    socket: WebSocket,
    id: String,
    registry: ExtensionHostRegistry,
    session: Arc<ExtensionHostSession>,
    stdout: tokio::process::ChildStdout,
) {
    let (mut ws_send, mut ws_recv) = socket.split();
    let mut lines = tokio::io::BufReader::new(stdout).lines();

    loop {
        tokio::select! {
            msg = ws_recv.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        let mut stdin = session.inner.stdin.lock().await;
                        let _ = stdin.write_all(text.as_bytes()).await;
                        let _ = stdin.write_all(b"\n").await;
                        let _ = stdin.flush().await;
                    }
                    Some(Ok(Message::Binary(b))) => {
                        if let Ok(text) = std::str::from_utf8(&b) {
                            let mut stdin = session.inner.stdin.lock().await;
                            let _ = stdin.write_all(text.as_bytes()).await;
                            let _ = stdin.write_all(b"\n").await;
                            let _ = stdin.flush().await;
                        }
                    }
                    Some(Ok(Message::Ping(p))) => {
                        let _ = ws_send.send(Message::Pong(p)).await;
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => {
                        break;
                    }
                }
            }
            line = lines.next_line() => {
                match line {
                    Ok(Some(line)) => {
                        handle_node_line(&session, &line).await;
                        let _ = ws_send.send(Message::Text(line.into())).await;
                    }
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
        }
    }

    registry.write().await.remove(&id);
    if let Ok(mut child) = session.inner.child.try_lock() {
        let _ = child.start_kill();
    }
    let _ = ws_send.send(Message::Close(None)).await;
}

async fn handle_node_line(session: &ExtensionHostSession, line: &str) {
    let value = match serde_json::from_str::<serde_json::Value>(line) {
        Ok(v) => v,
        Err(_) => return,
    };

    if value.get("type").and_then(|t| t.as_str()) != Some("lsp_ready") {
        return;
    }

    let language = match value.get("language").and_then(|l| l.as_str()) {
        Some(l) => l.to_string(),
        None => return,
    };

    let transport = value
        .get("transport")
        .and_then(|t| t.as_str())
        .unwrap_or("tcp")
        .to_string();
    let host = value
        .get("host")
        .and_then(|h| h.as_str())
        .unwrap_or("127.0.0.1")
        .to_string();
    let port = match value.get("port").and_then(|p| p.as_u64()) {
        Some(p) => p as u16,
        None => return,
    };

    session.active_language_servers.write().await.insert(
        language,
        LspEndpoint {
            transport,
            host,
            port,
        },
    );
}

pub fn extension_host_routes() -> Router<ExtensionHostRegistry> {
    Router::new()
        .route("/extension-host/start", post(start_handler))
        .route("/extension-host/kill", post(kill_handler))
        .route("/extension-host/{id}", get(websocket_handler))
}
