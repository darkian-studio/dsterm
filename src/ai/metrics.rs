#![allow(dead_code)]

use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

static TOTAL_GENERATIONS: AtomicU64 = AtomicU64::new(0);
static TOTAL_PROMPT_TOKENS: AtomicU64 = AtomicU64::new(0);
static TOTAL_COMPLETION_TOKENS: AtomicU64 = AtomicU64::new(0);
static TOTAL_GENERATION_TIME_MS: AtomicU64 = AtomicU64::new(0);

static RECENT_GENERATIONS: Mutex<Vec<GenerationMetrics>> = Mutex::new(Vec::new());

#[derive(Debug, Clone, Serialize)]
pub struct GenerationMetrics {
    pub prompt_tokens: i32,
    pub completion_tokens: i32,
    pub prompt_latency_ms: u64,
    pub first_token_latency_ms: u64,
    pub total_latency_ms: u64,
    pub tokens_per_sec: f64,
}

pub fn record_generation(m: GenerationMetrics) {
    TOTAL_GENERATIONS.fetch_add(1, Ordering::Relaxed);
    TOTAL_PROMPT_TOKENS.fetch_add(m.prompt_tokens as u64, Ordering::Relaxed);
    TOTAL_COMPLETION_TOKENS.fetch_add(m.completion_tokens as u64, Ordering::Relaxed);
    TOTAL_GENERATION_TIME_MS.fetch_add(m.total_latency_ms, Ordering::Relaxed);

    if let Ok(mut recent) = RECENT_GENERATIONS.lock() {
        recent.push(m);
        if recent.len() > 100 {
            recent.remove(0);
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MetricsSnapshot {
    pub total_generations: u64,
    pub total_prompt_tokens: u64,
    pub total_completion_tokens: u64,
    pub total_generation_time_ms: u64,
    pub avg_tokens_per_sec: f64,
    pub recent_generations: Vec<GenerationMetrics>,
}

pub fn snapshot() -> MetricsSnapshot {
    let total_ms = TOTAL_GENERATION_TIME_MS.load(Ordering::Relaxed);
    let total_tokens = TOTAL_COMPLETION_TOKENS.load(Ordering::Relaxed);
    let avg_tps = if total_ms > 0 {
        (total_tokens as f64 / total_ms as f64) * 1000.0
    } else {
        0.0
    };

    let recent = RECENT_GENERATIONS
        .lock()
        .ok()
        .map(|r| r.clone())
        .unwrap_or_default();

    MetricsSnapshot {
        total_generations: TOTAL_GENERATIONS.load(Ordering::Relaxed),
        total_prompt_tokens: TOTAL_PROMPT_TOKENS.load(Ordering::Relaxed),
        total_completion_tokens: TOTAL_COMPLETION_TOKENS.load(Ordering::Relaxed),
        total_generation_time_ms: total_ms,
        avg_tokens_per_sec: avg_tps,
        recent_generations: recent,
    }
}
