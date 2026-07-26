pub mod web;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderRequest {
    pub operation: String,
    #[serde(flatten)]
    pub payload: serde_json::Value,
    #[serde(default)]
    pub budgets: Budgets,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Budgets {
    #[serde(default = "default_max_bytes")]
    pub max_bytes: usize,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default = "default_max_pages")]
    pub max_pages: usize,
    #[serde(default)]
    pub allow_browser: bool,
}

impl Default for Budgets {
    fn default() -> Self {
        Self {
            max_bytes: default_max_bytes(),
            timeout_secs: default_timeout_secs(),
            max_pages: default_max_pages(),
            allow_browser: false,
        }
    }
}

fn default_max_bytes() -> usize {
    2_000_000
}
fn default_timeout_secs() -> u64 {
    20
}
fn default_max_pages() -> usize {
    5
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> &'static str;
    async fn execute(&self, request: ProviderRequest) -> ProviderResponse;
}
