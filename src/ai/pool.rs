use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use super::inspect;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadedModel {
    pub id: String,
    pub path: String,
    pub architecture: String,
    pub quantisation: String,
    pub parameter_count: Option<f64>,
    pub file_size: u64,
    pub estimated_memory_bytes: u64,
    pub loaded_at: u64,
    pub last_accessed_at: u64,
    pub ref_count: u32,
    pub status: ModelPoolStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ModelPoolStatus {
    Loading,
    Loaded,
    Unloading,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EvictionPolicy {
    Lru,
    Lfu,
}

#[derive(Debug, Clone)]
pub struct PoolConfig {
    pub max_models: usize,
    pub max_memory_bytes: u64,
    pub eviction_policy: EvictionPolicy,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_models: 4,
            max_memory_bytes: 0,
            eviction_policy: EvictionPolicy::Lru,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PoolStats {
    pub loaded_count: usize,
    pub total_ref_count: u32,
    pub total_allocated_bytes: u64,
    pub resident_memory_bytes: u64,
    pub available_bytes: u64,
    pub max_models: usize,
    pub max_memory_bytes: u64,
    pub eviction_policy: String,
}

pub struct ModelPoolInner {
    pub models: HashMap<String, LoadedModel>,
    config: PoolConfig,
}

impl ModelPoolInner {
    pub fn new(config: PoolConfig) -> Self {
        Self {
            models: HashMap::new(),
            config,
        }
    }

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    pub fn load(&mut self, path: &str) -> Result<LoadedModel, String> {
        let meta =
            inspect::inspect_model(path).map_err(|e| format!("Failed to inspect model: {e}"))?;

        let model_id = meta["model"]["id"]
            .as_str()
            .unwrap_or(&format!("local://{}", path))
            .to_string();

        if let Some(model) = self.models.get_mut(&model_id) {
            model.ref_count += 1;
            model.last_accessed_at = Self::now_secs();
            return Ok(model.clone());
        }

        let architecture = meta["architecture"]["architecture"]
            .as_str()
            .unwrap_or("unknown")
            .to_string();
        let quantisation = meta["model"]["quantisation"]
            .as_str()
            .unwrap_or("unknown")
            .to_string();
        let parameter_count = meta["model"]["parameter_count_raw"].as_f64();
        let file_size = meta["model"]["file_size"].as_u64().unwrap_or(0);
        let estimated_memory = meta["memory"]["total_bytes"].as_u64().unwrap_or(0);

        if self.config.max_memory_bytes > 0 {
            let current_memory: u64 = self.models.values().map(|m| m.estimated_memory_bytes).sum();
            if current_memory + estimated_memory > self.config.max_memory_bytes {
                self.evict_for_memory(estimated_memory)?;
            }
        }

        if self.config.max_models > 0 && self.models.len() >= self.config.max_models {
            self.evict_for_count()?;
        }

        let now = Self::now_secs();
        let model = LoadedModel {
            id: model_id.clone(),
            path: path.to_string(),
            architecture,
            quantisation,
            parameter_count,
            file_size,
            estimated_memory_bytes: estimated_memory,
            loaded_at: now,
            last_accessed_at: now,
            ref_count: 1,
            status: ModelPoolStatus::Loaded,
        };

        self.models.insert(model_id, model.clone());
        Ok(model)
    }

    pub fn unload(&mut self, id: &str) -> Result<bool, String> {
        let model = self
            .models
            .get_mut(id)
            .ok_or_else(|| format!("Model not loaded: {id}"))?;

        if model.ref_count > 0 {
            model.ref_count -= 1;
        }
        model.last_accessed_at = Self::now_secs();

        if model.ref_count == 0 {
            self.models.remove(id);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn get(&self, id: &str) -> Option<&LoadedModel> {
        self.models.get(id)
    }

    pub fn list(&self) -> Vec<LoadedModel> {
        self.models.values().cloned().collect()
    }

    pub fn stats(&self) -> PoolStats {
        let total_allocated: u64 = self.models.values().map(|m| m.estimated_memory_bytes).sum();
        let total_ref: u32 = self.models.values().map(|m| m.ref_count).sum();

        let available = if self.config.max_memory_bytes > 0 {
            self.config.max_memory_bytes.saturating_sub(total_allocated)
        } else {
            0
        };

        PoolStats {
            loaded_count: self.models.len(),
            total_ref_count: total_ref,
            total_allocated_bytes: total_allocated,
            resident_memory_bytes: total_allocated,
            available_bytes: available,
            max_models: self.config.max_models,
            max_memory_bytes: self.config.max_memory_bytes,
            eviction_policy: match self.config.eviction_policy {
                EvictionPolicy::Lru => "lru".to_string(),
                EvictionPolicy::Lfu => "lfu".to_string(),
            },
        }
    }

    fn evict_for_count(&mut self) -> Result<(), String> {
        if !self.evict_lru() {
            return Err("No evictable models (all have active references)".to_string());
        }
        Ok(())
    }

    fn evict_for_memory(&mut self, needed: u64) -> Result<(), String> {
        let mut freed = 0u64;
        loop {
            let before = self.models.len();
            let evicted_bytes = self.evict_lru_with_bytes();
            if self.models.len() == before {
                return Err(format!(
                    "Insufficient memory: need {needed} bytes, could not evict enough"
                ));
            }
            freed += evicted_bytes;
            if freed >= needed {
                break;
            }
        }
        Ok(())
    }

    fn evict_lru(&mut self) -> bool {
        let victim = self
            .models
            .iter()
            .filter(|(_, m)| m.ref_count == 0)
            .min_by_key(|(_, m)| m.last_accessed_at)
            .map(|(id, _)| id.clone());

        if let Some(id) = victim {
            self.models.remove(&id);
            true
        } else {
            false
        }
    }

    fn evict_lru_with_bytes(&mut self) -> u64 {
        let victim = self
            .models
            .iter()
            .filter(|(_, m)| m.ref_count == 0)
            .min_by_key(|(_, m)| m.last_accessed_at)
            .map(|(id, m)| (id.clone(), m.estimated_memory_bytes));

        if let Some((id, bytes)) = victim {
            self.models.remove(&id);
            bytes
        } else {
            0
        }
    }

    pub fn unload_all(&mut self) {
        self.models.clear();
    }

    pub fn touch(&mut self, id: &str) -> bool {
        if let Some(model) = self.models.get_mut(id) {
            model.last_accessed_at = Self::now_secs();
            true
        } else {
            false
        }
    }
}

pub type ModelPoolState = Arc<RwLock<ModelPoolInner>>;

use std::sync::Arc;
use tokio::sync::RwLock;

#[cfg(test)]
mod tests {
    use super::*;

    fn make_model(id: &str, path: &str, mem: u64, ts: u64, ref_count: u32) -> LoadedModel {
        LoadedModel {
            id: id.to_string(),
            path: path.to_string(),
            architecture: "llama".to_string(),
            quantisation: "Q4_K_M".to_string(),
            parameter_count: Some(1_000_000_000.0),
            file_size: mem,
            estimated_memory_bytes: mem,
            loaded_at: ts,
            last_accessed_at: ts,
            ref_count,
            status: ModelPoolStatus::Loaded,
        }
    }

    #[test]
    fn test_pool_new_is_empty() {
        let pool = ModelPoolInner::new(PoolConfig::default());
        assert_eq!(pool.models.len(), 0);
        assert_eq!(pool.list().len(), 0);
    }

    #[test]
    fn test_pool_stats_empty() {
        let pool = ModelPoolInner::new(PoolConfig::default());
        let stats = pool.stats();
        assert_eq!(stats.loaded_count, 0);
        assert_eq!(stats.total_ref_count, 0);
        assert_eq!(stats.total_allocated_bytes, 0);
        assert_eq!(stats.resident_memory_bytes, 0);
        assert_eq!(stats.eviction_policy, "lru");
    }

    #[test]
    fn test_pool_unload_not_loaded() {
        let mut pool = ModelPoolInner::new(PoolConfig::default());
        let result = pool.unload("nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn test_pool_unload_all() {
        let mut pool = ModelPoolInner::new(PoolConfig::default());
        pool.models
            .insert("a".to_string(), make_model("a", "/a.gguf", 100, 1, 1));
        pool.models
            .insert("b".to_string(), make_model("b", "/b.gguf", 200, 2, 1));
        assert_eq!(pool.models.len(), 2);
        pool.unload_all();
        assert_eq!(pool.models.len(), 0);
    }

    #[test]
    fn test_pool_unload_decrements_ref_count() {
        let mut pool = ModelPoolInner::new(PoolConfig::default());
        pool.models
            .insert("a".to_string(), make_model("a", "/a.gguf", 100, 1, 2));
        let result = pool.unload("a");
        assert!(result.is_ok());
        assert!(!result.unwrap()); // not fully unloaded
        assert_eq!(pool.get("a").unwrap().ref_count, 1);
    }

    #[test]
    fn test_pool_unload_removes_when_zero() {
        let mut pool = ModelPoolInner::new(PoolConfig::default());
        pool.models
            .insert("a".to_string(), make_model("a", "/a.gguf", 100, 1, 1));
        let result = pool.unload("a");
        assert!(result.is_ok());
        assert!(result.unwrap()); // fully unloaded
        assert!(pool.get("a").is_none());
    }

    #[test]
    fn test_pool_touch_updates_timestamp() {
        let mut pool = ModelPoolInner::new(PoolConfig::default());
        pool.models
            .insert("a".to_string(), make_model("a", "/a.gguf", 100, 1, 1));
        let old_ts = pool.get("a").unwrap().last_accessed_at;
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert!(pool.touch("a"));
        let new_ts = pool.get("a").unwrap().last_accessed_at;
        assert!(new_ts > old_ts);
    }

    #[test]
    fn test_pool_touch_nonexistent() {
        let mut pool = ModelPoolInner::new(PoolConfig::default());
        assert!(!pool.touch("nonexistent"));
    }

    #[test]
    fn test_pool_stats_with_models() {
        let mut pool = ModelPoolInner::new(PoolConfig::default());
        pool.models
            .insert("a".to_string(), make_model("a", "/a.gguf", 100, 1, 2));
        pool.models
            .insert("b".to_string(), make_model("b", "/b.gguf", 200, 2, 1));
        let stats = pool.stats();
        assert_eq!(stats.loaded_count, 2);
        assert_eq!(stats.total_ref_count, 3);
        assert_eq!(stats.total_allocated_bytes, 300);
        assert_eq!(stats.resident_memory_bytes, 300);
    }

    #[test]
    fn test_pool_evict_lru_removes_oldest_zero_ref() {
        let mut pool = ModelPoolInner::new(PoolConfig::default());
        pool.models
            .insert("a".to_string(), make_model("a", "/a.gguf", 100, 1, 0));
        pool.models
            .insert("b".to_string(), make_model("b", "/b.gguf", 200, 2, 0));
        assert!(pool.evict_lru());
        assert!(pool.get("a").is_none());
        assert!(pool.get("b").is_some());
    }

    #[test]
    fn test_pool_evict_lru_skips_referenced() {
        let mut pool = ModelPoolInner::new(PoolConfig::default());
        pool.models
            .insert("a".to_string(), make_model("a", "/a.gguf", 100, 1, 1));
        pool.models
            .insert("b".to_string(), make_model("b", "/b.gguf", 200, 2, 0));
        assert!(pool.evict_lru());
        assert!(pool.get("a").is_some()); // still referenced
        assert!(pool.get("b").is_none()); // evicted
    }

    #[test]
    fn test_pool_evict_lru_no_candidates() {
        let mut pool = ModelPoolInner::new(PoolConfig::default());
        pool.models
            .insert("a".to_string(), make_model("a", "/a.gguf", 100, 1, 1));
        assert!(!pool.evict_lru());
        assert!(pool.get("a").is_some());
    }

    #[test]
    fn test_pool_evict_for_count() {
        let mut pool = ModelPoolInner::new(PoolConfig::default());
        pool.models
            .insert("a".to_string(), make_model("a", "/a.gguf", 100, 1, 0));
        let result = pool.evict_for_count();
        assert!(result.is_ok());
        assert!(pool.get("a").is_none());
    }

    #[test]
    fn test_pool_evict_for_count_no_candidates() {
        let mut pool = ModelPoolInner::new(PoolConfig::default());
        pool.models
            .insert("a".to_string(), make_model("a", "/a.gguf", 100, 1, 1));
        let result = pool.evict_for_count();
        assert!(result.is_err());
    }

    #[test]
    fn test_pool_config_default() {
        let config = PoolConfig::default();
        assert_eq!(config.max_models, 4);
        assert_eq!(config.max_memory_bytes, 0);
        assert_eq!(config.eviction_policy, EvictionPolicy::Lru);
    }

    #[test]
    fn test_pool_stats_with_memory_limit() {
        let config = PoolConfig {
            max_models: 4,
            max_memory_bytes: 500,
            eviction_policy: EvictionPolicy::Lru,
        };
        let mut pool = ModelPoolInner::new(config);
        pool.models
            .insert("a".to_string(), make_model("a", "/a.gguf", 300, 1, 1));
        let stats = pool.stats();
        assert_eq!(stats.total_allocated_bytes, 300);
        assert_eq!(stats.available_bytes, 200);
    }

    #[test]
    fn test_pool_stats_memory_limit_exhausted() {
        let config = PoolConfig {
            max_models: 4,
            max_memory_bytes: 100,
            eviction_policy: EvictionPolicy::Lru,
        };
        let mut pool = ModelPoolInner::new(config);
        pool.models
            .insert("a".to_string(), make_model("a", "/a.gguf", 100, 1, 1));
        let stats = pool.stats();
        assert_eq!(stats.available_bytes, 0);
    }
}
