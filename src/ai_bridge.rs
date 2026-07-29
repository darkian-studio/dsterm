use axum::extract::{
    ws::{Message, WebSocket, WebSocketUpgrade},
    Path, Query, State,
};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

use crate::ai::error::{self, AiError};
use crate::ai::gguf;
use crate::ai::inspect;

fn ok_response(method: &str, data: Value) -> (StatusCode, Json<Value>) {
    (
        StatusCode::OK,
        Json(json!({ "success": true, "method": method, "data": data, "message": "ok" })),
    )
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct ModelRegistration {
    pub id: String,
    pub provider: String,
    pub repository: String,
    pub filename: String,
    pub revision: String,
    pub local_path: String,
    pub size: u64,
    pub sha256: String,
    pub quantisation: String,
    pub parameter_count: String,
    pub download_url: String,
    pub installed_at: String,
    pub status: ModelStatus,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ModelStatus {
    Downloading,
    Downloaded,
    Verified,
    Registered,
    Loaded,
    Failed,
    Corrupted,
}

impl std::fmt::Display for ModelStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelStatus::Downloading => write!(f, "downloading"),
            ModelStatus::Downloaded => write!(f, "downloaded"),
            ModelStatus::Verified => write!(f, "verified"),
            ModelStatus::Registered => write!(f, "registered"),
            ModelStatus::Loaded => write!(f, "loaded"),
            ModelStatus::Failed => write!(f, "failed"),
            ModelStatus::Corrupted => write!(f, "corrupted"),
        }
    }
}

pub struct ModelRegistryInner {
    pub models: Vec<ModelRegistration>,
    storage_path: PathBuf,
}

impl ModelRegistryInner {
    fn storage_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        PathBuf::from(home).join(".darkian/ai_model_registry.json")
    }

    pub fn load() -> Self {
        let path = Self::storage_path();
        let models = if path.exists() {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        Self {
            models,
            storage_path: path,
        }
    }

    fn save(&self) {
        if let Some(parent) = self.storage_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string(&self.models) {
            let _ = std::fs::write(&self.storage_path, json);
        }
    }

    pub fn register(&mut self, model: ModelRegistration) {
        self.models.retain(|m| m.id != model.id);
        self.models.push(model);
        self.save();
    }

    pub fn remove(&mut self, id: &str) {
        self.models.retain(|m| m.id != id);
        self.save();
    }

    pub fn get(&self, id: &str) -> Option<&ModelRegistration> {
        self.models.iter().find(|m| m.id == id)
    }

    pub fn list(&self) -> &[ModelRegistration] {
        &self.models
    }

    pub fn update_status(&mut self, id: &str, status: ModelStatus) {
        if let Some(model) = self.models.iter_mut().find(|m| m.id == id) {
            model.status = status;
            self.save();
        }
    }
}

pub type ModelRegistryState = Arc<RwLock<ModelRegistryInner>>;

#[derive(Clone)]
pub struct AiSession {
    pub id: String,
    pub created_at: u64,
    pub metadata: Value,
}

pub type AiRegistry = Arc<RwLock<Vec<AiSession>>>;

pub struct InferenceSupervisor {
    pub pid: Option<u32>,
    pub status: String,
}

impl InferenceSupervisor {
    pub fn new() -> Self {
        Self {
            pid: None,
            status: "idle".to_string(),
        }
    }
}

pub type SupervisorState = Arc<RwLock<InferenceSupervisor>>;

#[derive(Clone)]
pub struct AiState {
    pub registry: AiRegistry,
    pub supervisor: SupervisorState,
    pub model_registry: ModelRegistryState,
}

impl AiState {
    pub fn new() -> Self {
        Self {
            registry: Arc::new(RwLock::new(Vec::new())),
            supervisor: Arc::new(RwLock::new(InferenceSupervisor::new())),
            model_registry: Arc::new(RwLock::new(ModelRegistryInner::load())),
        }
    }
}

