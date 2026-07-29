use axum::extract::{
    ws::{Message, WebSocket, WebSocketUpgrade},
    Query,
};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

pub struct AiSession {
    pub id: String,
    pub created_at: u64,
    pub metadata: Value,
}

pub type AiRegistry = Arc<RwLock<Vec<AiSession>>>;

static AI_SESSIONS_CREATED: AtomicU64 = AtomicU64::new(0);

fn ts_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

async fn ai_inspect(body: Option<Json<Value>>) -> impl IntoResponse {
    let _ = body;
    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "method": "inference.inspectModel",
            "data": null,
            "message": "stub: inspectModel not yet implemented"
        })),
    )
}

async fn ai_load(body: Option<Json<Value>>) -> impl IntoResponse {
    let _ = body;
    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "method": "inference.loadModel",
            "data": { "loaded": true },
            "message": "stub: loadModel not yet implemented"
        })),
    )
}

async fn ai_unload(body: Option<Json<Value>>) -> impl IntoResponse {
    let _ = body;
    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "method": "inference.unloadModel",
            "data": { "unloaded": true },
            "message": "stub: unloadModel not yet implemented"
        })),
    )
}

async fn ai_list_models() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "method": "inference.listModels",
            "data": { "models": [] },
            "message": "stub: listModels not yet implemented"
        })),
    )
}

async fn ai_loaded_models() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "method": "inference.loadedModels",
            "data": { "models": [] },
            "message": "stub: loadedModels not yet implemented"
        })),
    )
}

async fn ai_delete(body: Option<Json<Value>>) -> impl IntoResponse {
    let _ = body;
    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "method": "inference.deleteModel",
            "data": { "deleted": true },
            "message": "stub: deleteModel not yet implemented"
        })),
    )
}

async fn ai_create_session(
    State(registry): State<AiRegistry>,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    let _ = body;
    let id = uuid::Uuid::new_v4().to_string();
    let session = AiSession {
        id: id.clone(),
        created_at: ts_secs(),
        metadata: json!({}),
    };
    registry.write().await.push(session);
    AI_SESSIONS_CREATED.fetch_add(1, Ordering::Relaxed);
    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "method": "inference.createSession",
            "data": { "session_id": id },
            "message": "stub: createSession"
        })),
    )
}

async fn ai_release_session(
    State(registry): State<AiRegistry>,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    let session_id = body
        .and_then(|b| b.0.get("session_id").cloned())
        .and_then(|v| v.as_str().map(String::from));
    if let Some(sid) = session_id {
        registry.write().await.retain(|s| s.id != sid);
    }
    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "method": "inference.releaseSession",
            "data": { "released": true },
            "message": "stub: releaseSession"
        })),
    )
}

async fn ai_cancel_session(body: Option<Json<Value>>) -> impl IntoResponse {
    let _ = body;
    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "method": "inference.cancelSession",
            "data": { "cancelled": true },
            "message": "stub: cancelSession"
        })),
    )
}

async fn ai_session_state(
    State(registry): State<AiRegistry>,
    Query(params): Query<Value>,
) -> impl IntoResponse {
    let session_id = params.get("session_id").and_then(|v| v.as_str());
    let found = registry
        .read()
        .await
        .iter()
        .find(|s| session_id.map_or(false, |sid| s.id == sid));
    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "method": "inference.sessionState",
            "data": found.map(|s| json!({
                "id": s.id,
                "created_at": s.created_at,
                "metadata": s.metadata
            })).unwrap_or(json!(null)),
            "message": if found.is_some() { "session found" } else { "session not found" }
        })),
    )
}

async fn ai_generate(body: Option<Json<Value>>) -> impl IntoResponse {
    let _ = body;
    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "method": "inference.generate",
            "data": {
                "text": "",
                "usage": { "prompt_tokens": 0, "completion_tokens": 0 }
            },
            "message": "stub: generate not yet implemented"
        })),
    )
}

async fn ai_complete(body: Option<Json<Value>>) -> impl IntoResponse {
    let _ = body;
    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "method": "inference.complete",
            "data": {
                "text": "",
                "usage": { "prompt_tokens": 0, "completion_tokens": 0 }
            },
            "message": "stub: complete not yet implemented"
        })),
    )
}

async fn ai_embed(body: Option<Json<Value>>) -> impl IntoResponse {
    let _ = body;
    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "method": "inference.embed",
            "data": { "embeddings": [] },
            "message": "stub: embed not yet implemented"
        })),
    )
}

async fn ai_tokenize(body: Option<Json<Value>>) -> impl IntoResponse {
    let _ = body;
    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "method": "inference.tokenize",
            "data": { "tokens": [] },
            "message": "stub: tokenize not yet implemented"
        })),
    )
}

async fn ai_detokenize(body: Option<Json<Value>>) -> impl IntoResponse {
    let _ = body;
    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "method": "inference.detokenize",
            "data": { "text": "" },
            "message": "stub: detokenize not yet implemented"
        })),
    )
}

