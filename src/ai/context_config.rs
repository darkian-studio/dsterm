#![allow(dead_code)]

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
    pub fn to_dsterm_config(&self) -> crate::ai::llama::bindings::DstermCtxConfig {
        use crate::ai::llama::bindings::DstermCtxConfig;
        DstermCtxConfig {
            n_ctx: self.n_ctx,
            n_batch: self.n_batch,
            n_ubatch: self.n_ubatch,
            n_threads: self.n_threads,
            n_threads_batch: self.n_threads_batch,
            pooling_type: self.pooling_type,
            embeddings: false,
            flash_attn: self.flash_attn,
            offload_kqv: self.offload_kqv,
            rope_scaling_type: self.rope_scaling_type,
        }
    }
}

/// Largest power-of-two context (>= 2048, <= the model's native context) whose
/// estimated memory footprint fits the device RAM budget, mirroring
/// PocketPal's heuristic: ceiling = min(60% of RAM, RAM - 1.2 GiB).
///
/// `mem` must carry the KV cache estimate computed at the model's native
/// context length (see `MemoryBreakdown::context_length`).
pub fn auto_n_ctx(mem: &super::pool::MemoryBreakdown, total_ram_bytes: u64) -> u32 {
    const MIN_CTX: u32 = 2048;
    const MAX_CTX: u32 = 32768;

    let train = if mem.context_length > 0 {
        mem.context_length as u64
    } else {
        4096
    };
    let kv_per_token = mem.kv_cache_bytes / train;

    if kv_per_token == 0 {
        // No KV estimate available; stay conservative.
        return train.min(MAX_CTX as u64).max(MIN_CTX as u64) as u32;
    }

    let budget = total_ram_bytes
        .saturating_sub(1_200_000_000)
        .min(total_ram_bytes.saturating_mul(6) / 10);

    let fits = |ctx: u64| -> bool {
        mem.weights_bytes
            .saturating_add(kv_per_token.saturating_mul(ctx))
            .saturating_add(mem.overhead_bytes)
            <= budget
    };

    // Largest power of two <= train.
    let mut ctx = if train >= 1 {
        1u64 << (63 - train.leading_zeros())
    } else {
        1
    };
    loop {
        if ctx < MIN_CTX as u64 {
            return MIN_CTX;
        }
        if ctx <= MAX_CTX as u64 && fits(ctx) {
            return ctx as u32;
        }
        ctx /= 2;
    }
}

#[cfg(test)]
mod tests {
    use super::auto_n_ctx;
    use crate::ai::pool::MemoryBreakdown;

    fn qwen3_0_6b() -> MemoryBreakdown {
        MemoryBreakdown {
            weights_bytes: 375_816_192,
            kv_cache_bytes: 4_697_620_480,
            runtime_buffers_bytes: 0,
            overhead_bytes: 50_000_000,
            total_bytes: 5_123_436_672,
            context_length: 32768,
        }
    }

    #[test]
    fn picks_largest_fitting_power_of_two() {
        // 5.6 GB device -> budget ~3.4 GB; 16K ctx fits (~2.8 GB), 32K doesn't.
        let n = auto_n_ctx(&qwen3_0_6b(), 5_624_656_000);
        assert_eq!(n, 16384);
    }

    #[test]
    fn small_device_floors_at_2048() {
        // 2 GB device -> budget ~0.8 GB; 2K ctx (~0.72 GB) is the floor.
        let n = auto_n_ctx(&qwen3_0_6b(), 2_000_000_000);
        assert_eq!(n, 2048);
    }

    #[test]
    fn huge_device_uses_native_context() {
        // 24 GB device -> budget ~13.7 GB; full 32K ctx fits.
        let n = auto_n_ctx(&qwen3_0_6b(), 24_000_000_000);
        assert_eq!(n, 32768);
    }

    #[test]
    fn never_exceeds_native_context() {
        let n = auto_n_ctx(&qwen3_0_6b(), u64::MAX);
        assert!(n <= 32768);
        assert_eq!(n, 32768);
    }
}