static AI_SESSIONS_CREATED: AtomicU64 = AtomicU64::new(0);

fn ts_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

async fn ai_inspect(body: Option<Json<Value>>) -> Result<impl IntoResponse, AiError> {
    let path = body
        .as_ref()
        .and_then(|b| b.0.get("path"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if path.is_empty() {
        return Err(error::bad_request("path is required"));
    }

    if !std::path::Path::new(path).exists() {
        return Err(error::file_not_found(path));
    }

    match inspect::inspect_model(path) {
        Ok(data) => Ok(ok_response("inference.inspectModel", data)),
        Err(e) => Err(error::invalid_gguf(e)),
    }
}

async fn ai_load(body: Option<Json<Value>>) -> impl IntoResponse {
    let _ = body;
    ok_response("inference.loadModel", json!({ "loaded": true }))
}

async fn ai_unload(body: Option<Json<Value>>) -> impl IntoResponse {
    let _ = body;
    ok_response("inference.unloadModel", json!({ "unloaded": true }))
}

async fn ai_list_models(State(state): State<AiState>) -> impl IntoResponse {
    let guard = state.model_registry.read().await;
    let models = guard.list().to_vec();
    drop(guard);
    ok_response("inference.listModels", json!({ "models": models }))
}

async fn ai_model_register(
    State(state): State<AiState>,
    Json(body): Json<Value>,
) -> Result<impl IntoResponse, AiError> {
    let id = body
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let local_path = body
        .get("local_path")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if id.is_empty() {
        return Err(error::bad_request("model id is required"));
    }
    if local_path.is_empty() {
        return Err(error::bad_request("local_path is required"));
    }

    let provider = body
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let repository = body
        .get("repository")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let filename = body
        .get("filename")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let model = ModelRegistration {
        id,
        provider,
        repository,
        filename,
        revision: body
            .get("revision")
            .and_then(|v| v.as_str())
            .unwrap_or("latest")
            .to_string(),
        local_path,
        size: body.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
        sha256: body
            .get("sha256")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        quantisation: body
            .get("quantisation")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        parameter_count: body
            .get("parameter_count")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        download_url: body
            .get("download_url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        installed_at: body
            .get("installed_at")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        status: ModelStatus::Registered,
    };

    state.model_registry.write().await.register(model);
    Ok(ok_response(
        "inference.registerModel",
        json!({ "registered": true }),
    ))
}

async fn ai_model_remove(
    State(state): State<AiState>,
    Json(body): Json<Value>,
) -> Result<impl IntoResponse, AiError> {
    let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if id.is_empty() {
        return Err(error::bad_request("model id is required"));
    }
    state.model_registry.write().await.remove(id);
    Ok(ok_response(
        "inference.removeModel",
        json!({ "removed": true }),
    ))
}

async fn ai_model_get(
    State(state): State<AiState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AiError> {
    let guard = state.model_registry.read().await;
    let found = guard.get(&id).cloned();
    drop(guard);
    match found {
        Some(model) => Ok(ok_response("inference.getModel", json!(model))),
        None => Err(error::model_not_found(id)),
    }
}

async fn ai_model_update_status(
    State(state): State<AiState>,
    Json(body): Json<Value>,
) -> Result<impl IntoResponse, AiError> {
    let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let status_str = body.get("status").and_then(|v| v.as_str()).unwrap_or("");
    if id.is_empty() {
        return Err(error::bad_request("model id is required"));
    }
    let status = match status_str {
        "downloading" => ModelStatus::Downloading,
        "downloaded" => ModelStatus::Downloaded,
        "verified" => ModelStatus::Verified,
        "registered" => ModelStatus::Registered,
        "loaded" => ModelStatus::Loaded,
        "failed" => ModelStatus::Failed,
        "corrupted" => ModelStatus::Corrupted,
        _ => ModelStatus::Registered,
    };
    state.model_registry.write().await.update_status(id, status);
    Ok(ok_response(
        "inference.updateStatus",
        json!({ "status_updated": true }),
    ))
}

async fn ai_loaded_models() -> impl IntoResponse {
    ok_response("inference.loadedModels", json!({ "models": [] }))
}

async fn ai_delete(body: Option<Json<Value>>) -> impl IntoResponse {
    let _ = body;
    ok_response("inference.deleteModel", json!({ "deleted": true }))
}

async fn ai_create_session(
    State(state): State<AiState>,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    let _ = body;
    let id = uuid::Uuid::new_v4().to_string();
    let session = AiSession {
        id: id.clone(),
        created_at: ts_secs(),
        metadata: json!({}),
    };
    state.registry.write().await.push(session);
    AI_SESSIONS_CREATED.fetch_add(1, Ordering::Relaxed);
    ok_response("inference.createSession", json!({ "session_id": id }))
}

async fn ai_release_session(
    State(state): State<AiState>,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    let session_id = body
        .and_then(|b| b.0.get("session_id").cloned())
        .and_then(|v| v.as_str().map(String::from));
    if let Some(sid) = session_id {
        state.registry.write().await.retain(|s| s.id != sid);
    }
    ok_response("inference.releaseSession", json!({ "released": true }))
}

async fn ai_cancel_session(body: Option<Json<Value>>) -> impl IntoResponse {
    let _ = body;
    ok_response("inference.cancelSession", json!({ "cancelled": true }))
}

async fn ai_session_state(
    State(state): State<AiState>,
    Query(params): Query<Value>,
) -> impl IntoResponse {
    let session_id = params.get("session_id").and_then(|v| v.as_str());
    let guard = state.registry.read().await;
    let found = guard
        .iter()
        .find(|s| session_id.is_some_and(|sid| s.id == sid))
        .cloned();
    drop(guard);
    let data = found.as_ref().map(|s| {
        json!({
            "id": s.id,
            "created_at": s.created_at,
            "metadata": s.metadata
        })
    });
    ok_response("inference.sessionState", json!({ "session": data }))
}

async fn ai_generate(body: Option<Json<Value>>) -> impl IntoResponse {
    let _ = body;
    ok_response(
        "inference.generate",
        json!({ "text": "", "usage": { "prompt_tokens": 0, "completion_tokens": 0 } }),
    )
}

async fn ai_complete(body: Option<Json<Value>>) -> impl IntoResponse {
    let _ = body;
    ok_response(
        "inference.complete",
        json!({ "text": "", "usage": { "prompt_tokens": 0, "completion_tokens": 0 } }),
    )
}

async fn ai_embed(body: Option<Json<Value>>) -> impl IntoResponse {
    let _ = body;
    ok_response("inference.embed", json!({ "embeddings": [] }))
}

async fn ai_tokenize(body: Option<Json<Value>>) -> impl IntoResponse {
    let _ = body;
    ok_response("inference.tokenize", json!({ "tokens": [] }))
}

async fn ai_detokenize(body: Option<Json<Value>>) -> impl IntoResponse {
    let _ = body;
    ok_response("inference.detokenize", json!({ "text": "" }))
}

async fn ai_statistics() -> impl IntoResponse {
    let sessions_total = AI_SESSIONS_CREATED.load(Ordering::Relaxed);
    ok_response(
        "inference.statistics",
        json!({
            "sessions_created_total": sessions_total,
            "models_loaded": 0,
            "requests_processed": 0,
            "tokens_generated": 0
        }),
    )
}

async fn ai_memory() -> impl IntoResponse {
    ok_response(
        "inference.memory",
        json!({
            "resident_models": [],
            "total_allocated_bytes": 0,
            "available_bytes": 0
        }),
    )
}

async fn ai_health() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(
            json!({ "success": true, "status": "healthy", "service": "ai", "version": env!("CARGO_PKG_VERSION") }),
        ),
    )
}

async fn ai_capabilities() -> impl IntoResponse {
    ok_response(
        "inference.capabilities",
        json!({
            "chat": false,
            "completion": false,
            "fim": false,
            "embeddings": false,
            "tool_calling": false,
            "streaming": true,
            "model_inspection": true,
            "gguf_parsing": true,
            "metadata_extraction": true,
            "architecture_detection": true,
            "memory_estimation": true,
            "capability_detection": true
        }),
    )
}

async fn ai_diagnostics(State(state): State<AiState>) -> impl IntoResponse {
    let sessions_total = AI_SESSIONS_CREATED.load(Ordering::Relaxed);
    let guard = state.model_registry.read().await;
    let model_details: Vec<Value> = guard
        .list()
        .iter()
        .map(|m| {
            json!({
                "id": m.id,
                "name": m.filename,
                "quantisation": m.quantisation,
                "parameter_count": m.parameter_count,
                "size": m.size,
                "status": m.status,
                "path": m.local_path
            })
        })
        .collect();
    drop(guard);

    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "service": "ai",
            "status": "healthy",
            "data": {
                "sessions_created_total": sessions_total,
                "active_sessions": 0,
                "registered_models": model_details,
                "loaded_models": [],
                "resident_memory_bytes": 0,
                "pool": {
                    "enabled": false,
                    "capacity": null,
                    "loaded": 0
                },
                "gguf_support": {
                    "enabled": true,
                    "metadata_version": 3,
                    "supported_formats": ["GGUF"],
                    "max_context_length": 0,
                    "quantisations_supported": [
                        "Q2_K", "Q3_K_S", "Q3_K_M", "Q3_K_L",
                        "Q4_0", "Q4_1", "Q4_K_S", "Q4_K_M",
                        "Q5_0", "Q5_1", "Q5_K_S", "Q5_K_M",
                        "Q6_K", "Q8_0", "Q8_1",
                        "F16", "BF16",
                        "IQ1_S", "IQ1_M",
                        "IQ2_XXS", "IQ2_XS", "IQ2_S", "IQ2_M",
                        "IQ3_XXS", "IQ3_XS", "IQ3_S", "IQ3_M",
                        "IQ4_NL", "IQ4_XS"
                    ]
                },
                "model_count": model_details.len(),
                "uptime_secs": ts_secs()
            }
        })),
    )
}

