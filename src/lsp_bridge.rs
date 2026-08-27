//! LSP HTTP+WebSocket bridge — process lifecycle owned by dsterm.
use crate::process_bridge::{self, ProcessRegistry};
use axum::extract::{
    ws::{Message, WebSocket, WebSocketUpgrade},
    Path,
};
use axum::routing::{get, post};
use axum::Router;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::{Deserialize, Serialize};

pub type LspRegistry = ProcessRegistry;

#[derive(Deserialize)]
pub struct LspStartRequest {
    pub id: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
}

#[derive(Serialize)]
#[allow(dead_code)]
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
#[allow(dead_code)]
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

    let kill_timeout = crate::terminal::get_config().bridges.kill_timeout_secs;
    let session = match process_bridge::spawn_process(&process_bridge::SpawnConfig {
        command: req.command,
        args: req.args,
        cwd: req.cwd,
        env: None,
        stderr_target: "lsp_stderr",
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

    registry.write().await.insert(req.id.clone(), session);

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
    let kill_timeout = crate::terminal::get_config().bridges.kill_timeout_secs;

    let killed = if let Some(id) = &req.id {
        if process_bridge::kill_one(&registry, id, kill_timeout).await {
            vec![id.clone()]
        } else {
            vec![]
        }
    } else {
        process_bridge::kill_all(&registry, kill_timeout).await
    };

    (
        StatusCode::OK,
        Json(serde_json::json!({ "killed": killed })),
    )
}

pub async fn lsp_websocket(
    ws: WebSocketUpgrade,
    Path(id): Path<String>,
    State(registry): State<LspRegistry>,
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

    ws.on_upgrade(move |socket| {
        process_bridge::content_length_pump(socket, id, registry, session, stdout)
    })
}

pub fn lsp_routes() -> Router<LspRegistry> {
    Router::new()
        .route("/lsp/start", post(lsp_start))
        .route("/lsp/kill", post(lsp_kill))
        .route("/lsp/{id}", get(lsp_websocket))
}
