#![allow(dead_code)]

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use crate::ai::context_config::ContextConfig;
use crate::ai::metrics::{self, GenerationMetrics};
use crate::ai::sampler::SamplingConfig;

pub struct GenerationSession {
    pub session_id: String,
    pub model_ref: Arc<super::llama::LlamaModel>,
    pub context_config: ContextConfig,
    pub sampling_config: SamplingConfig,
    pub max_tokens: i32,
    pub cancel: Arc<AtomicBool>,
    pub started_at: Instant,
    pub prompt_token_count: i32,
    pub first_token_at: Option<Instant>,
    pub completed_at: Option<Instant>,
}

impl GenerationSession {
    pub fn new(
        session_id: String,
        model_ref: Arc<super::llama::LlamaModel>,
        context_config: ContextConfig,
        sampling_config: SamplingConfig,
        max_tokens: i32,
        cancel: Arc<AtomicBool>,
    ) -> Self {
        Self {
            session_id,
            model_ref,
            context_config,
            sampling_config,
            max_tokens,
            cancel,
            started_at: Instant::now(),
            prompt_token_count: 0,
            first_token_at: None,
            completed_at: None,
        }
    }

    pub fn record_first_token(&mut self) {
        if self.first_token_at.is_none() {
            self.first_token_at = Some(Instant::now());
        }
    }

    pub fn complete(&mut self) {
        self.completed_at = Some(Instant::now());
        self.record_metrics();
    }

    fn record_metrics(&self) {
        let total = self
            .completed_at
            .map(|c| c.duration_since(self.started_at).as_millis() as u64)
            .unwrap_or(0);
        let prompt_latency = self
            .first_token_at
            .map(|f| f.duration_since(self.started_at).as_millis() as u64)
            .unwrap_or(0);
        let first_token_latency = self
            .first_token_at
            .and_then(|f| {
                self.completed_at
                    .map(|c| c.duration_since(f).as_millis() as u64)
            })
            .unwrap_or(0);
        let tps = if total > 0 {
            (self.prompt_token_count as f64 / total as f64) * 1000.0
        } else {
            0.0
        };

        metrics::record_generation(GenerationMetrics {
            prompt_tokens: self.prompt_token_count,
            completion_tokens: self.prompt_token_count,
            prompt_latency_ms: prompt_latency,
            first_token_latency_ms: first_token_latency,
            total_latency_ms: total,
            tokens_per_sec: tps,
        });
    }
}
