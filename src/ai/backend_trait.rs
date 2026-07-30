#![allow(dead_code)]

use std::sync::Arc;

use crate::ai::context_config::ContextConfig;
use crate::ai::inference_error::InferenceError;
use crate::ai::sampler::SamplingConfig;

pub type BackendResult<T> = Result<T, InferenceError>;

pub struct BackendCapabilities {
    pub backend_version: String,
    pub llama_commit: String,
    pub supported_gguf_versions: Vec<i32>,
    pub flash_attn: bool,
    pub gpu_support: bool,
    pub simd: Vec<String>,
    pub threading: bool,
    pub build_flags: Vec<String>,
}

impl Default for BackendCapabilities {
    fn default() -> Self {
        Self {
            backend_version: env!("CARGO_PKG_VERSION").to_string(),
            llama_commit: "bundled".to_string(),
            supported_gguf_versions: vec![3],
            flash_attn: false,
            gpu_support: false,
            simd: vec!["default".to_string()],
            threading: true,
            build_flags: vec![],
        }
    }
}

#[async_trait::async_trait]
pub trait InferenceBackend: Send + Sync {
    fn capabilities(&self) -> BackendCapabilities;
    async fn validate(&self, ctx_config: &ContextConfig) -> BackendResult<()>;
    async fn tokenize(&self, text: &str, add_bos: bool) -> BackendResult<Vec<i32>>;
    async fn detokenize(&self, token: i32) -> BackendResult<String>;
    async fn generate(
        &self,
        prompt: &str,
        context_config: ContextConfig,
        sampling_config: SamplingConfig,
        max_tokens: i32,
    ) -> BackendResult<GenerateOutput>;
    async fn generate_streaming(
        &self,
        prompt: &str,
        context_config: ContextConfig,
        sampling_config: SamplingConfig,
        max_tokens: i32,
        sink: Box<dyn TokenSink + Send>,
    ) -> BackendResult<GenerateOutput>;
}

pub trait TokenSink {
    fn on_token(&mut self, token: &str) -> Result<(), String>;
    fn on_error(&mut self, err: &str);
    fn is_cancelled(&self) -> bool;
}

#[derive(Debug, Clone)]
pub struct GenerateOutput {
    pub text: String,
    pub prompt_tokens: i32,
    pub completion_tokens: i32,
    pub stopped_by_eos: bool,
    pub stopped_by_max_tokens: bool,
}

pub type BackendRef = Arc<dyn InferenceBackend>;
