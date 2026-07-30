use std::collections::HashSet;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct CapabilityRegistry {
    capabilities: HashSet<String>,
}

impl CapabilityRegistry {
    pub fn new() -> Self {
        Self {
            capabilities: HashSet::new(),
        }
    }

    pub fn register(&mut self, capability: &str) {
        self.capabilities.insert(capability.to_string());
    }

    pub fn has(&self, capability: &str) -> bool {
        self.capabilities.contains(capability)
    }

    pub fn all(&self) -> Vec<&str> {
        let mut sorted: Vec<&str> = self.capabilities.iter().map(|s| s.as_str()).collect();
        sorted.sort();
        sorted
    }

    pub fn to_json_map(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        for cap in &self.capabilities {
            map.insert(cap.clone(), serde_json::Value::Bool(true));
        }
        serde_json::Value::Object(map)
    }

    pub fn build_ai_capabilities(llama_enabled: bool) -> Self {
        let mut reg = Self::new();
        if llama_enabled {
            reg.register("chat");
            reg.register("completion");
            reg.register("fim");
            reg.register("streaming");
            reg.register("per_token_streaming");
            reg.register("thinking");
            reg.register("fim_streaming");
            reg.register("tool_calling");
        }
        reg.register("model_inspection");
        reg.register("gguf_parsing");
        reg.register("metadata_extraction");
        reg.register("architecture_detection");
        reg.register("memory_estimation");
        reg.register("capability_detection");
        if llama_enabled {
            reg.register("llama_backend");
        }
        reg
    }
}