async fn supervisor_spawn(
    State(state): State<AiState>,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    let _ = body;
    let mut sup = state.supervisor.write().await;
    sup.status = "running".to_string();
    sup.pid = None;
    ok_response(
        "supervisor.spawn",
        json!({ "pid": null, "status": "running" }),
    )
}

async fn supervisor_stop(State(state): State<AiState>) -> impl IntoResponse {
    let mut sup = state.supervisor.write().await;
    sup.status = "stopped".to_string();
    ok_response("supervisor.stop", json!({ "status": "stopped" }))
}

async fn supervisor_kill(State(state): State<AiState>) -> impl IntoResponse {
    let mut sup = state.supervisor.write().await;
    sup.status = "killed".to_string();
    sup.pid = None;
    ok_response("supervisor.kill", json!({ "status": "killed" }))
}

async fn supervisor_health(State(state): State<AiState>) -> impl IntoResponse {
    let sup = state.supervisor.read().await;
    ok_response(
        "supervisor.health",
        json!({ "pid": sup.pid, "status": sup.status, "alive": false }),
    )
}

async fn ai_generate_stream(
    ws: WebSocketUpgrade,
    State(state): State<AiState>,
) -> impl IntoResponse {
    let _ = state;
    ws.on_upgrade(handle_generate_stream)
}

