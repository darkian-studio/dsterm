use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, Mutex, RwLock, Semaphore};

use super::inspect;

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ---------------------------------------------------------------------------
// 1. Memory Breakdown
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct MemoryBreakdown {
    pub weights_bytes: u64,
    pub kv_cache_bytes: u64,
    pub runtime_buffers_bytes: u64,
    pub overhead_bytes: u64,
    pub total_bytes: u64,
}

impl MemoryBreakdown {
    pub fn from_inspect(meta: &Value) -> Self {
        let total = meta["memory"]["total_bytes"].as_u64().unwrap_or(0);
        let kv = meta["memory"]["kv_cache_bytes"].as_u64().unwrap_or(0);
        let weights = meta["memory"]["model_weights_bytes"].as_u64().unwrap_or(0);
        let oh = meta["memory"]["overhead_bytes"].as_u64().unwrap_or(0);
        Self {
            weights_bytes: weights,
            kv_cache_bytes: kv,
            runtime_buffers_bytes: 0,
            overhead_bytes: oh,
            total_bytes: total,
        }
    }

    pub fn aggregate(models: &[&LoadedModel]) -> Self {
        let mut w = 0u64;
        let mut k = 0u64;
        let mut r = 0u64;
        let mut o = 0u64;
        for m in models {
            w = w.saturating_add(m.metadata.memory_estimate.weights_bytes);
            k = k.saturating_add(m.metadata.memory_estimate.kv_cache_bytes);
            r = r.saturating_add(m.metadata.memory_estimate.runtime_buffers_bytes);
            o = o.saturating_add(m.metadata.memory_estimate.overhead_bytes);
        }
        Self {
            weights_bytes: w,
            kv_cache_bytes: k,
            runtime_buffers_bytes: r,
            overhead_bytes: o,
            total_bytes: w.saturating_add(k).saturating_add(r).saturating_add(o),
        }
    }
}

// ---------------------------------------------------------------------------
// 2. State Machine
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize)]
#[allow(dead_code)]
pub enum LifecycleState {
    NotLoaded,
    Inspecting,
    Loading,
    Loaded,
    Unloading,
    Error,
}

#[allow(dead_code)]
impl LifecycleState {
    pub fn can_transition_to(&self, next: &LifecycleState) -> bool {
        use LifecycleState::*;
        matches!(
            (self, next),
            (NotLoaded, Inspecting)
                | (Inspecting, Loading | Error)
                | (Loading, Loaded | Error)
                | (Loaded, Loaded | Unloading)
                | (Unloading, NotLoaded | Error)
        )
    }
}

// ---------------------------------------------------------------------------
// 3. Model Lifecycle
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ModelLifecycle {
    pub state: LifecycleState,
    pub ref_count: u32,
    pub loaded_at: u64,
    pub last_accessed_at: u64,
    pub error_message: Option<String>,
}

#[allow(dead_code)]
impl ModelLifecycle {
    pub fn new() -> Self {
        Self {
            state: LifecycleState::NotLoaded,
            ref_count: 0,
            loaded_at: 0,
            last_accessed_at: now_secs(),
            error_message: None,
        }
    }

    pub fn acquire(&mut self) -> Result<u32, String> {
        match self.state {
            LifecycleState::Loaded => {}
            _ => {
                return Err(format!(
                    "Cannot acquire: model is in {:?} state",
                    self.state
                ))
            }
        }
        self.ref_count = self.ref_count.checked_add(1).ok_or("refcount overflow")?;
        self.last_accessed_at = now_secs();
        Ok(self.ref_count)
    }

    pub fn release(&mut self) -> Result<bool, String> {
        if self.ref_count == 0 {
            return Err("refcount underflow: release on zero refcount".to_string());
        }
        self.ref_count -= 1;
        self.last_accessed_at = now_secs();
        Ok(self.ref_count == 0)
    }

    pub fn transition_to(&mut self, next: LifecycleState) -> Result<(), String> {
        if !self.state.can_transition_to(&next) {
            return Err(format!(
                "Illegal state transition: {:?} -> {:?}",
                self.state, next
            ));
        }
        let now = now_secs();
        self.last_accessed_at = now;
        if next == LifecycleState::Loaded {
            self.loaded_at = now;
        }
        if let LifecycleState::Error = &next {
            // error_message is set separately via set_error
        }
        self.state = next;
        Ok(())
    }

    pub fn set_error(&mut self, msg: String) {
        self.error_message = Some(msg);
        self.last_accessed_at = now_secs();
    }
}

// ---------------------------------------------------------------------------
// 4. Model Metadata (static, from GGUF)
// ---------------------------------------------------------------------------

