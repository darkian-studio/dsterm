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
use std::time::Duration;
use tokio::sync::RwLock;

use crate::ai::error::{self, AiError};
use crate::ai::inspect;
#[cfg(feature = "llama")]
use crate::ai::llama;
use crate::ai::pool::{
    LoadLockManager, LoadLockManagerState, ModelPoolInner, ModelPoolState, PoolConfig,
};

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

async fn ai_generate(
    State(state): State<AiState>,
    body: Option<Json<Value>>,
) -> Result<impl IntoResponse, AiError> {
    let body = body.ok_or_else(|| error::bad_request("body required"))?;

    let prompt = body.0.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
    let model_id = body
        .0
        .get("model_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if prompt.is_empty() {
        return Err(error::bad_request("prompt is required"));
    }
    if model_id.is_empty() {
        return Err(error::bad_request("model_id is required"));
    }

    #[cfg(feature = "llama")]
    {
        return ai_generate_real(state, body.0, prompt, model_id).await;
    }

    #[cfg(not(feature = "llama"))]
    {
        let _ = (state, model_id);
        Ok(ok_response(
            "inference.generate",
            json!({ "text": "", "usage": { "prompt_tokens": 0, "completion_tokens": 0 } }),
        ))
    }
}

#[cfg(feature = "llama")]
async fn ai_generate_real(
    state: AiState,
    body: Value,
    prompt: &str,
    model_id: &str,
) -> Result<impl IntoResponse, AiError> {
    let pool = state.model_pool.read().await;
    let loaded = pool
        .get(model_id)
        .or_else(|| pool.get_by_registry_id(model_id))
        .map(|m| {
            (
                m.metadata.pool_id.clone(),
                m.runtime.as_ref().and_then(|r| r.model.clone()),
            )
        })
        .ok_or_else(|| error::model_not_found(model_id))?;

    let (pool_id, backend) = loaded;
    let llama_model =
        backend.ok_or_else(|| error::internal_error(format!("model {pool_id} has no backend")))?;
    drop(pool);

    let n_ctx = body.get("n_ctx").and_then(|v| v.as_u64()).unwrap_or(2048) as u32;
    let mut ctx_params = llama::bindings::llama_context_default_params();
    ctx_params.n_ctx = n_ctx;

    let mut ctx = llama_model
        .create_context(ctx_params)
        .map_err(|e| error::internal_error(e))?;

    let config = llama::GenerateConfig {
        max_tokens: body
            .get("max_tokens")
            .and_then(|v| v.as_i64())
            .unwrap_or(512) as i32,
        temperature: body
            .get("temperature")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.7) as f32,
        top_p: body.get("top_p").and_then(|v| v.as_f64()).unwrap_or(0.9) as f32,
        repeat_penalty: body
            .get("repeat_penalty")
            .and_then(|v| v.as_f64())
            .unwrap_or(1.1) as f32,
        frequency_penalty: body
            .get("frequency_penalty")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0) as f32,
        presence_penalty: body
            .get("presence_penalty")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0) as f32,
    };

    let result = ctx
        .generate(prompt, &config)
        .map_err(|e| error::internal_error(e))?;

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

