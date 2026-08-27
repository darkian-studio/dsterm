//! ACP agent bridge — spawns an AI coding agent as a
//! subprocess and bridges its newline-delimited JSON (NDJSON)
//! stdio to a WebSocket. ACP permission requests (session/request_permission)
//! pass through verbatim.
use crate::process_bridge::{self, ProcessRegistry};
use axum::extract::{
    ws::{Message, WebSocket, WebSocketUpgrade},
    Path,
};
use axum::routing::{get, post};
use axum::Router;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Deserialize;
use std::collections::HashMap;

pub type AgentRegistry = ProcessRegistry;

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

    let session = match process_bridge::spawn_process(&process_bridge::SpawnConfig {
        command: req.command,
        args: req.args,
        cwd: req.cwd,
        env: req.env,
        stderr_target: "agent_stderr",
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
            "ws_path": format!("/agents/{}", req.id)
        })),
    )
}

pub async fn agent_kill(
    State(registry): State<AgentRegistry>,
    body: Option<Json<AgentKillRequest>>,
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

    ws.on_upgrade(move |socket| process_bridge::ndjson_pump(socket, id, registry, session, stdout))
}

pub fn agent_routes() -> Router<AgentRegistry> {
    Router::new()
        .route("/agents/start", post(agent_start))
        .route("/agents/kill", post(agent_kill))
        .route("/agents/{id}", get(agent_websocket))
}
