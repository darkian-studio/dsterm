//! Shared process-lifecycle bridge used by LSP, DAP, MCP, extension-host, and
//! agent bridges. Each bridge passes a prefix string; this module provides the
//! fully-wired axum Router so the per-bridge files are ~5 lines.

use crate::proto_frame::{encode_frame, FrameDecoder};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex, RwLock};
use tokio::task;
use tokio::time::timeout;

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub struct ProcessSession {
    pub child: Arc<Mutex<Child>>,
    pub stdin: Arc<Mutex<ChildStdin>>,
    pub stdout: Arc<Mutex<Option<ChildStdout>>>,
    pub pid: u32,
}

pub type ProcessRegistry = Arc<RwLock<HashMap<String, Arc<ProcessSession>>>>;

pub fn new_registry() -> ProcessRegistry {
    Arc::new(RwLock::new(HashMap::new()))
}

// ---------------------------------------------------------------------------
// Bridge configuration
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FramingMode {
    /// Content-Length header framing (LSP, DAP, MCP).
    ContentLength,
    /// Newline-delimited JSON (extension-host, agent).
    Ndjson,
}

pub struct BridgeConfig {
    pub prefix: &'static str,
    pub stderr_target: &'static str,
    pub framing: FramingMode,
}

// ---------------------------------------------------------------------------
// Request / response types (shared across all bridges)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct StartRequest {
    pub id: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: Option<HashMap<String, String>>,
}

#[derive(Deserialize, Default)]
pub struct KillRequest {
    pub id: Option<String>,
    #[serde(default)]
    pub all: Option<bool>,
}

// ---------------------------------------------------------------------------
// Spawn
// ---------------------------------------------------------------------------

pub struct SpawnConfig {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: Option<HashMap<String, String>>,
    pub stderr_target: &'static str,
    /// Protocol integrity: children whose stdout is a strict ndjson stream
    /// (one JSON object per line, e.g. extension-host) must not inherit
    /// ambient env that can make them emit non-JSON to that stream.
    pub isolate_env: bool,
}

pub async fn spawn_process(config: &SpawnConfig) -> Result<Arc<ProcessSession>, (u16, String)> {
    let mut command = Command::new(&config.command);
    command
        .args(&config.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    if config.isolate_env {
        command.env_clear();
        for key in ["PATH", "HOME", "LANG"] {
            if let Ok(val) = std::env::var(key) {
                command.env(key, val);
            }
        }
    }

    if let Some(env) = &config.env {
        command.envs(env);
    }
    if let Some(cwd) = &config.cwd {
        command.current_dir(cwd);
    }

    let mut child = command
        .spawn()
        .map_err(|e| (500u16, format!("spawn failed: {e}")))?;

    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take();
    let pid = child.id().unwrap_or(0);
    let command_str = config.command.clone();
    let target = config.stderr_target;

    if let Some(stderr) = stderr {
        task::spawn(async move {
            let reader = tokio::io::BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::warn!(program = %command_str, pid = %pid, stderr_target = target, "{}", line);
            }
        });
    }

    Ok(Arc::new(ProcessSession {
        child: Arc::new(Mutex::new(child)),
        stdin: Arc::new(Mutex::new(stdin)),
        stdout: Arc::new(Mutex::new(Some(stdout))),
        pid,
    }))
}

// ---------------------------------------------------------------------------
// Kill helpers
// ---------------------------------------------------------------------------

/// Kill a single session, waiting up to `timeout_secs` for a clean exit.
pub async fn kill_session(session: &ProcessSession, timeout_secs: u64) {
    let mut child = match session.child.try_lock() {
        Ok(g) => g,
        Err(_) => {
            tracing::warn!(pid = %session.pid, "kill_session: child lock contended, waiting");
            // Fall back to blocking lock to guarantee kill (avoid silent no-op)
            session.child.lock().await
        }
    };
    if child.start_kill().is_ok() {
        match timeout(Duration::from_secs(timeout_secs), child.wait()).await {
            Ok(Ok(_)) => {}
            _ => {
                let _ = child.kill().await;
            }
        }
    }
}

/// Remove and kill every session in the registry. Returns the IDs killed.
pub async fn kill_all(registry: &ProcessRegistry, timeout_secs: u64) -> Vec<String> {
    let ids: Vec<String> = registry.read().await.keys().cloned().collect();
    let mut killed = Vec::with_capacity(ids.len());
    for id in ids {
        let session = registry.write().await.remove(&id);
        if let Some(session) = session {
            kill_session(&session, timeout_secs).await;
        }
        killed.push(id);
    }
    killed
}

/// Remove and kill a single session by ID. Returns `true` if found.
pub async fn kill_one(registry: &ProcessRegistry, id: &str, timeout_secs: u64) -> bool {
    let session = registry.write().await.remove(id);
    if let Some(session) = session {
        kill_session(&session, timeout_secs).await;
        true
    } else {
        false
    }
}