async fn ai_complete(
    State(state): State<AiState>,
    body: Option<Json<Value>>,
) -> Result<impl IntoResponse, AiError> {
    let body = body.ok_or_else(|| error::bad_request("body required"))?;

    let prompt = body.0.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
    let model_id = body
        .0
        .get("model_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if prompt.is_empty() {
        return Err(error::bad_request("prompt is required"));
    }
    if model_id.is_empty() {
        return Err(error::bad_request("model_id is required"));
    }

    #[cfg(feature = "llama")]
    {
        return ai_generate_real(state, body.0, prompt, model_id).await;
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
            "fim": false,
            "embeddings": false,
            "tool_calling": false,
            "streaming": cfg!(feature = "llama"),
            "model_inspection": true,
            "gguf_parsing": true,
            "metadata_extraction": true,
            "architecture_detection": true,
            "memory_estimation": true,
            "capability_detection": true,
            "llama_backend": cfg!(feature = "llama")
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

    // Wait for first message with generation params
    let msg = match receiver.next().await {
        Some(Ok(Message::Text(text))) => text,
        _ => {
            let _ = sender
                .send(Message::Text(
                    serde_json::to_string(
                        &json!({"type": "error", "data": {"message": "expected text message"}}),
                    )
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
                    serde_json::to_string(
                        &json!({"type": "error", "data": {"message": "invalid JSON"}}),
                    )
                    .unwrap()
                    .into(),
                ))
                .await;
            return;
        }
    };

    let prompt = params.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
    let model_id = params
        .get("model_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let session_id = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if prompt.is_empty() {
        let _ = sender
            .send(Message::Text(
                serde_json::to_string(
                    &json!({"type": "error", "data": {"message": "prompt is required"}}),
                )
                .unwrap()
                .into(),
            ))
            .await;
        return;
    }

    // Register cancel token if session_id provided
    if !session_id.is_empty() {
        state
            .cancel_tokens
            .write()
            .await
            .insert(session_id.to_string(), cancel.clone());
    }

    #[cfg(feature = "llama")]
    {
        let result = handle_generate_stream_llama(
            &mut sender,
            &mut receiver,
            state,
            &params,
            prompt,
            model_id,
            cancel.clone(),
        )
        .await;
        if let Err(e) = result {
            let _ = sender
                .send(Message::Text(
                    serde_json::to_string(&json!({"type": "error", "data": {"message": e}}))
                        .unwrap()
                        .into(),
                ))
                .await;
        }
    }

    #[cfg(not(feature = "llama"))]
    {
        let _ = (state, model_id);
        let done = json!({
            "type": "done",
            "data": {
                "text": "",
                "usage": { "prompt_tokens": 0, "completion_tokens": 0 }
            }
        });
        let _ = sender
            .send(Message::Text(serde_json::to_string(&done).unwrap().into()))
            .await;
    }

    // Cleanup cancel token
    if !session_id.is_empty() {
        state.cancel_tokens.write().await.remove(session_id);
    }

    let _ = sender.send(Message::Close(None)).await;
    while let Some(msg) = receiver.next().await {
        if matches!(msg, Ok(Message::Close(_)) | Err(_)) {
            break;
        }
    }
}

#[cfg(feature = "llama")]
async fn handle_generate_stream_llama(
    sender: &mut futures::stream::SplitSink<WebSocket, Message>,
    receiver: &mut futures::stream::SplitStream<WebSocket>,
    state: AiState,
    params: &Value,
    prompt: &str,
    model_id: &str,
    cancel: Arc<AtomicBool>,
) -> Result<(), String> {
    let pool = state.model_pool.read().await;
    let loaded = pool
        .get(model_id)
        .or_else(|| pool.get_by_registry_id(model_id))
        .map(|m| (m.runtime.as_ref().and_then(|r| r.model.clone())))
        .ok_or_else(|| format!("model not found: {model_id}"))?;

    let llama_model = loaded.ok_or_else(|| format!("model {model_id} has no backend"))?;
    drop(pool);

    let n_ctx = params.get("n_ctx").and_then(|v| v.as_u64()).unwrap_or(2048) as u32;
    let mut ctx_params = llama::bindings::llama_context_default_params();
    ctx_params.n_ctx = n_ctx;

    let mut ctx = llama_model
        .create_context(ctx_params)
        .map_err(|e| format!("failed to create context: {e}"))?;

    let config = llama::GenerateConfig {
        max_tokens: params
            .get("max_tokens")
            .and_then(|v| v.as_i64())
            .unwrap_or(512) as i32,
        temperature: params
            .get("temperature")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.7) as f32,
        top_p: params.get("top_p").and_then(|v| v.as_f64()).unwrap_or(0.9) as f32,
        repeat_penalty: params
            .get("repeat_penalty")
            .and_then(|v| v.as_f64())
            .unwrap_or(1.1) as f32,
        frequency_penalty: params
            .get("frequency_penalty")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0) as f32,
        presence_penalty: params
            .get("presence_penalty")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0) as f32,
    };

    // Tokenize prompt first to get token count for streaming
    let prompt_tokens = ctx
        .tokenize(prompt, true)
        .map_err(|e| format!("tokenize failed: {e}"))?;
    let prompt_token_count = prompt_tokens.len() as i32;

    // Run generation with cancel checking
    let result = ctx
        .generate_with_cancel(prompt, &config, &cancel)
        .map_err(|e| format!("generation failed: {e}"))?;

    // Build the completed output with all tokens
    let done = json!({
        "type": "done",
        "data": {
            "text": result.text,
            "usage": {
                "prompt_tokens": prompt_token_count,
                "completion_tokens": result.completion_tokens
            }
        }
    });

    let _ = sender
        .send(Message::Text(serde_json::to_string(&done).unwrap().into()))
        .await;

    Ok(())
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
            // Without llama backend, generate returns stub (OK)
            assert_eq!(response.status(), StatusCode::OK);
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
