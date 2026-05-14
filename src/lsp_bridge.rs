//! LSP HTTP+WebSocket bridge — process lifecycle owned by dsterm.
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

pub struct LspSession {
    pub child: Arc<Mutex<Child>>,
    pub stdin: Arc<Mutex<ChildStdin>>,
    pub stdout: Arc<Mutex<Option<ChildStdout>>>,
    pub pid: u32,
}

pub type LspRegistry = Arc<RwLock<HashMap<String, Arc<LspSession>>>>;

#[derive(Deserialize)]
pub struct LspStartRequest {
    pub id: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
}

#[derive(Serialize)]
pub struct LspStartResponse {
    pub id: String,
    #[serde(rename = "ws_path")]
    pub ws_path: String,
}

#[derive(Deserialize, Default)]
pub struct LspKillRequest {
    pub id: Option<String>,
}

#[derive(Serialize)]
pub struct LspKillResponse {
    pub killed: Vec<String>,
}

pub async fn lsp_start(
    State(registry): State<LspRegistry>,
    Json(req): Json<LspStartRequest>,
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
                tracing::warn!(target: "lsp_stderr", program = %command_copy, pid = %pid_copy, "{}", line);
            }
        });
    }

    let session = LspSession {
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
            "ws_path": format!("/lsp/{}", req.id)
        })),
    )
}

pub async fn lsp_kill(
    State(registry): State<LspRegistry>,
    body: Option<Json<LspKillRequest>>,
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