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
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
#[cfg(feature = "llama")]
use std::time::Instant;

use tokio::sync::RwLock;

#[cfg(feature = "llama")]
use crate::ai::backend_trait::{InferenceBackend, TokenSink};
#[cfg(feature = "llama")]
use crate::ai::context_config::auto_n_ctx;
use crate::ai::error::{self, AiError};
use crate::ai::generation_event::{GenerationEvent, SessionState};
use crate::ai::inspect;
#[cfg(feature = "llama")]
use crate::ai::llama_backend::LlamaBackend;
#[cfg(feature = "llama")]
use crate::ai::output_parser::OutputParser;
use crate::ai::pool::{
    LoadLockManager, LoadLockManagerState, ModelPoolInner, ModelPoolState, PoolConfig,
};

/// Total physical RAM in bytes (MemTotal from /proc/meminfo), used as the
/// RAM budget for the auto context-size heuristic.
/// Cached after first read to avoid per-inference /proc parsing.
#[cfg(feature = "llama")]
fn read_total_memory_bytes() -> u64 {
    use std::sync::OnceLock;
    static CACHE: OnceLock<u64> = OnceLock::new();
    *CACHE.get_or_init(|| {
        std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|s| {
                s.lines().find_map(|l| {
                    let mut it = l.split_whitespace();
                    if it.next() == Some("MemTotal:") {
                        it.next().and_then(|v| v.parse::<u64>().ok())
                    } else {
                        None
                    }
                })
            })
            .map(|kb| kb.saturating_mul(1024))
            .unwrap_or(4_000_000_000)
    })
}
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
        PathBuf::from(home).join(".ds/ai_model_registry.json")
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

pub type CancellationMap = Arc<RwLock<HashMap<String, Arc<AtomicBool>>>>;

#[derive(Clone)]
pub struct AiState {
    pub registry: AiRegistry,
    pub supervisor: SupervisorState,
    pub model_registry: ModelRegistryState,
    pub model_pool: ModelPoolState,
    pub load_locks: LoadLockManagerState,
    pub cancel_tokens: CancellationMap,
}

