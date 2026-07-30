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
}

#[cfg(feature = "llama")]
unsafe impl Send for LlamaSampler {}

#[cfg(feature = "llama")]
impl LlamaSampler {
    pub fn new(ctx: *mut std::ffi::c_void, config: SamplingConfig) -> Self {
        Self { config, ctx }
    }
}

#[cfg(feature = "llama")]
impl Sampler for LlamaSampler {
    fn sample(
        &self,
        _logits: *const f32,
        n_vocab: i32,
    ) -> Result<i32, crate::ai::inference_error::InferenceError> {
        let mut candidates_vec: Vec<llama_token_data> = (0..n_vocab)
            .map(|i| llama_token_data {
                id: i,
                logit: unsafe { *_logits.add(i as usize) },
                p: 0.0,
            })
            .collect();

        let mut candidates = llama_token_data_array {
            data: candidates_vec.as_mut_ptr(),
            size: n_vocab as usize,
            sorted: false,
        };

        unsafe {
            llama_sample_repetition_penalties(
                self.ctx,
                &mut candidates,
                std::ptr::null(),
                0,
                self.config.repeat_penalty,
                self.config.frequency_penalty,
                self.config.presence_penalty,
            );

            if (self.config.temperature - 0.0).abs() > f32::EPSILON {
                llama_sample_temperature(self.ctx, &mut candidates, self.config.temperature);
            }

            if (self.config.top_p - 1.0).abs() > f32::EPSILON && self.config.top_p > 0.0 {
                llama_sample_top_p(self.ctx, &mut candidates, self.config.top_p, 1);
            }

            if self.config.top_k > 0 {
                llama_sample_top_k(self.ctx, &mut candidates, self.config.top_k, 1);
            }

            if (self.config.min_p - 0.0).abs() > f32::EPSILON && self.config.min_p > 0.0 {
                llama_sample_min_p(self.ctx, &mut candidates, self.config.min_p, 1);
            }

            let token_id = llama_sample_token(self.ctx, &mut candidates);
            Ok(token_id)
        }
    }

    fn config(&self) -> &SamplingConfig {
        &self.config
    }
}

pub type SamplerRef = Arc<dyn Sampler + Send + Sync>;
