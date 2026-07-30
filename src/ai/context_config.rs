#![allow(dead_code)]

#[cfg(feature = "llama")]
use crate::ai::llama::bindings::llama_context_params;

pub struct ContextConfig {
    pub n_ctx: u32,
    pub n_batch: u32,
    pub n_ubatch: u32,
    pub n_threads: i32,
    pub n_threads_batch: i32,
    pub flash_attn: bool,
    pub offload_kqv: bool,
    pub rope_scaling_type: i32,
    pub no_kv_offload: bool,
    pub pooling_type: i32,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            n_ctx: 2048,
            n_batch: 512,
            n_ubatch: 512,
            n_threads: -1,
            n_threads_batch: -1,
            flash_attn: false,
            offload_kqv: true,
            rope_scaling_type: 0,
            no_kv_offload: false,
            pooling_type: 0,
        }
    }
}

impl ContextConfig {
    pub fn validate(&self) -> Result<(), crate::ai::inference_error::InferenceError> {
        use crate::ai::inference_error::InferenceError;
        if self.n_ctx == 0 {
            return Err(InferenceError::Internal("n_ctx must be > 0".into()));
        }
        if self.n_batch == 0 {
            return Err(InferenceError::Internal("n_batch must be > 0".into()));
        }
        if self.n_ubatch == 0 {
            return Err(InferenceError::Internal("n_ubatch must be > 0".into()));
        }
        Ok(())
    }

    #[cfg(feature = "llama")]
    pub fn to_llama_params(&self) -> llama_context_params {
        let mut p = unsafe { crate::ai::llama::bindings::llama_context_default_params() };
        p.n_ctx = self.n_ctx;
        p.n_batch = self.n_batch;
        p.n_ubatch = self.n_ubatch;
        p.n_threads = self.n_threads;
        p.n_threads_batch = self.n_threads_batch;
        p.flash_attn = self.flash_attn;
        p.offload_kqv = self.offload_kqv;
        p.rope_scaling_type = self.rope_scaling_type;
        p.no_kv_offload = self.no_kv_offload;
        p.pooling_type = self.pooling_type;
        p
    }
}
