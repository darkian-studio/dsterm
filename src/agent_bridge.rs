//! ACP agent bridge — spawns an AI coding agent as a subprocess and bridges its
//! newline-delimited JSON (NDJSON) stdio to a WebSocket. Mirrors the
//! extension-host bridge; Content-Length framing is intentionally NOT used.
//! ACP permission requests (session/request_permission) pass through verbatim.
use axum::extract::{
    ws::{Message, WebSocket, WebSocketUpgrade},
    Path,
};
use axum::routing::{get, post};
use axum::Router;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
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

#[allow(dead_code)]
pub struct AgentSession {
    pub child: Arc<Mutex<Child>>,
    pub stdin: Arc<Mutex<ChildStdin>>,
    pub stdout: Arc<Mutex<Option<ChildStdout>>>,
    pub pid: u32,
}

pub type AgentRegistry = Arc<RwLock<HashMap<String, Arc<AgentSession>>>>;

#[derive(Deserialize)]
pub struct AgentStartRequest {
    pub id: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: Option<HashMap<String, String>>,
}

#[derive(Deserialize, Default)]
pub struct AgentKillRequest {
    pub id: Option<String>,
}

pub async fn agent_start(
    State(registry): State<AgentRegistry>,
    Json(req): Json<AgentStartRequest>,
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
    if let Some(env) = &req.env {
        for (key, value) in env {
            command.env(key, value);
        }
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
                tracing::warn!(target: "agent_stderr", program = %command_copy, pid = %pid_copy, "{}", line);
            }
        });
    }

    let session = AgentSession {
        child: Arc::new(Mutex::new(child)),
        stdin: Arc::new(Mutex::new(stdin)),
        stdout: Arc::new(Mutex::new(Some(stdout))),
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
            "ws_path": format!("/agents/{}", req.id)
        })),
    )
}

pub async fn agent_kill(
    State(registry): State<AgentRegistry>,
    body: Option<Json<AgentKillRequest>>,
) -> impl IntoResponse {
    let req = body.map(|Json(b)| b).unwrap_or_default();

    let target_ids = if let Some(id) = &req.id {
        vec![id.clone()]
    } else {
        registry.read().await.keys().cloned().collect()
    };

    let kill_timeout = crate::terminal::get_config().bridges.kill_timeout_secs;
    let mut killed = Vec::new();

    for id in target_ids {
        let session = registry.write().await.remove(&id);
        if let Some(session) = session {
            let mut child = session.child.lock().await;
            if child.start_kill().is_ok() {
                match timeout(Duration::from_secs(kill_timeout), child.wait()).await {
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

pub async fn agent_websocket(
    ws: WebSocketUpgrade,
    Path(id): Path<String>,
    State(registry): State<AgentRegistry>,
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

    ws.on_upgrade(move |socket| agent_pump(socket, id, registry, session, stdout))
}

async fn agent_pump(
    socket: WebSocket,
    id: String,
    registry: AgentRegistry,
    session: Arc<AgentSession>,
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

pub fn agent_routes() -> Router<AgentRegistry> {
    Router::new()
        .route("/agents/start", post(agent_start))
        .route("/agents/kill", post(agent_kill))
        .route("/agents/{id}", get(agent_websocket))
}