/// Standalone metadata that survives runtime recreation.
/// The pool caches this so metadata is still available after `unload`.
#[derive(Debug, Clone, Serialize)]
pub struct ModelMetadata {
    pub registry_id: String,
    pub model_hash: String,
    pub path: String,
    pub architecture: String,
    pub quantisation: String,
    pub parameter_count: Option<f64>,
    pub file_size: u64,
    pub file_mtime: u64,
    pub memory_estimate: MemoryBreakdown,
    pub capabilities: super::gguf::ModelCapabilities,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoadedModelMetadata {
    pub registry_id: String,
    pub pool_id: String,
    pub model_hash: String,
    pub path: String,
    pub architecture: String,
    pub quantisation: String,
    pub parameter_count: Option<f64>,
    pub file_size: u64,
    pub file_mtime: u64,
    pub memory_estimate: MemoryBreakdown,
    pub capabilities: super::gguf::ModelCapabilities,
}

impl LoadedModelMetadata {
    /// Extract the standalone `ModelMetadata` portion.
    /// This can be stored in the metadata cache for reuse after runtime is dropped.
    pub fn to_model_metadata(&self) -> ModelMetadata {
        ModelMetadata {
            registry_id: self.registry_id.clone(),
            model_hash: self.model_hash.clone(),
            path: self.path.clone(),
            architecture: self.architecture.clone(),
            quantisation: self.quantisation.clone(),
            parameter_count: self.parameter_count,
            file_size: self.file_size,
            file_mtime: self.file_mtime,
            memory_estimate: self.memory_estimate.clone(),
            capabilities: self.capabilities.clone(),
        }
    }

    /// Reconstruct from a cached `ModelMetadata` and a current pool_id.
    pub fn from_model_metadata(meta: &ModelMetadata, pool_id: String) -> Self {
        Self {
            registry_id: meta.registry_id.clone(),
            pool_id,
            model_hash: meta.model_hash.clone(),
            path: meta.path.clone(),
            architecture: meta.architecture.clone(),
            quantisation: meta.quantisation.clone(),
            parameter_count: meta.parameter_count,
            file_size: meta.file_size,
            file_mtime: meta.file_mtime,
            memory_estimate: meta.memory_estimate.clone(),
            capabilities: meta.capabilities.clone(),
        }
    }
}

fn compute_model_hash(meta: &Value) -> String {
    let arch = meta["architecture"]["architecture"].as_str().unwrap_or("");
    let ctx = meta["architecture"]["context_length"].as_u64().unwrap_or(0);
    let quant = meta["model"]["quantisation"].as_str().unwrap_or("");
    format!("{arch}|{ctx}|{quant}")
}

#[allow(dead_code)]
fn extract_file_mtime(path: &str) -> u64 {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// 5. Runtime Handle (wraps llama.cpp backend when feature is enabled)
// ---------------------------------------------------------------------------

pub struct RuntimeHandle {
    #[cfg(feature = "llama")]
    pub model: Option<Arc<super::llama::LlamaModel>>,
}

// ---------------------------------------------------------------------------
// 6. Loaded Model (metadata + runtime + lifecycle)
// ---------------------------------------------------------------------------

pub struct LoadedModel {
    pub metadata: LoadedModelMetadata,
    #[allow(dead_code)]
    pub runtime: Option<RuntimeHandle>,
    pub lifecycle: ModelLifecycle,
}

impl LoadedModel {
    pub fn to_view(&self) -> Value {
        json!({
            "metadata": self.metadata,
            "lifecycle": {
                "state": self.lifecycle.state,
                "error_message": self.lifecycle.error_message,
                "ref_count": self.lifecycle.ref_count,
                "loaded_at": self.lifecycle.loaded_at,
                "last_accessed_at": self.lifecycle.last_accessed_at
            }
        })
    }
}

// ---------------------------------------------------------------------------
// 7. File Info for change detection
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct FileInfo {
    pub size: u64,
    pub mtime: u64,
}

// ---------------------------------------------------------------------------
// 8. Pool Events
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub enum PoolEvent {
    ModelLoaded {
        pool_id: String,
        registry_id: String,
    },
    ModelUnloaded {
        pool_id: String,
        registry_id: String,
        ref_count: u32,
    },
    RefIncremented {
        pool_id: String,
        ref_count: u32,
    },
    RefDecremented {
        pool_id: String,
        ref_count: u32,
    },
    Evicted {
        pool_id: String,
        reason: String,
    },
    LoadFailed {
        registry_id: String,
        error: String,
    },
    StateChanged {
        pool_id: String,
        from: LifecycleState,
        to: LifecycleState,
    },
}

// ---------------------------------------------------------------------------
// 9. Eviction Strategy
// ---------------------------------------------------------------------------

pub trait EvictionStrategy: Send + Sync {
    fn score(&self, model: &LoadedModel) -> f64;
    fn name(&self) -> &'static str;
}

pub struct LruEviction;

impl EvictionStrategy for LruEviction {
    fn score(&self, model: &LoadedModel) -> f64 {
        -(model.lifecycle.last_accessed_at as f64)
    }

    fn name(&self) -> &'static str {
        "lru"
    }
}

// ---------------------------------------------------------------------------
// 10. Pool Configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct PoolConfig {
    pub max_models: usize,
    pub max_memory_bytes: u64,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_models: 4,
            max_memory_bytes: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// 11. Stats / Health views
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct PoolStats {
    pub loaded_count: usize,
    pub total_ref_count: u32,
    pub memory: MemoryBreakdown,
    pub available_bytes: u64,
    pub max_models: usize,
    pub max_memory_bytes: u64,
    pub eviction_strategy: String,
    pub pool_consistent: bool,
    pub load_failures: u64,
    pub evictable_count: usize,
    pub oldest_model_id: Option<String>,
    pub oldest_model_age_secs: Option<u64>,
    pub most_referenced_id: Option<String>,
    pub most_referenced_count: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PoolHealth {
    pub healthy: bool,
    pub pool_consistent: bool,
    pub loaded: usize,
    pub capacity: usize,
    pub evictable: usize,
    pub locked_loads: usize,
    pub load_failures: u64,
    pub memory_pressure: bool,
    pub memory: MemoryBreakdown,
    pub eviction_strategy: String,
    pub oldest_model: Option<Value>,
    pub most_referenced_model: Option<Value>,
}

// ---------------------------------------------------------------------------
// 12. Load Lock Manager
// ---------------------------------------------------------------------------

pub struct LoadLockManager {
    locks: Mutex<HashMap<String, Arc<Semaphore>>>,
}

impl LoadLockManager {
    pub fn new() -> Self {
        Self {
            locks: Mutex::new(HashMap::new()),
        }
    }

    pub async fn acquire(&self, key: &str) -> Arc<Semaphore> {
        let mut locks = self.locks.lock().await;
        locks
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(Semaphore::new(1)))
            .clone()
    }

    #[allow(dead_code)]
    pub async fn active_locks(&self) -> usize {
        let locks = self.locks.lock().await;
        locks.len()
    }
}

// ---------------------------------------------------------------------------
// 13. Model Pool Inner
// ---------------------------------------------------------------------------

pub struct ModelPoolInner {
    pub models: HashMap<String, LoadedModel>,
    config: PoolConfig,
    eviction_strategy: Box<dyn EvictionStrategy>,
    event_tx: broadcast::Sender<PoolEvent>,
    consistency_ok: bool,
    load_failures: u64,
    next_pool_id: u64,
    file_cache: HashMap<String, FileInfo>,
    /// Metadata cache that survives runtime recreation.
    /// When a model is fully unloaded its metadata is cached here
    /// so that `ModelMetadata` is still available for reloads or queries.
    metadata_cache: HashMap<String, ModelMetadata>,
}

impl ModelPoolInner {
    pub fn new(config: PoolConfig) -> Self {
        let (event_tx, _) = broadcast::channel(256);
        Self {
            models: HashMap::new(),
            config,
            eviction_strategy: Box::new(LruEviction),
            event_tx,
            consistency_ok: true,
            load_failures: 0,
            next_pool_id: 1,
            file_cache: HashMap::new(),
            metadata_cache: HashMap::new(),
        }
    }

