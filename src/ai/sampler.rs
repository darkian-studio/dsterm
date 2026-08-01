#![allow(dead_code)]

use std::sync::Arc;

#[cfg(feature = "llama")]
use crate::ai::llama::bindings::*;

pub struct SamplingConfig {
    pub temperature: f32,
    pub top_p: f32,
    pub top_k: i32,
    pub min_p: f32,
    pub repeat_penalty: f32,
    pub frequency_penalty: f32,
    pub presence_penalty: f32,
}

impl Default for SamplingConfig {
    fn default() -> Self {
        Self {
            temperature: 0.7,
            top_p: 0.9,
            top_k: 40,
            min_p: 0.05,
            repeat_penalty: 1.1,
            frequency_penalty: 0.0,
            presence_penalty: 0.0,
        }
    }
}

pub trait Sampler: Send {
    fn sample(
        &self,
        logits: *const f32,
        n_vocab: i32,
    ) -> Result<i32, crate::ai::inference_error::InferenceError>;
    fn config(&self) -> &SamplingConfig;
}

#[cfg(feature = "llama")]
pub struct LlamaSampler {
    pub config: SamplingConfig,
    ctx: *mut std::ffi::c_void,
    sampler: *mut std::ffi::c_void,
}

#[cfg(feature = "llama")]
unsafe impl Send for LlamaSampler {}

#[cfg(feature = "llama")]
impl LlamaSampler {
    pub fn new(ctx: *mut std::ffi::c_void, config: SamplingConfig) -> Self {
        // D3: penalty_last_n = -1 (full context window) -- the old sampling
        // path passed an empty token history, silently disabling penalties.
        let cfg = DstermSamplerConfig {
            temperature: config.temperature,
            top_k: config.top_k,
            top_p: config.top_p,
            min_p: config.min_p,
            repeat_penalty: config.repeat_penalty,
            frequency_penalty: config.frequency_penalty,
            presence_penalty: config.presence_penalty,
            penalty_last_n: -1,
        };
        let sampler = unsafe { dsterm_llama_sampler_new(&cfg) };
        Self {
            config,
            ctx,
            sampler,
        }
    }
}

#[cfg(feature = "llama")]
impl Drop for LlamaSampler {
    fn drop(&mut self) {
        if !self.sampler.is_null() {
            unsafe { dsterm_llama_sampler_free(self.sampler) }
        }
    }
}

#[cfg(feature = "llama")]
impl Sampler for LlamaSampler {
    fn sample(
        &self,
        _logits: *const f32,
        _n_vocab: i32,
    ) -> Result<i32, crate::ai::inference_error::InferenceError> {
        if self.sampler.is_null() {
            return Err(crate::ai::inference_error::InferenceError::new(
                "SAMPLER_ERROR",
                "sampler chain failed to initialize",
            ));
        }

        // The sampler chain reads logits from the context itself.
        let token_id = unsafe { dsterm_llama_sample(self.sampler, self.ctx) };
        if token_id < 0 {
            return Err(crate::ai::inference_error::InferenceError::new(
                "SAMPLER_ERROR",
                "dsterm_llama_sample failed",
            ));
        }
        Ok(token_id)
    }

    fn config(&self) -> &SamplingConfig {
        &self.config
    }
}

pub type SamplerRef = Arc<dyn Sampler + Send + Sync>;