async fn ai_statistics() -> impl IntoResponse {
    let sessions_total = AI_SESSIONS_CREATED.load(Ordering::Relaxed);
    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "method": "inference.statistics",
            "data": {
                "sessions_created_total": sessions_total,
                "models_loaded": 0,
                "requests_processed": 0,
                "tokens_generated": 0
            }
        })),
    )
}

async fn ai_memory() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "method": "inference.memory",
            "data": {
                "resident_models": [],
                "total_allocated_bytes": 0,
                "available_bytes": 0
            }
        })),
    )
}

async fn ai_health() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "status": "healthy",
            "service": "ai",
            "version": env!("CARGO_PKG_VERSION")
        })),
    )
}

async fn ai_capabilities() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "method": "inference.capabilities",
            "data": {
                "chat": false,
                "completion": false,
                "fim": false,
                "embeddings": false,
                "tool_calling": false,
                "streaming": true
            }
        })),
    )
}

async fn ai_diagnostics() -> impl IntoResponse {
    let sessions_total = AI_SESSIONS_CREATED.load(Ordering::Relaxed);
    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "service": "ai",
            "status": "healthy",
            "data": {
                "sessions_created_total": sessions_total,
                "active_sessions": 0,
                "loaded_models": 0,
                "backend": "none",
                "uptime_secs": ts_secs()
            }
        })),
    )
}

async fn ai_generate_stream(
    ws: WebSocketUpgrade,
    State(registry): State<AiRegistry>,
) -> impl IntoResponse {
    let _ = registry;
    ws.on_upgrade(handle_generate_stream)
}

async fn handle_generate_stream(mut socket: WebSocket) {
    let (mut sender, mut receiver) = socket.split();

    let tick = tokio::time::interval(Duration::from_millis(50));
    let mut tick = tokio::pin!(tick);

    let done = json!({
        "type": "done",
        "data": {
            "text": "",
            "usage": { "prompt_tokens": 0, "completion_tokens": 0 }
        }
    });

    loop {
        tokio::select! {
            _ = tick.as_mut().tick() => {
                let _ = sender
                    .send(Message::Text(serde_json::to_string(&done).unwrap().into()))
                    .await;
                let _ = sender.send(Message::Close(None)).await;
                break;
            }
            msg = receiver.next() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }
}

pub fn ai_routes() -> Router<AiRegistry> {
    Router::new()
        .route("/ai/inspect", post(ai_inspect))
        .route("/ai/load", post(ai_load))
        .route("/ai/unload", post(ai_unload))
        .route("/ai/models", get(ai_list_models))
        .route("/ai/models/loaded", get(ai_loaded_models))
        .route("/ai/delete", post(ai_delete))
        .route("/ai/sessions", post(ai_create_session))
        .route("/ai/sessions/release", post(ai_release_session))
        .route("/ai/sessions/cancel", post(ai_cancel_session))
        .route("/ai/sessions/state", get(ai_session_state))
        .route("/ai/generate", post(ai_generate))
        .route("/ai/complete", post(ai_complete))
        .route("/ai/embed", post(ai_embed))
        .route("/ai/tokenize", post(ai_tokenize))
        .route("/ai/detokenize", post(ai_detokenize))
        .route("/ai/statistics", get(ai_statistics))
        .route("/ai/memory", get(ai_memory))
        .route("/ai/health", get(ai_health))
        .route("/ai/capabilities", get(ai_capabilities))
        .route("/ai/diagnostics", get(ai_diagnostics))
        .route("/ai/generate-stream", get(ai_generate_stream))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;
    use tower::ServiceExt;

    #[tokio::test]
    async fn test_ai_health() {
        let app = ai_routes().with_state(Arc::new(RwLock::new(Vec::new())));
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/ai/health")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "healthy");
        assert_eq!(json["service"], "ai");
    }

    #[tokio::test]
    async fn test_ai_diagnostics() {
        let app = ai_routes().with_state(Arc::new(RwLock::new(Vec::new())));
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/ai/diagnostics")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "healthy");
        assert!(json["data"]["uptime_secs"].as_u64().is_some());
    }

    #[tokio::test]
    async fn test_ai_capabilities() {
        let app = ai_routes().with_state(Arc::new(RwLock::new(Vec::new())));
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/ai/capabilities")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["data"]["streaming"], true);
        assert_eq!(json["data"]["chat"], false);
    }

    #[tokio::test]
    async fn test_ai_create_session() {
        let registry: AiRegistry = Arc::new(RwLock::new(Vec::new()));
        let app = ai_routes().with_state(registry.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/ai/sessions")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::to_vec(&json!({})).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert!(json["data"]["session_id"].as_str().is_some());
        assert_eq!(registry.read().await.len(), 1);
    }

    #[tokio::test]
    async fn test_ai_list_models() {
        let app = ai_routes().with_state(Arc::new(RwLock::new(Vec::new())));
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/ai/models")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert!(json["data"]["models"].is_array());
    }

    #[tokio::test]
    async fn test_ai_generate_stub() {
        let app = ai_routes().with_state(Arc::new(RwLock::new(Vec::new())));
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/ai/generate")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::to_vec(&json!({
                            "prompt": "hello",
                            "session_id": "test"
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert!(json["data"]["text"].is_string());
    }
}
