#![allow(dead_code)]

use std::ffi::c_char;
use std::os::raw::{c_float, c_int, c_uint};

#[allow(non_camel_case_types)]
pub type llama_token = i32;

/// Mirror of the llama_batch struct (verified field-for-field against the
/// vendored llama.h at tag b10210, lines 255-264). Re-verify on upstream bumps.
#[derive(Clone)]
#[repr(C)]
pub struct llama_batch {
    pub n_tokens: c_int,
    pub token: *mut llama_token,
    pub embd: *mut c_float,
    pub pos: *mut i32,
    pub n_seq_id: *mut c_int,
    pub seq_id: *mut *mut c_int,
    pub logits: *mut i8,
}

/// dsterm-owned context config. The shim translates this into llama.cpp's
/// (much larger, unstable) llama_context_params. Layout must match
/// `dsterm_ctx_config` in third_party/llama.cpp/dsterm_shim.h exactly.
#[repr(C)]
pub struct DstermCtxConfig {
    pub n_ctx: u32,
    pub n_batch: u32,
    pub n_ubatch: u32,
    pub n_threads: i32,
    pub n_threads_batch: i32,
    pub pooling_type: i32,
    pub embeddings: bool,
    pub flash_attn: bool,
    pub offload_kqv: bool,
    pub rope_scaling_type: i32,
}

/// dsterm-owned sampler config. Layout must match `dsterm_sampler_config`
/// in third_party/llama.cpp/dsterm_shim.h exactly.
#[repr(C)]
pub struct DstermSamplerConfig {
    pub temperature: f32,
    pub top_k: i32,
    pub top_p: f32,
    pub min_p: f32,
    pub repeat_penalty: f32,
    pub frequency_penalty: f32,
    pub presence_penalty: f32,
    pub penalty_last_n: i32,
}

extern "C" {
    // dsterm shim (third_party/llama.cpp/dsterm_shim.c).
    // All *_load / *_new functions return NULL on failure and never partially
    // construct a handle. Rust callers must treat NULL as a recoverable error.
    pub fn dsterm_llama_model_load(path: *const c_char) -> *mut std::ffi::c_void;
    pub fn dsterm_llama_model_vocab(model: *const std::ffi::c_void) -> *const std::ffi::c_void;
    pub fn dsterm_llama_model_free(model: *mut std::ffi::c_void);
    pub fn dsterm_llama_n_embd(model: *const std::ffi::c_void) -> c_int;

    pub fn dsterm_llama_ctx_new(
        model: *const std::ffi::c_void,
        cfg: *const DstermCtxConfig,
    ) -> *mut std::ffi::c_void;
    pub fn dsterm_llama_ctx_free(ctx: *mut std::ffi::c_void);
    pub fn dsterm_llama_n_ctx(ctx: *const std::ffi::c_void) -> c_uint;

    pub fn dsterm_llama_tokenize(
        vocab: *const std::ffi::c_void,
        text: *const c_char,
        len: c_int,
        tokens: *mut llama_token,
        max: c_int,
        add_bos: bool,
        special: bool,
    ) -> c_int;
    pub fn dsterm_llama_token_to_piece(
        vocab: *const std::ffi::c_void,
        token: llama_token,
        buf: *mut c_char,
        len: c_int,
        lstrip: c_int,
        special: bool,
    ) -> c_int;
    pub fn dsterm_llama_token_bos(vocab: *const std::ffi::c_void) -> llama_token;
    pub fn dsterm_llama_token_eos(vocab: *const std::ffi::c_void) -> llama_token;
    pub fn dsterm_llama_n_vocab(vocab: *const std::ffi::c_void) -> c_int;

    pub fn dsterm_llama_get_logits(ctx: *mut std::ffi::c_void) -> *mut c_float;
    pub fn dsterm_llama_get_embeddings(ctx: *mut std::ffi::c_void) -> *mut c_float;

    pub fn dsterm_llama_sampler_new(
        cfg: *const DstermSamplerConfig,
    ) -> *mut std::ffi::c_void;
    pub fn dsterm_llama_sample(
        sampler: *mut std::ffi::c_void,
        ctx: *mut std::ffi::c_void,
    ) -> c_int;
    pub fn dsterm_llama_sampler_free(sampler: *mut std::ffi::c_void);

    // Stable llama.cpp API kept direct (verified against vendored llama.h at b10210).
    pub fn llama_batch_get_one(tokens: *mut llama_token, n_tokens: c_int) -> llama_batch;
    pub fn llama_batch_free(batch: llama_batch);
    pub fn llama_decode(ctx: *mut std::ffi::c_void, batch: llama_batch) -> c_int;
    pub fn llama_pooling_type(ctx: *const std::ffi::c_void) -> c_int;
}