async fn handle_generate_stream(socket: WebSocket) {
    let (mut sender, mut receiver) = socket.split();

    let done = json!({
        "type": "done",
        "data": {
            "text": "",
            "usage": { "prompt_tokens": 0, "completion_tokens": 0 }
        }
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    let _ = sender
        .send(Message::Text(serde_json::to_string(&done).unwrap().into()))
        .await;
    let _ = sender.send(Message::Close(None)).await;

    while let Some(msg) = receiver.next().await {
        if matches!(msg, Ok(Message::Close(_)) | Err(_)) {
            break;
        }
    }
}

pub fn ai_routes() -> Router<AiState> {
    Router::new()
        .route("/ai/inspect", post(ai_inspect))
        .route("/ai/load", post(ai_load))
        .route("/ai/unload", post(ai_unload))
        .route("/ai/models", get(ai_list_models))
        .route("/ai/models/register", post(ai_model_register))
        .route("/ai/models/remove", post(ai_model_remove))
        .route("/ai/models/update-status", post(ai_model_update_status))
        .route("/ai/models/{id}", get(ai_model_get))
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
        .route("/ai/supervisor/spawn", post(supervisor_spawn))
        .route("/ai/supervisor/stop", post(supervisor_stop))
        .route("/ai/supervisor/kill", post(supervisor_kill))
        .route("/ai/supervisor/health", get(supervisor_health))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;
    use tower::util::ServiceExt;

    fn test_state() -> AiState {
        AiState::new()
    }

    #[tokio::test]
    async fn test_ai_health() {
        let app = ai_routes().with_state(test_state());
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
        let app = ai_routes().with_state(test_state());
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
        assert!(json["data"]["pool"]["enabled"] == false);
        assert!(json["data"]["loaded_models"].is_array());
        assert!(json["data"]["registered_models"].is_array());
        assert!(json["data"]["gguf_support"]["enabled"] == true);
    }

    #[tokio::test]
    async fn test_ai_capabilities() {
        let app = ai_routes().with_state(test_state());
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
        let state = test_state();
        let app = ai_routes().with_state(state.clone());
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
        assert_eq!(state.registry.read().await.len(), 1);
    }

    #[tokio::test]
    async fn test_ai_list_models() {
        let app = ai_routes().with_state(test_state());
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
        let app = ai_routes().with_state(test_state());
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

    #[tokio::test]
    async fn test_supervisor_health() {
        let app = ai_routes().with_state(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/ai/supervisor/health")
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
        assert_eq!(json["data"]["status"], "idle");
    }

    #[tokio::test]
    async fn test_supervisor_spawn() {
        let state = test_state();
        let app = ai_routes().with_state(state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/ai/supervisor/spawn")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::to_vec(&json!({})).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(state.supervisor.read().await.status, "running");
    }

    #[tokio::test]
    async fn test_supervisor_kill() {
        let state = test_state();
        let app = ai_routes().with_state(state.clone());
        let _ = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/ai/supervisor/spawn")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::to_vec(&json!({})).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/ai/supervisor/kill")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(state.supervisor.read().await.status, "killed");
    }

    #[tokio::test]
    async fn test_ai_inspect_requires_path() {
        let app = ai_routes().with_state(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/ai/inspect")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::to_vec(&json!({})).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["message"], "path is required");
    }

    #[tokio::test]
    async fn test_ai_inspect_not_found() {
        let app = ai_routes().with_state(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/ai/inspect")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::to_vec(&json!({"path": "/nonexistent/foo.gguf"})).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_ai_diagnostics_pool_placeholders() {
        let app = ai_routes().with_state(test_state());
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
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["data"]["gguf_support"]["enabled"], true);
        assert!(json["data"]["gguf_support"]["quantisations_supported"].is_array());
        assert_eq!(json["data"]["pool"]["enabled"], false);
        assert_eq!(json["data"]["resident_memory_bytes"], 0);
        assert!(json["data"]["loaded_models"].is_array());
        assert!(json["data"]["registered_models"].is_array());
    }

    #[tokio::test]
    async fn test_ai_capabilities_includes_inspection() {
        let app = ai_routes().with_state(test_state());
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
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["data"]["model_inspection"], true);
        assert_eq!(json["data"]["gguf_parsing"], true);
    }

    #[tokio::test]
    async fn test_ai_inspect_errors_on_non_gguf() {
        let app = ai_routes().with_state(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/ai/inspect")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::to_vec(&json!({"path": "/dev/null"})).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }
}