// ---------------------------------------------------------------------------
// Axum handlers (generic over bridge prefix)
// ---------------------------------------------------------------------------

async fn start_handler(
    State(registry): State<ProcessRegistry>,
    Extension(config): Extension<Arc<BridgeConfig>>,
    Json(req): Json<StartRequest>,
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

    let session = match spawn_process(&SpawnConfig {
        command: req.command.clone(),
        args: req.args.clone(),
        cwd: req.cwd.clone(),
        env: req.env.clone(),
        stderr_target: config.stderr_target,
        // Protocol integrity: generic bridges (LSP/DAP) need broad env
        // (CARGO_HOME, GOPATH, venv) — do not isolate.
        isolate_env: false,
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

    // Re-check under write lock to close race where two concurrent starts passed the read check
    {
        let mut registry = registry.write().await;
        if registry.contains_key(&req.id) {
            // Another task raced us — clean up the process we just spawned
            let kill_timeout = crate::terminal::get_config().bridges.kill_timeout_secs;
            kill_session(&session, kill_timeout).await;
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({"error": "session exists", "id": req.id})),
            );
        }
        registry.insert(req.id.clone(), session);
    }
    let ws_path = format!("/{}/{}", config.prefix, urlencoding::encode(&req.id));

    (
        StatusCode::OK,
        Json(serde_json::json!({ "id": req.id, "ws_path": ws_path })),
    )
}
async fn kill_handler(
    State(registry): State<ProcessRegistry>,
    Extension(_config): Extension<Arc<BridgeConfig>>,
    body: Option<Json<KillRequest>>,
) -> impl IntoResponse {
    let req = body.map(|Json(b)| b).unwrap_or_default();
    let kill_timeout = crate::terminal::get_config().bridges.kill_timeout_secs;

    if req.id.is_none() && req.all != Some(true) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "must provide id or all:true"})),
        )
            .into_response();
    }
    let killed = if let Some(id) = &req.id {
        if kill_one(&registry, id, kill_timeout).await {
            vec![id.clone()]
        } else {
            vec![]
        }
    } else {
        kill_all(&registry, kill_timeout).await
    };

    (
        StatusCode::OK,
        Json(serde_json::json!({ "killed": killed })),
    )
        .into_response()
}

async fn websocket_handler(
    ws: WebSocketUpgrade,
    Path(id): Path<String>,
    State(registry): State<ProcessRegistry>,
    Extension(config): Extension<Arc<BridgeConfig>>,
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
    let framing = config.framing;

    ws.on_upgrade(move |socket| async move {
        match framing {
            FramingMode::ContentLength => {
                content_length_pump(socket, id, registry, session, stdout).await;
            }
            FramingMode::Ndjson => {
                ndjson_pump(socket, id, registry, session, stdout).await;
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Public: wire everything into an axum Router
// ---------------------------------------------------------------------------

/// Returns a fully-wired `Router<ProcessRegistry>` for the given bridge prefix.
///
/// ```ignore
/// // In lsp_bridge.rs:
/// pub fn lsp_routes() -> Router<LspRegistry> {
///     process_bridge::routes("lsp")
/// }
/// ```
pub fn routes(prefix: &'static str) -> Router<ProcessRegistry> {
    let stderr_target: &'static str = Box::leak(format!("{prefix}_stderr").into_boxed_str());

    let framing = match prefix {
        "extension-host" | "agents" => FramingMode::Ndjson,
        _ => FramingMode::ContentLength,
    };

    let config = Arc::new(BridgeConfig {
        prefix,
        stderr_target,
        framing,
    });

    Router::new()
        .route(&format!("/{prefix}/start"), post(start_handler))
        .route(&format!("/{prefix}/kill"), post(kill_handler))
        .route(&format!("/{prefix}/{{id}}"), get(websocket_handler))
        .layer(Extension(config))
}

// ---------------------------------------------------------------------------
// WebSocket pumps (kept here — used by the generic websocket_handler)
// ---------------------------------------------------------------------------

async fn content_length_pump(
    socket: WebSocket,
    id: String,
    registry: ProcessRegistry,
    session: Arc<ProcessSession>,
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

async fn ndjson_pump(
    socket: WebSocket,
    id: String,
    registry: ProcessRegistry,
    session: Arc<ProcessSession>,
    stdout: ChildStdout,
) {
    ndjson_pump_with_hook(socket, id, registry, session, stdout, |_| async {}).await
}

pub async fn ndjson_pump_with_hook<F, Fut>(
    socket: WebSocket,
    id: String,
    registry: ProcessRegistry,
    session: Arc<ProcessSession>,
    stdout: ChildStdout,
    hook: F,
) where
    F: Fn(String) -> Fut + Send + Sync,
    Fut: std::future::Future<Output = ()> + Send,
{
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
                        hook(line.clone()).await;
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
