//! Extension-host HTTP+WebSocket bridge — Node.js process lifecycle owned by dsterm.
//!
//! Unlike lsp_bridge/dap_bridge, the stdio protocol here is newline-delimited JSON:
//! exactly one complete JSON object per `\n`-terminated line. Content-Length framing
//! (proto_frame) is intentionally NOT used here.
use axum::extract::{
    ws::{Message, WebSocket, WebSocketUpgrade},
    Path,
};
use axum::routing::{get, post};
use axum::Router;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex, RwLock};
use tokio::task;
use tokio::time::timeout;

#[derive(Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct LspEndpoint {
    pub transport: String,
    pub host: String,
    pub port: u16,
}

#[allow(dead_code)]
pub struct ExtensionHostSession {
    pub child: Arc<Mutex<Child>>,
    pub stdin: Arc<Mutex<ChildStdin>>,
    pub stdout: Arc<Mutex<Option<ChildStdout>>>,
    pub active_language_servers: Arc<RwLock<HashMap<String, LspEndpoint>>>,
    pub pid: u32,
}

pub type ExtensionHostRegistry = Arc<RwLock<HashMap<String, Arc<ExtensionHostSession>>>>;

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

pub async fn extension_host_start(
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

    let mut command = Command::new(&req.node_path);
    command
        .arg(&req.script_path)
        .env("DS_EXTENSIONS_DIR", &req.extensions_dir)
        .env("DS_WORKSPACE_ROOT", &req.workspace_root)
        .env("DS_SESSION_ID", &req.id)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("spawn failed: {e}"), "id": req.id})),
            );
        }
    };

    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take();
    let pid = child.id().unwrap_or(0);
    let script_str = req.script_path.clone();

    if let Some(stderr) = stderr {
        let pid_copy = pid;
        let script_copy = script_str;
        task::spawn(async move {
            let reader = tokio::io::BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::warn!(target: "extension_host_stderr", program = %script_copy, pid = %pid_copy, "{}", line);
            }
        });
    }

    let session = ExtensionHostSession {
        child: Arc::new(Mutex::new(child)),
        stdin: Arc::new(Mutex::new(stdin)),
        stdout: Arc::new(Mutex::new(Some(stdout))),
        active_language_servers: Arc::new(RwLock::new(HashMap::new())),
        pid,
    };

    registry
        .write()
        .await
        .insert(req.id.clone(), Arc::new(session));

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "id": req.id,
            "ws_path": format!("/extension-host/{}", req.id)
        })),
    )
}

pub async fn extension_host_kill(
    State(registry): State<ExtensionHostRegistry>,
    body: Option<Json<ExtensionHostKillRequest>>,
) -> impl IntoResponse {
    let req = body.map(|Json(b)| b).unwrap_or_default();

    let target_ids = if let Some(id) = &req.id {
        vec![id.clone()]
    } else {
        registry.read().await.keys().cloned().collect()
    };

    let mut killed = Vec::new();

    for id in target_ids {
        let session = registry.write().await.remove(&id);
        if let Some(session) = session {
            let mut child = session.child.lock().await;
            if child.start_kill().is_ok() {
                match timeout(Duration::from_secs(2), child.wait()).await {
                    Ok(Ok(_)) => {}
                    Ok(Err(_)) => {
                        let _ = child.kill().await;
                    }
                    Err(_) => {
                        let _ = child.kill().await;
                    }
                }
            }
            drop(child);
        }
        killed.push(id);
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({ "killed": killed })),
    )
}

pub async fn extension_host_websocket(
    ws: WebSocketUpgrade,
    Path(id): Path<String>,
    State(registry): State<ExtensionHostRegistry>,
) -> impl IntoResponse {
    let session = registry.read().await.get(&id).cloned();
    if session.is_none() {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    }
    let session = session.unwrap();
    let stdout = session.stdout.lock().await.take();
    if stdout.is_none() {
        return (StatusCode::CONFLICT, "stdout already claimed").into_response();
    }
    let stdout = stdout.unwrap();

    ws.on_upgrade(move |socket| extension_host_pump(socket, id, registry, session, stdout))
}

async fn extension_host_pump(
    socket: WebSocket,
    id: String,
    registry: ExtensionHostRegistry,
    session: Arc<ExtensionHostSession>,
    stdout: ChildStdout,
) {
    let (mut ws_send, mut ws_recv) = socket.split();
    let mut lines = tokio::io::BufReader::new(stdout).lines();

    loop {
        tokio::select! {
            msg = ws_recv.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        let mut stdin = session.stdin.lock().await;
                        let _ = stdin.write_all(text.as_bytes()).await;
                        let _ = stdin.write_all(b"\n").await;
                        let _ = stdin.flush().await;
                    }
                    Some(Ok(Message::Binary(b))) => {
                        if let Ok(text) = std::str::from_utf8(&b) {
                            let mut stdin = session.stdin.lock().await;
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
    if let Ok(mut child) = session.child.try_lock() {
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
        .route("/extension-host/start", post(extension_host_start))
        .route("/extension-host/kill", post(extension_host_kill))
        .route("/extension-host/{id}", get(extension_host_websocket))
}
