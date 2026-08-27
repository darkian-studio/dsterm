//! Shared process-lifecycle bridge used by LSP, DAP, MCP, extension-host, and
//! agent bridges. Eliminates the copy-paste across those modules.
//!
//! Each bridge keeps its own thin module for route registration and any
//! bridge-specific start-request fields, but delegates spawning, killing,
//! and WebSocket pumping to the helpers here.

use crate::proto_frame::{encode_frame, FrameDecoder};
use axum::extract::ws::{Message, WebSocket};
use futures::{SinkExt, StreamExt};
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
// Spawn
// ---------------------------------------------------------------------------

pub struct SpawnConfig {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: Option<HashMap<String, String>>,
    /// Target label for stderr log lines (e.g. "lsp_stderr", "dap_stderr").
    pub stderr_target: &'static str,
}

/// Spawn a child process, drain its stderr in the background, and return a
/// [`ProcessSession`]. Returns `Err(message)` on failure.
pub async fn spawn_process(config: &SpawnConfig) -> Result<Arc<ProcessSession>, (u16, String)> {
    let mut command = Command::new(&config.command);
    command
        .args(&config.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

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
                tracing::warn!(target: target, program = %command_str, pid = %pid, "{}", line);
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
// Kill
// ---------------------------------------------------------------------------

/// Kill a single session, waiting up to `timeout_secs` for a clean exit.
pub async fn kill_session(session: &ProcessSession, timeout_secs: u64) {
    if let Ok(mut child) = session.child.try_lock() {
        if child.start_kill().is_ok() {
            match timeout(Duration::from_secs(timeout_secs), child.wait()).await {
                Ok(Ok(_)) => {}
                _ => {
                    let _ = child.kill().await;
                }
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
// WebSocket pumps
// ---------------------------------------------------------------------------

/// Bidirectional relay using **Content-Length framing** (LSP / DAP / MCP).
pub async fn content_length_pump(
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

/// Bidirectional relay using **newline-delimited JSON** (extension-host / agent).
pub async fn ndjson_pump(
    socket: WebSocket,
    id: String,
    registry: ProcessRegistry,
    session: Arc<ProcessSession>,
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
