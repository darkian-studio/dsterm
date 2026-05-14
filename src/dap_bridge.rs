//! DAP HTTP+WebSocket bridge — process lifecycle owned by dsterm.
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncBufReadExt;
use tokio::sync::{Mutex, RwLock};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use serde::{Deserialize, Serialize};
use axum::{extract::State, response::IntoResponse, http::StatusCode, Json};
use tokio::task;
use tokio::time::timeout;
use axum::extract::{ws::{Message, WebSocket, WebSocketUpgrade}, Path};
use axum::routing::{get, post};
use axum::Router;
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use crate::proto_frame::{FrameDecoder, encode_frame};

#[allow(dead_code)]
pub struct DapSession {
    pub child: Arc<Mutex<Child>>,
    pub stdin: Arc<Mutex<ChildStdin>>,
    pub stdout: Arc<Mutex<Option<ChildStdout>>>,
    pub pid: u32,
}

pub type DapRegistry = Arc<RwLock<HashMap<String, Arc<DapSession>>>>;

#[derive(Deserialize)]
pub struct DapStartRequest {
    pub id: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
}

#[derive(Serialize)]
#[allow(dead_code)]
pub struct DapStartResponse {
    pub id: String,
    #[serde(rename = "ws_path")]
    pub ws_path: String,
}

#[derive(Deserialize, Default)]
pub struct DapKillRequest {
    pub id: Option<String>,
}

#[derive(Serialize)]
#[allow(dead_code)]
pub struct DapKillResponse {
    pub killed: Vec<String>,
}

pub async fn dap_start(
    State(registry): State<DapRegistry>,
    Json(req): Json<DapStartRequest>,
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

    let mut command = Command::new(&req.command);
    command
        .args(&req.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    if let Some(cwd) = &req.cwd {
        command.current_dir(cwd);
    }

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
    let command_str = req.command.clone();

    if let Some(stderr) = stderr {
        let pid_copy = pid;
        let command_copy = command_str;
        task::spawn(async move {
            let reader = tokio::io::BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::warn!(target: "dap_stderr", program = %command_copy, pid = %pid_copy, "{}", line);
            }
        });
    }

    let session = DapSession {
        child: Arc::new(Mutex::new(child)),
        stdin: Arc::new(Mutex::new(stdin)),
        stdout: Arc::new(Mutex::new(Some(stdout))),
        pid,
    };

    registry.write().await.insert(req.id.clone(), Arc::new(session));

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "id": req.id,
            "ws_path": format!("/dap/{}", req.id)
        })),
    )
}

pub async fn dap_kill(
    State(registry): State<DapRegistry>,
    body: Option<Json<DapKillRequest>>,
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

pub async fn dap_websocket(
    ws: WebSocketUpgrade,
    Path(id): Path<String>,
    State(registry): State<DapRegistry>,
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

    ws.on_upgrade(move |socket| dap_pump(socket, id, registry, session, stdout))
}

async fn dap_pump(
    socket: WebSocket,
    id: String,
    registry: DapRegistry,
    session: Arc<DapSession>,
    mut stdout: ChildStdout,
) {
    let (mut ws_send, mut ws_recv) = socket.split();
    let mut decoder = FrameDecoder::new();
    let mut buf = [0u8; 8192];

    loop {
        tokio::select! {
            msg = ws_recv.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        let frame = encode_frame(&text);
                        let mut stdin = session.stdin.lock().await;
                        let _ = stdin.write_all(&frame).await;
                        let _ = stdin.flush().await;
                    }
                    Some(Ok(Message::Binary(b))) => {
                        if let Ok(text) = std::str::from_utf8(&b) {
                            let frame = encode_frame(text);
                            let mut stdin = session.stdin.lock().await;
                            let _ = stdin.write_all(&frame).await;
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
            result = stdout.read(&mut buf) => {
                match result {
                    Ok(0) => break,
                    Ok(n) => {
                        if let Ok(payloads) = decoder.feed(&buf[..n]) {
                            for payload in payloads {
                                let _ = ws_send.send(Message::Text(payload.into())).await;
                            }
                        }
                    }
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

pub fn dap_routes() -> Router<DapRegistry> {
    Router::new()
        .route("/dap/start", post(dap_start))
        .route("/dap/kill", post(dap_kill))
        .route("/dap/{id}", get(dap_websocket))
}