impl AiState {
    pub fn new() -> Self {
        Self {
            registry: Arc::new(RwLock::new(Vec::new())),
            supervisor: Arc::new(RwLock::new(InferenceSupervisor::new())),
            model_registry: Arc::new(RwLock::new(ModelRegistryInner::load())),
            model_pool: Arc::new(RwLock::new(ModelPoolInner::new(PoolConfig::default()))),
            load_locks: Arc::new(LoadLockManager::new()),
            cancel_tokens: Arc::new(RwLock::new(HashMap::new())),
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

async fn ai_load(
    State(state): State<AiState>,
    body: Option<Json<Value>>,
) -> Result<impl IntoResponse, AiError> {
    let body = body.ok_or_else(|| error::bad_request("request body is required"))?;

    let path = body
        .0
        .get("path")
        .and_then(|v| v.as_str())
        .filter(|p| !p.is_empty())
        .map(String::from);

    let id = body
        .0
        .get("id")
        .and_then(|v| v.as_str())
        .filter(|i| !i.is_empty())
        .map(String::from);

    let model_path = if let Some(p) = path {
        p
    } else if let Some(id) = id {
        let guard = state.model_registry.read().await;
        let model = guard.get(&id).cloned();
        drop(guard);
        match model {
            Some(m) => m.local_path,
            None => return Err(error::model_not_found(id)),
        }
    } else {
        return Err(error::bad_request("either id or path is required"));
    };

    if !std::path::Path::new(&model_path).exists() {
        return Err(error::file_not_found(&model_path));
    }

    // Acquire per-model load lock to prevent duplicate loads
    let lock_key = format!("path:{}", model_path);
    let semaphore = state.load_locks.acquire(&lock_key).await;
    let _permit = semaphore.acquire().await.unwrap();

    let pool = state.model_pool.clone();
    let mut guard = pool.write().await;
    match guard.load(&model_path) {
        Ok(model) => {
            let view = model.to_view();
            let ref_count = model.lifecycle.ref_count;
            Ok(ok_response(
                "inference.loadModel",
                json!({ "loaded": true, "model": view, "ref_count": ref_count }),
            ))
        }
        Err(e) => {
            guard.record_load_failure(&e);
            Err(error::internal_error(e))
        }
    }
}

async fn ai_unload(
    State(state): State<AiState>,
    body: Option<Json<Value>>,
) -> Result<impl IntoResponse, AiError> {
    let body = body.ok_or_else(|| error::bad_request("request body is required"))?;
    let id = body.0.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if id.is_empty() {
        return Err(error::bad_request("model id is required"));
    }

    let pool = state.model_pool.clone();
    let mut guard = pool.write().await;

    // Resolve pool_id: try as direct pool_id, then as registry_id
    let pool_id = if guard.get(id).is_some() {
        id.to_string()
    } else if let Some(model) = guard.get_by_registry_id(id) {
        model.metadata.pool_id.clone()
    } else {
        return Err(error::model_not_found(id));
    };

    match guard.unload(&pool_id) {
        Ok(fully_unloaded) => {
            let ref_count = match guard.get(&pool_id) {
                Some(m) => m.lifecycle.ref_count,
                None => 0,
            };
            Ok(ok_response(
                "inference.unloadModel",
                json!({ "unloaded": fully_unloaded, "ref_count": ref_count }),
            ))
        }
        Err(e) => Err(error::internal_error(e)),
    }
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

async fn ai_loaded_models(State(state): State<AiState>) -> impl IntoResponse {
    let guard = state.model_pool.read().await;
    let models = guard.list();
    drop(guard);
    ok_response("inference.loadedModels", json!({ "models": models }))
}

async fn ai_delete(body: Option<Json<Value>>) -> Result<axum::response::Response, AiError> {
    let _ = body;
    Err(AiError::new(
        StatusCode::NOT_IMPLEMENTED,
        "NOT_IMPLEMENTED",
        "deleteModel not implemented; use /ai/models/remove or /ai/unload",
    ))
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

async fn ai_cancel_session(
    State(state): State<AiState>,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    let session_id = body
        .and_then(|b| b.0.get("session_id").cloned())
        .and_then(|v| v.as_str().map(String::from));

    if let Some(sid) = session_id {
        if let Some(token) = state.cancel_tokens.write().await.remove(&sid) {
            token.store(true, Ordering::SeqCst);
        }
    }

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

/// Resolve an empty model_id to the pool_id of the first loaded model so
/// clients that never send a model id (e.g. the DS app's local chat
/// transport) still work against the loaded model.
async fn resolve_model_id(state: &AiState, model_id: &str) -> Result<String, AiError> {
    if !model_id.is_empty() {
        return Ok(model_id.to_string());
    }
    let guard = state.model_pool.read().await;
    let first = guard.list().first().cloned();
    drop(guard);
    let m = first.ok_or_else(|| error::bad_request("no model loaded and no model_id provided"))?;
    let pool_id = m["metadata"]["pool_id"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| error::bad_request("loaded model has no pool_id"))?;
    tracing::warn!("resolve_model_id: empty model_id, falling back to first loaded {pool_id}");
    Ok(pool_id)
}

async fn ai_chat(
    State(state): State<AiState>,
    body: Option<Json<Value>>,
) -> Result<impl IntoResponse, AiError> {
    let body = body.ok_or_else(|| error::bad_request("body required"))?;
    let body_value = body.0.clone();

    let messages = body_value
        .get("messages")
        .and_then(|v| v.as_array())
        .ok_or_else(|| error::bad_request("messages array required"))?;
    let model_id = body_value
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let model_id = resolve_model_id(&state, &model_id).await?;

    let n_ctx = body_value
        .get("n_ctx")
        .and_then(|v| v.as_u64())
        .unwrap_or(2048) as u32;
    if let Err(e) = crate::ai::chat_template::validate_context(messages, n_ctx) {
        return Err(error::bad_request(&e));
    }

    #[cfg(feature = "llama")]
    {
        return ai_generate_shared(state, body_value, &model_id).await;
    }

    #[cfg(not(feature = "llama"))]
    {
        let _ = (state, model_id);
        Err::<(StatusCode, Json<Value>), AiError>(error::internal_error(
            "Inference backend not compiled. Rebuild dsterm with the `llama` feature enabled.",
        ))
    }
}

async fn ai_generate(
    State(state): State<AiState>,
    body: Option<Json<Value>>,
) -> Result<impl IntoResponse, AiError> {
    let body = body.ok_or_else(|| error::bad_request("body required"))?;

    let body_value = body.0.clone();
    let prompt = body_value
        .get("prompt")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let model_id = body_value
        .get("model_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if prompt.is_empty() {
        return Err(error::bad_request("prompt is required"));
    }
    let model_id = resolve_model_id(&state, &model_id).await?;

    #[cfg(feature = "llama")]
    {
        return ai_generate_shared(state, body_value, &model_id).await;
    }

    #[cfg(not(feature = "llama"))]
    {
        let _ = (state, model_id);
        Err::<(StatusCode, Json<Value>), AiError>(error::internal_error(
            "Inference backend not compiled. Rebuild dsterm with the `llama` feature enabled.",
        ))
    }
}

/// Run inference through the shared pipeline for HTTP endpoints.
/// Extracts sampling params from the JSON body and executes via the Scheduler.
#[cfg(feature = "llama")]
async fn ai_generate_shared(
    state: AiState,
    body: Value,
    model_id: &str,
) -> Result<impl IntoResponse, AiError> {
    let mut req = crate::ai::inference_request::InferenceRequest::from_value(&body);
    let pool = state.model_pool.read().await;
    let loaded = pool
        .get(model_id)
        .or_else(|| pool.get_by_registry_id(model_id))
        .map(|m| {
            (
                m.metadata.pool_id.clone(),
                m.metadata.architecture.clone(),
                m.metadata.memory_estimate.clone(),
                m.runtime.as_ref().and_then(|r| r.model.clone()),
            )
        })
        .ok_or_else(|| error::model_not_found(model_id))?;

    let (_pool_id, arch, mem, llama_model) = loaded;
    let model = llama_model.ok_or_else(|| error::internal_error("model has no llama backend"))?;
    req.architecture = arch;
    if req.n_ctx == 0 {
        req.n_ctx = auto_n_ctx(&mem, read_total_memory_bytes());
    }
    drop(pool);

    let backend = Arc::new(LlamaBackend::new(model));
    if body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        let result = crate::ai::scheduler::ImmediateScheduler::execute_sync_stream(&req, backend)
            .await
            .map_err(|e| error::internal_error(e.message()))?;
        Ok(ok_response(
            "inference.generate",
            json!({
                "text": result.text,
                "usage": {
                    "prompt_tokens": result.prompt_tokens,
                    "completion_tokens": result.completion_tokens
                }
            }),
        ))
    } else {
        let result = crate::ai::scheduler::ImmediateScheduler::execute_sync(&req, backend)
            .await
            .map_err(|e| error::internal_error(e.message()))?;
        Ok(ok_response(
            "inference.generate",
            json!({
                "text": result.text,
                "usage": {
                    "prompt_tokens": result.prompt_tokens,
                    "completion_tokens": result.completion_tokens
                }
            }),
        ))
    }
}

async fn ai_complete(
    State(state): State<AiState>,
    body: Option<Json<Value>>,
) -> Result<impl IntoResponse, AiError> {
    let body = body.ok_or_else(|| error::bad_request("body required"))?;

    let body_value = body.0.clone();

    let model_id = body_value
        .get("model_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // If no model_id provided, use the first loaded model
    let model_id = resolve_model_id(&state, &model_id).await?;

    #[cfg(feature = "llama")]
    {
        return ai_generate_shared(state, body_value, &model_id).await;
    }

    #[cfg(not(feature = "llama"))]
    {
        let _ = (state, model_id);
        Ok(ok_response(
            "inference.complete",
            json!({ "text": "", "usage": { "prompt_tokens": 0, "completion_tokens": 0 } }),
        ))
    }
}

async fn ai_embed(
    State(state): State<AiState>,
    body: Option<Json<Value>>,
) -> Result<impl IntoResponse, AiError> {
    let body = body.ok_or_else(|| error::bad_request("body required"))?;
    let body_value = body.0.clone();

    let texts = body_value
        .get("texts")
        .and_then(|v| v.as_array())
        .ok_or_else(|| error::bad_request("texts array required"))?;

    let text_strings: Vec<String> = texts
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();

    if text_strings.is_empty() {
        return Ok(ok_response("inference.embed", json!({ "embeddings": [] })));
    }

    let model_id = body_value
        .get("model_id")
        .or_else(|| body_value.get("model"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let model_id = resolve_model_id(&state, model_id).await?;

    #[cfg(feature = "llama")]
    {
        let pool = state.model_pool.read().await;
        let loaded = pool
            .get(&model_id)
            .or_else(|| pool.get_by_registry_id(&model_id))
            .map(|m| {
                (
                    m.metadata.pool_id.clone(),
                    m.runtime.as_ref().and_then(|r| r.model.clone()),
                )
            })
            .ok_or_else(|| error::model_not_found(&model_id))?;

        let (_pool_id, llama_model) = loaded;
        let model =
            llama_model.ok_or_else(|| error::internal_error("model has no llama backend"))?;
        drop(pool);

        let backend = Arc::new(LlamaBackend::new(model));
        let embeddings = backend
            .embed(&text_strings)
            .await
            .map_err(|e| error::internal_error(e.message()))?;

        Ok(ok_response(
            "inference.embed",
            json!({ "embeddings": embeddings }),
        ))
    }

    #[cfg(not(feature = "llama"))]
    {
        let _ = (state, model_id);
        Ok(ok_response("inference.embed", json!({ "embeddings": [] })))
    }
}

async fn ai_tokenize(body: Option<Json<Value>>) -> Result<axum::response::Response, AiError> {
    let _ = body;
    Err(AiError::new(
        StatusCode::NOT_IMPLEMENTED,
        "NOT_IMPLEMENTED",
        "tokenize not implemented; llama backend required",
    ))
}

async fn ai_detokenize(body: Option<Json<Value>>) -> Result<axum::response::Response, AiError> {
    let _ = body;
    Err(AiError::new(
        StatusCode::NOT_IMPLEMENTED,
        "NOT_IMPLEMENTED",
        "detokenize not implemented; llama backend required",
    ))
}
async fn ai_statistics(State(state): State<AiState>) -> impl IntoResponse {
    let sessions_total = AI_SESSIONS_CREATED.load(Ordering::Relaxed);
    let pool_guard = state.model_pool.read().await;
    let stats = pool_guard.stats();
    let pool_consistent = stats.pool_consistent;
    drop(pool_guard);
    ok_response(
        "inference.statistics",
        json!({
            "sessions_created_total": sessions_total,
            "models_loaded": stats.loaded_count,
            "active_references": stats.total_ref_count,
            "pool_consistent": pool_consistent,
            "requests_processed": 0,
            "tokens_generated": 0
        }),
    )
}

async fn ai_memory(State(state): State<AiState>) -> impl IntoResponse {
    let guard = state.model_pool.read().await;
    let stats = guard.stats();
    drop(guard);
    ok_response(
        "inference.memory",
        json!({
            "resident_models": stats.loaded_count,
            "total_allocated_bytes": stats.memory.total_bytes,
            "available_bytes": stats.available_bytes,
            "pool_stats": stats
        }),
    )
}

async fn ai_pool_health(State(state): State<AiState>) -> impl IntoResponse {
    let guard = state.model_pool.read().await;
    let health = guard.health();
    drop(guard);
    ok_response("inference.poolHealth", json!(health))
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
            "chat": cfg!(feature = "llama"),
            "completion": cfg!(feature = "llama"),
            "fim": cfg!(feature = "llama"),
        "fim_streaming": cfg!(feature = "llama"),
            "embeddings": cfg!(feature = "llama"),
            "tool_calling": cfg!(feature = "llama"),
            "streaming": cfg!(feature = "llama"),
            "model_inspection": true,
            "gguf_parsing": true,
            "metadata_extraction": true,
            "architecture_detection": true,
            "memory_estimation": true,
            "capability_detection": true,
            "llama_backend": cfg!(feature = "llama"),
            "per_token_streaming": cfg!(feature = "llama"),
            "thinking": cfg!(feature = "llama")
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

    let pool_guard = state.model_pool.read().await;
    let loaded_models = pool_guard.list();
    let pool_stats = pool_guard.stats();
    let pool_loaded = pool_stats.loaded_count;
    let pool_capacity = pool_stats.max_models;
    let resident_memory = pool_stats.memory.total_bytes;
    let pool_consistent = pool_stats.pool_consistent;
    drop(pool_guard);

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
                "loaded_models": loaded_models,
                "resident_memory_bytes": resident_memory,
                "pool_consistent": pool_consistent,
                "pool": {
                    "enabled": true,
                    "capacity": pool_capacity,
                    "loaded": pool_loaded
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
    ws.on_upgrade(move |socket| handle_generate_stream(socket, state))
}

async fn handle_generate_stream(socket: WebSocket, state: AiState) {
    let (mut sender, mut receiver) = socket.split();
    let cancel = Arc::new(AtomicBool::new(false));
    let session_state = Arc::new(RwLock::new(SessionState::Idle));

    // Wait for first message with generation params
    let msg = match receiver.next().await {
        Some(Ok(Message::Text(text))) => text,
        _ => {
            let _ = sender
                .send(Message::Text(
                    serde_json::to_string(&GenerationEvent::Error {
                        message: "expected text message".into(),
                    })
                    .unwrap()
                    .into(),
                ))
                .await;
            return;
        }
    };

    let params: Value = match serde_json::from_str(&msg) {
        Ok(v) => v,
        Err(_) => {
            let _ = sender
                .send(Message::Text(
                    serde_json::to_string(&GenerationEvent::Error {
                        message: "invalid JSON".into(),
                    })
                    .unwrap()
                    .into(),
                ))
                .await;
            return;
        }
    };

    let model_id = params
        .get("model_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let session_id = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let mode = params
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("chat")
        .to_string();

    // Validate that prompt or messages are present (will be resolved by run_generation via InferenceRequest)
    let has_prompt = params
        .get("prompt")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty());
    let has_messages = params
        .get("messages")
        .and_then(|v| v.as_array())
        .is_some_and(|a| !a.is_empty());
    let has_prefix_suffix = params
        .get("prefix")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty())
        || params
            .get("suffix")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty());
    if !has_prompt && !has_messages && !has_prefix_suffix {
        let _ = sender
            .send(Message::Text(
                serde_json::to_string(&GenerationEvent::Error {
                    message: "prompt, messages, or prefix/suffix required".into(),
                })
                .unwrap()
                .into(),
            ))
            .await;
        return;
    }

    // No model_id provided: fall back to the first loaded model so local
    // clients (DS chat, which never sends a model id) work out of the box.
    let model_id = match resolve_model_id(&state, &model_id).await {
        Ok(id) => id,
        Err(e) => {
            let _ = sender
                .send(Message::Text(
                    serde_json::to_string(&GenerationEvent::Error {
                        message: e.body.message,
                    })
                    .unwrap()
                    .into(),
                ))
                .await;
            let _ = sender.send(Message::Close(None)).await;
            return;
        }
    };

    // Session setup
    if !session_id.is_empty() {
        let mut ssl = session_state.write().await;
        if !ssl.can_transition_to(SessionState::Generating) {
            let _ = sender
                .send(Message::Text(
                    serde_json::to_string(&GenerationEvent::Error {
                        message: "session is closed or already generating".into(),
                    })
                    .unwrap()
                    .into(),
                ))
                .await;
            return;
        }
        *ssl = SessionState::Generating;

        state
            .cancel_tokens
            .write()
            .await
            .insert(session_id.clone(), cancel.clone());
    }

    // FIM priority: FIM requests run at higher priority for responsive editor
    let priority = if mode == "fim" { 10i32 } else { 0i32 };

    // Start generation
    let result = run_generation(
        &mut sender,
        &mut receiver,
        state.clone(),
        &params,
        "",
        &model_id,
        cancel.clone(),
        session_state.clone(),
        priority,
    )
    .await;

    // Cleanup
    if !session_id.is_empty() {
        state.cancel_tokens.write().await.remove(&session_id);
        let mut ssl = session_state.write().await;
        *ssl = SessionState::Idle;
    }

    if let Err(e) = result {
        let _ = sender
            .send(Message::Text(
                serde_json::to_string(&GenerationEvent::Error { message: e })
                    .unwrap()
                    .into(),
            ))
            .await;
    }

    let _ = sender.send(Message::Close(None)).await;
    while let Some(msg) = receiver.next().await {
        if matches!(msg, Ok(Message::Close(_)) | Err(_)) {
            break;
        }
    }
}

#[cfg(feature = "llama")]
#[allow(clippy::too_many_arguments)]
async fn run_generation(
    sender: &mut futures::stream::SplitSink<WebSocket, Message>,
    receiver: &mut futures::stream::SplitStream<WebSocket>,
    state: AiState,
    params: &Value,
    _prompt: &str,
    model_id: &str,
    cancel: Arc<AtomicBool>,
    session_state: Arc<RwLock<SessionState>>,
    _priority: i32,
) -> Result<(), String> {
    let start_time = Instant::now();
    let mut req = crate::ai::inference_request::InferenceRequest::from_value(params);
    // Auto-load & pool acquire
    let (llama_model, pool_id, arch, mem) = {
        let mut pool = state.model_pool.write().await;
        let found = pool
            .get(model_id)
            .or_else(|| pool.get_by_registry_id(model_id))
            .map(|m| {
                let pool_id = m.metadata.pool_id.clone();
                let arch = m.metadata.architecture.clone();
                let mem = m.metadata.memory_estimate.clone();
                let backend = m.runtime.as_ref().and_then(|r| r.model.clone());
                (pool_id, arch, mem, backend)
            });
        if let Some((pool_id, arch, mem, backend)) = found {
            let backend = backend.ok_or_else(|| format!("model {model_id} has no backend"))?;
            let m = pool.models.get_mut(&pool_id).unwrap();
            m.lifecycle.acquire().map_err(|e| format!("acquire: {e}"))?;
            (backend, pool_id, arch, mem)
        } else {
            drop(pool);
            let registry = state.model_registry.read().await;
            let local_path = registry
                .get(model_id)
                .map(|m| m.local_path.clone())
                .ok_or_else(|| format!("model not found in registry: {model_id}"))?;
            drop(registry);
            let mut pool = state.model_pool.write().await;
            pool.load(&local_path)?;
            let m = pool
                .get_by_registry_id(model_id)
                .ok_or_else(|| format!("model not loaded after auto-load: {model_id}"))?;
            let pool_id = m.metadata.pool_id.clone();
            let arch = m.metadata.architecture.clone();
            let mem = m.metadata.memory_estimate.clone();
            let backend = m
                .runtime
                .as_ref()
                .and_then(|r| r.model.clone())
                .ok_or_else(|| format!("model {model_id} has no backend"))?;
            (backend, pool_id, arch, mem)
        }
    };

    // Send protocol frame
    let protocol = GenerationEvent::Protocol {
        protocol_version: 1,
        backend: "llama".into(),
        model: model_id.to_string(),
    };
    let _ = sender
        .send(Message::Text(
            serde_json::to_string(&protocol).unwrap().into(),
        ))
        .await;

    // The backend resolves the prompt itself (native chat template for chat
    // mode); it needs the architecture for FIM template selection.
    req.architecture = arch;

    if req.n_ctx == 0 {
        req.n_ctx = auto_n_ctx(&mem, read_total_memory_bytes());
    }
    let context_config = req.to_context_config();
    let sampling_config = req.to_sampling_config();
    let max_tokens = req.max_tokens;

    let backend = LlamaBackend::new(llama_model);

    let (tx, mut rx) = tokio::sync::mpsc::channel::<GenerationEvent>(128);

    let sink_tx = tx.clone();
    let sink = Box::new(GenerationWsSink {
        tx: sink_tx,
        cancel: cancel.clone(),
        first_token: std::sync::atomic::AtomicBool::new(false),
        tool_parser: crate::ai::output_parser::ToolCallParser::new(),
    });

    let request = req.clone();

    let join_handle = tokio::spawn(async move {
        backend
            .generate_streaming(&request, context_config, sampling_config, max_tokens, sink)
            .await
    });

    drop(tx);

    let mut generation_done = false;
    let mut first_token_time: Option<Instant> = None;

    loop {
        tokio::select! {
            event = rx.recv() => {
                match event {
                    Some(GenerationEvent::Text { text }) => {
                        if first_token_time.is_none() {
                            first_token_time = Some(Instant::now());
                        }
                        let json = serde_json::to_string(&GenerationEvent::Text {
                            text,
                        }).unwrap();
                        if sender.send(Message::Text(json.into())).await.is_err() {
                            cancel.store(true, Ordering::Relaxed);
                            break;
                        }
                    }
                    Some(e @ (GenerationEvent::Usage { .. }
                             | GenerationEvent::Done
                             | GenerationEvent::Error { .. }
                             | GenerationEvent::Reasoning { .. }
                             | GenerationEvent::ToolCall { .. })) => {
                        let json = serde_json::to_string(&e).unwrap();
                        let _ = sender.send(Message::Text(json.into())).await;
                        if matches!(e, GenerationEvent::Done | GenerationEvent::Error { .. }) {
                            generation_done = matches!(e, GenerationEvent::Done);
                            break;
                        }
                    }
                    Some(_) => {}
                    None => {
                        generation_done = true;
                        break;
                    }
                }
            }
            ws_msg = receiver.next() => {
                match ws_msg {
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => {
                        cancel.store(true, Ordering::Relaxed);
                        {
                            let mut s = session_state.write().await;
                            if *s == SessionState::Generating {
                                *s = SessionState::Cancelling;
                            }
                        }
                        break;
                    }
                    Some(Ok(Message::Ping(data))) => {
                        let _ = sender.send(Message::Pong(data)).await;
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(cmd) = serde_json::from_str::<Value>(&text) {
                            if cmd.get("type").and_then(|v| v.as_str()) == Some("cancel") {
                                cancel.store(true, Ordering::Relaxed);
                                {
                                    let mut s = session_state.write().await;
                                    if *s == SessionState::Generating {
                                        *s = SessionState::Cancelling;
                                    }
                                }
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    let output = match join_handle.await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            if generation_done {
                return Err(format!("generation failed: {e}"));
            }
            return Ok(());
        }
        Err(e) => {
            return Err(format!("task failed: {e}"));
        }
    };

    // Scan completed output for any tool calls not yet extracted during streaming
    let final_tool_calls = crate::ai::output_parser::extract_tool_calls(&output.text);
    for tc_event in &final_tool_calls {
        if let crate::ai::stream_event::StreamEvent::ToolCall {
            id,
            function_name,
            arguments,
            ..
        } = tc_event
        {
            let _ = sender
                .send(Message::Text(
                    serde_json::to_string(&GenerationEvent::ToolCall {
                        id: id.clone(),
                        function_name: function_name.clone(),
                        arguments: arguments.clone(),
                    })
                    .unwrap()
                    .into(),
                ))
                .await;
        }
    }

    if generation_done {
        let elapsed = start_time.elapsed();
        let ttft = first_token_time
            .map(|t| t.duration_since(start_time).as_millis() as u64)
            .unwrap_or(0);
        let tps = if elapsed.as_secs_f64() > 0.0 {
            output.completion_tokens as f64 / elapsed.as_secs_f64()
        } else {
            0.0
        };

        let usage = GenerationEvent::Usage {
            prompt_tokens: output.prompt_tokens,
            completion_tokens: output.completion_tokens,
            total_tokens: output.prompt_tokens + output.completion_tokens,
            tokens_per_second: tps,
            first_token_ms: ttft,
            generation_ms: elapsed.as_millis() as u64,
        };
        let _ = sender
            .send(Message::Text(serde_json::to_string(&usage).unwrap().into()))
            .await;

        let _ = sender
            .send(Message::Text(
                serde_json::to_string(&GenerationEvent::Done)
                    .unwrap()
                    .into(),
            ))
            .await;
    }

    // Pool release
    {
        let mut pool = state.model_pool.write().await;
        let _ = pool.unload(&pool_id);
    }

    Ok(())
}

#[cfg(not(feature = "llama"))]
#[allow(clippy::too_many_arguments)]
async fn run_generation(
    sender: &mut futures::stream::SplitSink<WebSocket, Message>,
    _receiver: &mut futures::stream::SplitStream<WebSocket>,
    _state: AiState,
    _params: &Value,
    _prompt: &str,
    _model_id: &str,
    _cancel: Arc<AtomicBool>,
    _session_state: Arc<RwLock<SessionState>>,
    _priority: i32,
) -> Result<(), String> {
    let _ = (
        _receiver,
        _state,
        _params,
        _prompt,
        _model_id,
        _cancel,
        _session_state,
        _priority,
    );
    let _ = sender
        .send(Message::Text(
            serde_json::to_string(&GenerationEvent::Error {
                message: "Inference backend not compiled. Rebuild dsterm with the `llama` feature enabled.".to_string(),
            })
            .unwrap()
            .into(),
        ))
        .await;
    Ok(())
}

#[cfg(feature = "llama")]
struct GenerationWsSink {
    tx: tokio::sync::mpsc::Sender<GenerationEvent>,
    cancel: Arc<AtomicBool>,
    first_token: std::sync::atomic::AtomicBool,
    tool_parser: crate::ai::output_parser::ToolCallParser,
}

#[cfg(feature = "llama")]
impl GenerationWsSink {
    fn send_tool_call(&mut self, event: &crate::ai::stream_event::StreamEvent) {
        if let crate::ai::stream_event::StreamEvent::ToolCall {
            id,
            function_name,
            arguments,
            ..
        } = event
        {
            let tc = GenerationEvent::ToolCall {
                id: id.clone(),
                function_name: function_name.clone(),
                arguments: arguments.clone(),
            };
            let _ = self.tx.try_send(tc);
        }
    }
}

#[cfg(feature = "llama")]
impl TokenSink for GenerationWsSink {
    fn on_token(&mut self, token: &str) -> Result<(), String> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        self.first_token.store(true, Ordering::Relaxed);

        // Feed token to tool call parser — it extracts tool calls from the raw output
        let tool_events = self.tool_parser.push(token);
        for event in &tool_events {
            self.send_tool_call(event);
        }
        let event = GenerationEvent::Text {
            text: token.to_string(),
        };
        self.tx.try_send(event).map_err(|e| format!("send: {e}"))
    }

    fn on_error(&mut self, err: &str) {
        let event = GenerationEvent::Error {
            message: err.to_string(),
        };
        let _ = self.tx.try_send(event);
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
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
        .route("/ai/chat", post(ai_chat))
        .route("/ai/generate", post(ai_generate))
        .route("/ai/complete", post(ai_complete))
        .route("/ai/embed", post(ai_embed))
        .route("/ai/tokenize", post(ai_tokenize))
        .route("/ai/detokenize", post(ai_detokenize))
        .route("/ai/statistics", get(ai_statistics))
        .route("/ai/memory", get(ai_memory))
        .route("/ai/pool/health", get(ai_pool_health))
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
        assert!(json["data"]["pool"]["enabled"] == true);
        assert_eq!(json["data"]["pool_consistent"], true);
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
        assert_eq!(json["data"]["streaming"], cfg!(feature = "llama"));
        assert_eq!(json["data"]["chat"], cfg!(feature = "llama"));
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
    async fn test_ai_generate() {
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
                            "model_id": "nonexistent"
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        #[cfg(not(feature = "llama"))]
        {
            // Without llama backend, generate must fail loudly, not silently succeed
            assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        }
        #[cfg(feature = "llama")]
        {
            // With llama backend but no loaded model, returns model_not_found
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
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
        assert_eq!(json["error"]["message"], "path is required");
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
        assert_eq!(json["data"]["pool"]["enabled"], true);
        assert_eq!(json["data"]["pool"]["loaded"], 0);
        assert_eq!(json["data"]["resident_memory_bytes"], 0);
        assert_eq!(json["data"]["pool_consistent"], true);
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

    #[tokio::test]
    async fn test_ai_load_requires_id_or_path() {
        let app = ai_routes().with_state(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/ai/load")
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
        assert_eq!(json["error"]["message"], "either id or path is required");
    }

    #[tokio::test]
    async fn test_ai_load_model_not_found() {
        let app = ai_routes().with_state(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/ai/load")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::to_vec(&json!({"id": "nonexistent-model"})).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_ai_load_file_not_found() {
        let app = ai_routes().with_state(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/ai/load")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::to_vec(&json!({"path": "/nonexistent/model.gguf"})).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_ai_unload_model_not_loaded() {
        let app = ai_routes().with_state(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/ai/unload")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::to_vec(&json!({"id": "nonexistent-model"})).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_ai_unload_requires_id() {
        let app = ai_routes().with_state(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/ai/unload")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::to_vec(&json!({})).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_ai_loaded_models_empty() {
        let app = ai_routes().with_state(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/ai/models/loaded")
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
        assert_eq!(json["data"]["models"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_ai_statistics_includes_pool() {
        let app = ai_routes().with_state(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/ai/statistics")
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
        assert_eq!(json["data"]["models_loaded"], 0);
        assert_eq!(json["data"]["active_references"], 0);
        assert_eq!(json["data"]["pool_consistent"], true);
    }

    #[tokio::test]
    async fn test_ai_memory_includes_pool() {
        let app = ai_routes().with_state(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/ai/memory")
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
        assert_eq!(json["data"]["resident_models"], 0);
        assert_eq!(json["data"]["total_allocated_bytes"], 0);
        assert!(json["data"]["pool_stats"].is_object());
        assert_eq!(json["data"]["pool_stats"]["pool_consistent"], true);
    }

    #[tokio::test]
    async fn test_ai_cancel_session() {
        let state = test_state();
        let app = ai_routes().with_state(state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/ai/sessions/cancel")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::to_vec(&json!({"session_id": "test-session"})).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_ai_pool_health() {
        let app = ai_routes().with_state(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/ai/pool/health")
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
        assert_eq!(json["data"]["healthy"], true);
        assert_eq!(json["data"]["loaded"], 0);
    }
}