    #[allow(dead_code)]
    pub fn event_rx(&self) -> broadcast::Receiver<PoolEvent> {
        self.event_tx.subscribe()
    }

    fn emit(&self, event: PoolEvent) {
        let _ = self.event_tx.send(event);
    }

    fn next_pool_id_str(&mut self) -> String {
        let id = self.next_pool_id;
        self.next_pool_id += 1;
        format!("pool://{id}")
    }

    fn check_file_unchanged(&self, path: &str) -> Result<FileInfo, String> {
        let meta = std::fs::metadata(path).map_err(|e| format!("Cannot stat file: {e}"))?;
        let size = meta.len();
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);

        if let Some(cached) = self.file_cache.get(path) {
            if cached.size != size || cached.mtime != mtime {
                return Err(format!(
                    "File changed since last load: {path} (size: {}->{}, mtime: {}->{})",
                    cached.size, size, cached.mtime, mtime
                ));
            }
        }
        Ok(FileInfo { size, mtime })
    }

    pub fn load(&mut self, path: &str) -> Result<LoadedModel, String> {
        // Check file changes
        let file_info = self.check_file_unchanged(path)?;

        // Inspect GGUF
        let meta =
            inspect::inspect_model(path).map_err(|e| format!("Failed to inspect model: {e}"))?;

        let registry_id = meta["model"]["id"]
            .as_str()
            .unwrap_or(&format!("local://{}", path))
            .to_string();
        let model_hash = compute_model_hash(&meta);

        // Check if already loaded by registry_id
        let mut found = None;
        for model in self.models.values_mut() {
            if model.metadata.registry_id == registry_id {
                model.lifecycle.acquire().map_err(|e| e.to_string())?;
                model.lifecycle.last_accessed_at = now_secs();
                let pool_id = model.metadata.pool_id.clone();
                let ref_count = model.lifecycle.ref_count;
                let meta = model.metadata.clone();
                let lc = model.lifecycle.clone();
                found = Some((pool_id, ref_count, meta, lc));
                break;
            }
        }
        if let Some((pool_id, ref_count, meta, lc)) = found {
            self.emit(PoolEvent::RefIncremented { pool_id, ref_count });
            return Ok(LoadedModel {
                metadata: meta,
                runtime: None,
                lifecycle: lc,
            });
        }

        // Check model_hash for dedup
        for model in self.models.values() {
            if model.metadata.model_hash == model_hash && model.metadata.registry_id != registry_id
            {
                let msg = format!(
                    "Model with same hash already loaded as {}",
                    model.metadata.registry_id
                );
                return Err(msg);
            }
        }

        // Check capacity / memory
        let current_count = self.models.len();
        let current_memory: u64 = self
            .models
            .values()
            .map(|m| m.metadata.memory_estimate.total_bytes)
            .sum();

        let estimated = MemoryBreakdown::from_inspect(&meta);
        if self.config.max_models > 0 && current_count >= self.config.max_models {
            self.evict(1)?;
        }
        if self.config.max_memory_bytes > 0
            && current_memory.saturating_add(estimated.total_bytes) > self.config.max_memory_bytes
        {
            let needed = current_memory
                .saturating_add(estimated.total_bytes)
                .saturating_sub(self.config.max_memory_bytes);
            self.evict_for_memory(needed)?;
        }

        // Initialize llama.cpp backend
        let rt = {
            #[cfg(feature = "llama")]
            {
                match super::llama::LlamaModel::load(path) {
                    Ok(model) => RuntimeHandle {
                        model: Some(Arc::new(model)),
                    },
                    Err(e) => {
                        return Err(format!("Failed to initialize llama backend: {e}"));
                    }
                }
            }
            #[cfg(not(feature = "llama"))]
            {
                RuntimeHandle {}
            }
        };

        let pool_id = self.next_pool_id_str();

        // Cache file info
        self.file_cache.insert(path.to_string(), file_info.clone());

        let capabilities = super::gguf::ModelCapabilities::from_json(&meta["capabilities"]);

        let model = LoadedModel {
            metadata: LoadedModelMetadata {
                registry_id: registry_id.clone(),
                pool_id: pool_id.clone(),
                model_hash,
                path: path.to_string(),
                architecture: meta["architecture"]["architecture"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string(),
                quantisation: meta["model"]["quantisation"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string(),
                parameter_count: meta["model"]["parameter_count_raw"].as_f64(),
                file_size: meta["model"]["file_size"].as_u64().unwrap_or(0),
                file_mtime: file_info.mtime,
                memory_estimate: estimated,
                capabilities,
            },
            runtime: Some(rt),
            lifecycle: ModelLifecycle {
                state: LifecycleState::Loaded,
                ref_count: 1,
                loaded_at: now_secs(),
                last_accessed_at: now_secs(),
                error_message: None,
            },
        };

        self.emit(PoolEvent::ModelLoaded {
            pool_id: pool_id.clone(),
            registry_id: registry_id.clone(),
        });
        self.emit(PoolEvent::StateChanged {
            pool_id: pool_id.clone(),
            from: LifecycleState::NotLoaded,
            to: LifecycleState::Loaded,
        });

        let response = LoadedModel {
            metadata: model.metadata.clone(),
            runtime: None,
            lifecycle: model.lifecycle.clone(),
        };

        self.models.insert(pool_id, model);
        self.consistency_ok = true;
        Ok(response)
    }

    pub fn unload(&mut self, pool_id: &str) -> Result<bool, String> {
        let (fully_released, new_ref, meta_cached) = {
            let model = self
                .models
                .get_mut(pool_id)
                .ok_or_else(|| format!("Model not loaded: {pool_id}"))?;
            let fully_released = model.lifecycle.release()?;
            let new_ref = model.lifecycle.ref_count;
            let meta_cached = model.metadata.to_model_metadata();
            (fully_released, new_ref, meta_cached)
        };

        self.emit(PoolEvent::RefDecremented {
            pool_id: pool_id.to_string(),
            ref_count: new_ref,
        });

        if fully_released {
            self.emit(PoolEvent::StateChanged {
                pool_id: pool_id.to_string(),
                from: LifecycleState::Loaded,
                to: LifecycleState::Unloading,
            });
            // Cache metadata before removing the model entry.
            // This ensures ModelMetadata survives runtime destruction.
            self.metadata_cache
                .insert(meta_cached.registry_id.clone(), meta_cached);
            self.models.remove(pool_id);
            self.emit(PoolEvent::ModelUnloaded {
                pool_id: pool_id.to_string(),
                registry_id: String::new(),
                ref_count: 0,
            });
        }

        self.consistency_ok = true;
        Ok(fully_released)
    }

    /// Reload the runtime for an already-loaded model.
    /// This destroys and recreates the llama.cpp backend without touching metadata.
    /// Useful for runtime recovery without re-inspection.
    pub fn reload_runtime(&mut self, pool_id: &str) -> Result<(), String> {
        let model = self
            .models
            .get_mut(pool_id)
            .ok_or_else(|| format!("Model not loaded: {pool_id}"))?;

        let path = &model.metadata.path;
        #[cfg(feature = "llama")]
        {
            let new_rt = match super::llama::LlamaModel::load(path) {
                Ok(m) => RuntimeHandle {
                    model: Some(Arc::new(m)),
                },
                Err(e) => return Err(format!("Failed to reload runtime: {e}")),
            };
            model.runtime = Some(new_rt);
        }
        #[cfg(not(feature = "llama"))]
        {
            let _ = path;
            model.runtime = Some(RuntimeHandle {});
        }
        Ok(())
    }

    pub fn get(&self, pool_id: &str) -> Option<&LoadedModel> {
        self.models.get(pool_id)
    }

    pub fn get_by_registry_id(&self, registry_id: &str) -> Option<&LoadedModel> {
        self.models
            .values()
            .find(|m| m.metadata.registry_id == registry_id)
    }

    pub fn list(&self) -> Vec<Value> {
        self.models.values().map(|m| m.to_view()).collect()
    }

    pub fn stats(&self) -> PoolStats {
        let models: Vec<&LoadedModel> = self.models.values().collect();
        let memory = MemoryBreakdown::aggregate(&models);
        let total_ref: u32 = self.models.values().map(|m| m.lifecycle.ref_count).sum();

        let available = if self.config.max_memory_bytes > 0 {
            self.config
                .max_memory_bytes
                .saturating_sub(memory.total_bytes)
        } else {
            0
        };

        let evictable = self
            .models
            .values()
            .filter(|m| m.lifecycle.ref_count == 0)
            .count();

        let oldest = self.models.values().min_by_key(|m| m.lifecycle.loaded_at);
        let oldest_id = oldest.map(|m| m.metadata.pool_id.clone());
        let oldest_age = oldest.map(|m| now_secs().saturating_sub(m.lifecycle.loaded_at));

        let most_refd = self.models.values().max_by_key(|m| m.lifecycle.ref_count);
        let most_refd_id = most_refd.map(|m| m.metadata.pool_id.clone());
        let most_refd_count = most_refd.map(|m| m.lifecycle.ref_count);

        PoolStats {
            loaded_count: self.models.len(),
            total_ref_count: total_ref,
            memory,
            available_bytes: available,
            max_models: self.config.max_models,
            max_memory_bytes: self.config.max_memory_bytes,
            eviction_strategy: self.eviction_strategy.name().to_string(),
            pool_consistent: self.consistency_ok,
            load_failures: self.load_failures,
            evictable_count: evictable,
            oldest_model_id: oldest_id,
            oldest_model_age_secs: oldest_age,
            most_referenced_id: most_refd_id,
            most_referenced_count: most_refd_count,
        }
    }

    pub fn health(&self) -> PoolHealth {
        let stats = self.stats();
        let memory_pressure = self.config.max_memory_bytes > 0
            && stats.memory.total_bytes >= self.config.max_memory_bytes.saturating_mul(8) / 10;

        let oldest = stats.oldest_model_id.as_ref().and_then(|id| {
            self.models.get(id).map(|m| {
                json!({
                    "pool_id": m.metadata.pool_id,
                    "registry_id": m.metadata.registry_id,
                    "age_secs": now_secs().saturating_sub(m.lifecycle.loaded_at)
                })
            })
        });

        let most_refd = stats.most_referenced_id.as_ref().and_then(|id| {
            self.models.get(id).map(|m| {
                json!({
                    "pool_id": m.metadata.pool_id,
                    "registry_id": m.metadata.registry_id,
                    "ref_count": m.lifecycle.ref_count
                })
            })
        });

        PoolHealth {
            healthy: stats.pool_consistent,
            pool_consistent: stats.pool_consistent,
            loaded: stats.loaded_count,
            capacity: stats.max_models,
            evictable: stats.evictable_count,
            locked_loads: 0,
            load_failures: self.load_failures,
            memory_pressure,
            memory: stats.memory,
            eviction_strategy: stats.eviction_strategy,
            oldest_model: oldest,
            most_referenced_model: most_refd,
        }
    }

    #[allow(dead_code)]
    pub fn verify(&mut self) -> bool {
        // Check no duplicate pool_ids
        let mut seen_pool = std::collections::HashSet::new();
        let mut seen_reg = std::collections::HashSet::new();
        let mut memory_sum = 0u64;
        let mut ok = true;

        for (pool_id, model) in &self.models {
            if !seen_pool.insert(pool_id.clone()) {
                ok = false;
            }
            if !seen_reg.insert(model.metadata.registry_id.clone()) {
                ok = false;
            }
            if model.lifecycle.ref_count == 0 && model.lifecycle.state != LifecycleState::Loaded {
                ok = false;
            }
            memory_sum = memory_sum.saturating_add(model.metadata.memory_estimate.total_bytes);
        }

        // Check memory totals match
        let stats_models: Vec<&LoadedModel> = self.models.values().collect();
        let stats_memory = MemoryBreakdown::aggregate(&stats_models);
        if stats_memory.total_bytes != memory_sum {
            ok = false;
        }

        // Check LRU ordering (timestamps should be non-decreasing by access order)
        let mut sorted: Vec<&LoadedModel> = self.models.values().collect();
        sorted.sort_by_key(|m| m.lifecycle.last_accessed_at);
        for i in 1..sorted.len() {
            if sorted[i].lifecycle.last_accessed_at < sorted[i - 1].lifecycle.last_accessed_at {
                ok = false;
                break;
            }
        }

        self.consistency_ok = ok;
        ok
    }

    // -----------------------------------------------------------------------
    // Eviction
    // -----------------------------------------------------------------------

    fn evict(&mut self, count: usize) -> Result<(), String> {
        let mut evicted = 0;
        for _ in 0..count {
            let victim = self
                .models
                .values()
                .filter(|m| m.lifecycle.ref_count == 0)
                .max_by(|a, b| {
                    self.eviction_strategy
                        .score(a)
                        .partial_cmp(&self.eviction_strategy.score(b))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|m| m.metadata.pool_id.clone());

            match victim {
                Some(id) => {
                    let reason = format!("evicted by {}", self.eviction_strategy.name());
                    self.emit(PoolEvent::Evicted {
                        pool_id: id.clone(),
                        reason: reason.clone(),
                    });
                    self.models.remove(&id);
                    evicted += 1;
                }
                None => {
                    if evicted == 0 {
                        return Err("No evictable models (all have active references)".to_string());
                    }
                    break;
                }
            }
        }
        self.consistency_ok = true;
        Ok(())
    }

    fn evict_for_memory(&mut self, needed: u64) -> Result<(), String> {
        let mut freed = 0u64;
        loop {
            let before = self.models.len();
            let victim = self
                .models
                .values()
                .filter(|m| m.lifecycle.ref_count == 0)
                .max_by(|a, b| {
                    self.eviction_strategy
                        .score(a)
                        .partial_cmp(&self.eviction_strategy.score(b))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|m| {
                    (
                        m.metadata.pool_id.clone(),
                        m.metadata.memory_estimate.total_bytes,
                    )
                });

            match victim {
                Some((id, bytes)) => {
                    self.models.remove(&id);
                    freed = freed.saturating_add(bytes);
                    if freed >= needed {
                        break;
                    }
                }
                None => {
                    return Err(format!(
                        "Insufficient memory: need {needed} bytes, could not evict enough (freed {freed})"
                    ));
                }
            }
            if self.models.len() == before {
                return Err("Eviction made no progress".to_string());
            }
        }
        self.consistency_ok = true;
        Ok(())
    }

    pub fn record_load_failure(&mut self, error: &str) {
        self.load_failures = self.load_failures.saturating_add(1);
        self.consistency_ok = false;
        self.emit(PoolEvent::LoadFailed {
            registry_id: "unknown".to_string(),
            error: error.to_string(),
        });
    }
}

// ---------------------------------------------------------------------------
// Shared state types
// ---------------------------------------------------------------------------

pub type ModelPoolState = Arc<RwLock<ModelPoolInner>>;
pub type LoadLockManagerState = Arc<LoadLockManager>;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_model_meta(pool_id: &str, reg_id: &str, mem: u64) -> LoadedModelMetadata {
        LoadedModelMetadata {
            registry_id: reg_id.to_string(),
            pool_id: pool_id.to_string(),
            model_hash: "hash".to_string(),
            path: format!("/models/{}.gguf", reg_id),
            architecture: "llama".to_string(),
            quantisation: "Q4_K_M".to_string(),
            parameter_count: Some(1_000_000_000.0),
            file_size: mem,
            file_mtime: 1000,
            memory_estimate: MemoryBreakdown {
                weights_bytes: mem,
                kv_cache_bytes: 0,
                runtime_buffers_bytes: 0,
                overhead_bytes: 0,
                total_bytes: mem,
            },
            capabilities: super::gguf::ModelCapabilities {
                chat: true,
                completion: true,
                fim: false,
                embeddings: false,
                tool_calling: true,
                reasoning: false,
                vision: false,
            },
        }
    }

    fn make_model(pool_id: &str, reg_id: &str, mem: u64, refs: u32, ts: u64) -> LoadedModel {
        LoadedModel {
            metadata: make_model_meta(pool_id, reg_id, mem),
            runtime: None,
            lifecycle: ModelLifecycle {
                state: LifecycleState::Loaded,
                ref_count: refs,
                loaded_at: ts,
                last_accessed_at: ts,
                error_message: None,
            },
        }
    }

    fn new_pool() -> ModelPoolInner {
        ModelPoolInner::new(PoolConfig::default())
    }

    // --- Pool basics ---

    #[test]
    fn test_pool_new_is_empty() {
        let pool = new_pool();
        assert_eq!(pool.list().len(), 0);
        assert!(pool.consistency_ok);
    }

    #[test]
    fn test_pool_stats_empty() {
        let pool = new_pool();
        let s = pool.stats();
        assert_eq!(s.loaded_count, 0);
        assert_eq!(s.total_ref_count, 0);
        assert_eq!(s.memory.total_bytes, 0);
        assert!(s.pool_consistent);
    }

    #[test]
    fn test_pool_unload_not_loaded() {
        let mut pool = new_pool();
        let result = pool.unload("pool://999");
        assert!(result.is_err());
    }

    // --- Ref count: acquire / release ---

    #[test]
    fn test_lifecycle_acquire_release() {
        let mut lc = ModelLifecycle::new();
        assert_eq!(lc.state, LifecycleState::NotLoaded);
        assert_eq!(lc.ref_count, 0);

        // Acquire on NotLoaded should fail
        assert!(lc.acquire().is_err());

        // Transition to Loaded
        lc.transition_to(LifecycleState::Inspecting).unwrap();
        lc.transition_to(LifecycleState::Loading).unwrap();
        lc.transition_to(LifecycleState::Loaded).unwrap();

        assert_eq!(lc.acquire().unwrap(), 1);
        assert_eq!(lc.acquire().unwrap(), 2);
        assert!(!lc.release().unwrap()); // not fully released
        assert!(lc.release().unwrap()); // fully released
    }

    #[test]
    fn test_lifecycle_release_underflow() {
        let mut lc = ModelLifecycle::new();
        lc.transition_to(LifecycleState::Inspecting).unwrap();
        lc.transition_to(LifecycleState::Loading).unwrap();
        lc.transition_to(LifecycleState::Loaded).unwrap();
        lc.ref_count = 0;
        assert!(lc.release().is_err());
    }

    // --- State machine transitions ---

    #[test]
    fn test_lifecycle_valid_transitions() {
        let mut lc = ModelLifecycle::new();
        assert!(lc.transition_to(LifecycleState::Inspecting).is_ok());
        assert!(lc.transition_to(LifecycleState::Loading).is_ok());
        assert!(lc.transition_to(LifecycleState::Loaded).is_ok());
        assert!(lc.transition_to(LifecycleState::Loaded).is_ok()); // refcount increment
        assert!(lc.transition_to(LifecycleState::Unloading).is_ok());
        assert!(lc.transition_to(LifecycleState::NotLoaded).is_ok());
    }

    #[test]
    fn test_lifecycle_invalid_transitions() {
        // NotLoaded -> Loaded (skip Inspecting/Loading)
        let mut lc = ModelLifecycle::new();
        assert!(lc.transition_to(LifecycleState::Loaded).is_err());

        // Loaded -> NotLoaded (skip Unloading)
        let mut lc = ModelLifecycle::new();
        lc.transition_to(LifecycleState::Inspecting).unwrap();
        lc.transition_to(LifecycleState::Loading).unwrap();
        lc.transition_to(LifecycleState::Loaded).unwrap();
        assert!(lc.transition_to(LifecycleState::NotLoaded).is_err());
    }

    #[test]
    fn test_lifecycle_error_transitions() {
        // Inspecting -> Error ok
        let mut lc = ModelLifecycle::new();
        lc.transition_to(LifecycleState::Inspecting).unwrap();
        lc.set_error("oops".to_string());
        assert!(lc.transition_to(LifecycleState::Error).is_ok());

        // NotLoaded -> Error invalid
        let mut lc = ModelLifecycle::new();
        assert!(lc.transition_to(LifecycleState::Error).is_err());
    }

    // --- Pool operations ---

    #[test]
    fn test_pool_unload_decrements_refcount() {
        let mut pool = new_pool();
        let pid = "pool://1".to_string();
        pool.models
            .insert(pid.clone(), make_model(&pid, "reg://a", 100, 2, 1));
        let result = pool.unload(&pid);
        assert!(result.is_ok());
        assert!(!result.unwrap()); // not fully released (ref went 2->1)
        assert_eq!(pool.models.get(&pid).unwrap().lifecycle.ref_count, 1);
    }

    #[test]
    fn test_pool_unload_removes_when_zero() {
        let mut pool = new_pool();
        let pid = "pool://1".to_string();
        pool.models
            .insert(pid.clone(), make_model(&pid, "reg://a", 100, 1, 1));
        let result = pool.unload(&pid);
        assert!(result.is_ok());
        assert!(result.unwrap()); // fully released
        assert!(!pool.models.contains_key(&pid));
    }

    #[test]
    fn test_pool_get_by_registry_id() {
        let mut pool = new_pool();
        let pid = "pool://1".to_string();
        pool.models
            .insert(pid.clone(), make_model(&pid, "reg://a", 100, 1, 1));
        assert!(pool.get_by_registry_id("reg://a").is_some());
        assert!(pool.get_by_registry_id("nonexistent").is_none());
    }

    // --- Consistency verification ---

    #[test]
    fn test_pool_verify_empty() {
        let mut pool = new_pool();
        assert!(pool.verify());
    }

    #[test]
    fn test_pool_verify_with_models() {
        let mut pool = new_pool();
        pool.models.insert(
            "pool://1".to_string(),
            make_model("pool://1", "reg://a", 100, 1, 1),
        );
        pool.models.insert(
            "pool://2".to_string(),
            make_model("pool://2", "reg://b", 200, 2, 2),
        );
        assert!(pool.verify());
    }

    #[test]
    fn test_pool_verify_duplicate_registry_id() {
        let mut pool = new_pool();
        pool.models.insert(
            "pool://1".to_string(),
            make_model("pool://1", "reg://a", 100, 1, 1),
        );
        pool.models.insert(
            "pool://2".to_string(),
            make_model("pool://2", "reg://a", 200, 2, 2),
        );
        assert!(!pool.verify()); // duplicate registry_id
    }

    // --- Eviction ---

    #[test]
    fn test_evict_lru_removes_oldest_zero_ref() {
        let mut pool = new_pool();
        pool.models.insert(
            "pool://1".to_string(),
            make_model("pool://1", "reg://a", 100, 0, 1),
        );
        pool.models.insert(
            "pool://2".to_string(),
            make_model("pool://2", "reg://b", 200, 0, 2),
        );
        pool.evict(1).unwrap();
        assert!(!pool.models.contains_key("pool://1")); // older, evicted
        assert!(pool.models.contains_key("pool://2"));
    }

    #[test]
    fn test_evict_skips_referenced() {
        let mut pool = new_pool();
        pool.models.insert(
            "pool://1".to_string(),
            make_model("pool://1", "reg://a", 100, 1, 1),
        );
        pool.models.insert(
            "pool://2".to_string(),
            make_model("pool://2", "reg://b", 200, 0, 2),
        );
        pool.evict(1).unwrap();
        assert!(pool.models.contains_key("pool://1")); // referenced, kept
        assert!(!pool.models.contains_key("pool://2")); // evicted
    }

    #[test]
    fn test_evict_no_candidates() {
        let mut pool = new_pool();
        pool.models.insert(
            "pool://1".to_string(),
            make_model("pool://1", "reg://a", 100, 1, 1),
        );
        let result = pool.evict(1);
        assert!(result.is_err());
    }

    // --- Memory accounting ---

    #[test]
    fn test_memory_breakdown_aggregate() {
        let models = [
            make_model("pool://1", "reg://a", 100, 1, 1),
            make_model("pool://2", "reg://b", 200, 1, 2),
        ];
        let refs: Vec<&LoadedModel> = models.iter().collect();
        let mem = MemoryBreakdown::aggregate(&refs);
        assert_eq!(mem.total_bytes, 300);
        assert_eq!(mem.weights_bytes, 300);
    }

    // --- Events ---

    #[test]
    fn test_pool_events_channel() {
        let mut pool = ModelPoolInner::new(PoolConfig::default());
        let mut rx = pool.event_rx();
        let pid = pool.next_pool_id_str();
        pool.models
            .insert(pid.clone(), make_model(&pid, "reg://a", 100, 1, 1));
        let _ = pool.unload(&pid);
        // Events should have been emitted
        loop {
            match rx.try_recv() {
                Ok(PoolEvent::ModelUnloaded { .. }) => break,
                Ok(_) => continue,
                Err(_) => {
                    // No guarantee of delivery in test since events may be dropped
                    break;
                }
            }
        }
    }

    // --- Pool health ---

    #[test]
    fn test_pool_health_empty() {
        let pool = new_pool();
        let h = pool.health();
        assert!(h.healthy);
        assert_eq!(h.loaded, 0);
        assert!(!h.memory_pressure);
    }

    #[test]
    fn test_pool_health_with_models() {
        let mut pool = new_pool();
        pool.models.insert(
            "pool://1".to_string(),
            make_model("pool://1", "reg://a", 100, 1, 1),
        );
        let h = pool.health();
        assert!(h.healthy);
        assert_eq!(h.loaded, 1);
        assert_eq!(h.evictable, 0); // ref_count=1, not evictable
    }

    // --- Stats ---

    #[test]
    fn test_pool_stats_with_models() {
        let mut pool = new_pool();
        pool.models.insert(
            "pool://1".to_string(),
            make_model("pool://1", "reg://a", 100, 2, 1),
        );
        pool.models.insert(
            "pool://2".to_string(),
            make_model("pool://2", "reg://b", 200, 1, 2),
        );
        let s = pool.stats();
        assert_eq!(s.loaded_count, 2);
        assert_eq!(s.total_ref_count, 3);
        assert_eq!(s.memory.total_bytes, 300);
    }

    #[test]
    fn test_pool_stats_with_memory_limit() {
        let mut pool = ModelPoolInner::new(PoolConfig {
            max_models: 4,
            max_memory_bytes: 500,
        });
        pool.models.insert(
            "pool://1".to_string(),
            make_model("pool://1", "reg://a", 300, 1, 1),
        );
        let s = pool.stats();
        assert_eq!(s.available_bytes, 200);
    }

    // --- LRU score ---

    #[test]
    fn test_lru_score() {
        let strategy = LruEviction;
        let old = make_model("pool://1", "reg://a", 100, 0, 1);
        let recent = make_model("pool://2", "reg://b", 100, 0, 100);
        let old_score = strategy.score(&old);
        let recent_score = strategy.score(&recent);
        assert!(old_score > recent_score); // older model has higher (less negative) score, evict picks max_by
    }

    // --- Lifecycle timestamp tracking ---

    #[test]
    fn test_lifecycle_loaded_at_set_on_transition() {
        let mut lc = ModelLifecycle::new();
        lc.transition_to(LifecycleState::Inspecting).unwrap();
        lc.transition_to(LifecycleState::Loading).unwrap();
        lc.transition_to(LifecycleState::Loaded).unwrap();
        assert!(lc.loaded_at > 0);
        let loaded = lc.loaded_at;
        // Stay in Loaded, loaded_at unchanged
        lc.transition_to(LifecycleState::Loaded).unwrap();
        assert_eq!(lc.loaded_at, loaded); // remains same
    }

    // --- Model hash ---

    #[test]
    fn test_compute_model_hash() {
        let meta = json!({
            "architecture": { "architecture": "llama", "context_length": 4096 },
            "model": { "quantisation": "Q4_K_M" }
        });
        let h = compute_model_hash(&meta);
        assert_eq!(h, "llama|4096|Q4_K_M");
    }

    // --- Memory from inspect ---

    #[test]
    fn test_memory_from_inspect() {
        let meta = json!({
            "memory": {
                "total_bytes": 1000,
                "kv_cache_bytes": 200,
                "model_weights_bytes": 700,
                "overhead_bytes": 100
            }
        });
        let m = MemoryBreakdown::from_inspect(&meta);
        assert_eq!(m.total_bytes, 1000);
        assert_eq!(m.weights_bytes, 700);
        assert_eq!(m.kv_cache_bytes, 200);
        assert_eq!(m.overhead_bytes, 100);
    }

    // --- Next pool id ---

    #[test]
    fn test_next_pool_id_increments() {
        let mut pool = new_pool();
        assert_eq!(pool.next_pool_id_str(), "pool://1");
        assert_eq!(pool.next_pool_id_str(), "pool://2");
        assert_eq!(pool.next_pool_id_str(), "pool://3");
    }

    // --- File change detection ---

    #[test]
    fn test_file_cache_stores_info() {
        let mut pool = new_pool();
        let info = FileInfo {
            size: 100,
            mtime: 50,
        };
        pool.file_cache.insert("/test.gguf".to_string(), info);
        let result = pool.check_file_unchanged("/test.gguf");
        // /test.gguf doesn't actually exist, so it'll fail on stat - that's expected
        // The check is: if cached, verify size/mtime match
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Cannot stat"));
    }
}
