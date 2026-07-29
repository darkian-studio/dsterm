use std::ffi::c_char;
use std::os::raw::{c_float, c_int, c_uint};

#[allow(non_camel_case_types)]
pub type llama_token = i32;

#[repr(C)]
pub struct llama_model_params {
    pub n_gpu_layers: c_int,
    pub main_gpu: c_int,
    pub tensor_split: *const c_float,
    pub rpc_servers: *const c_char,
    pub vocab_only: bool,
    pub use_mmap: bool,
    pub use_mlock: bool,
    pub check_tensors: bool,
}

impl Default for llama_model_params {
    fn default() -> Self {
        Self {
            n_gpu_layers: 0,
            main_gpu: 0,
            tensor_split: std::ptr::null(),
            rpc_servers: std::ptr::null(),
            vocab_only: false,
            use_mmap: true,
            use_mlock: false,
            check_tensors: false,
        }
    }
}

#[repr(C)]
pub struct llama_context_params {
    pub n_ctx: c_uint,
    pub n_batch: c_uint,
    pub n_ubatch: c_uint,
    pub n_seq_max: c_int,
    pub n_threads: c_int,
    pub n_threads_batch: c_int,
    pub rope_scaling_type: c_int,
    pub pooling_type: c_int,
    pub attention_type: c_int,
    pub offload_kqv: bool,
    pub flash_attn: bool,
    pub no_kv_offload: bool,
    pub yarn_log_scale: c_float,
}

impl Default for llama_context_params {
    fn default() -> Self {
        Self {
            n_ctx: 2048,
            n_batch: 512,
            n_ubatch: 512,
            n_seq_max: 1,
            n_threads: 4,
            n_threads_batch: 4,
            rope_scaling_type: 0,
            pooling_type: 0,
            attention_type: 0,
            offload_kqv: true,
            flash_attn: false,
            no_kv_offload: false,
            yarn_log_scale: 0.0,
        }
    }
}

#[derive(Clone, Copy)]
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

#[repr(C)]
pub struct llama_token_data {
    pub id: llama_token,
    pub logit: c_float,
    pub p: c_float,
}

#[repr(C)]
pub struct llama_token_data_array {
    pub data: *mut llama_token_data,
    pub size: usize,
    pub sorted: bool,
}

extern "C" {
    #[link_name = "llama_model_default_params"]
    pub fn llama_model_default_params() -> llama_model_params;

    #[link_name = "llama_context_default_params"]
    pub fn llama_context_default_params() -> llama_context_params;

    #[link_name = "llama_load_model_from_file"]
    pub fn llama_load_model_from_file(
        path: *const c_char,
        params: llama_model_params,
    ) -> *mut std::ffi::c_void;

    #[link_name = "llama_free_model"]
    pub fn llama_free_model(model: *mut std::ffi::c_void);

    #[link_name = "llama_new_context_with_model"]
    pub fn llama_new_context_with_model(
        model: *mut std::ffi::c_void,
        params: llama_context_params,
    ) -> *mut std::ffi::c_void;

    #[link_name = "llama_free"]
    pub fn llama_free(ctx: *mut std::ffi::c_void);

    #[link_name = "llama_tokenize"]
    pub fn llama_tokenize(
        ctx: *mut std::ffi::c_void,
        text: *const c_char,
        text_len: c_int,
        tokens: *mut llama_token,
        n_max_tokens: c_int,
        add_bos: bool,
        special: bool,
    ) -> c_int;

    #[link_name = "llama_token_to_piece"]
    pub fn llama_token_to_piece(
        ctx: *mut std::ffi::c_void,
        token: llama_token,
        buf: *mut c_char,
        length: c_int,
        lstrip: bool,
        special: bool,
    ) -> c_int;

    #[link_name = "llama_batch_get_one"]
    pub fn llama_batch_get_one(tokens: *mut llama_token, n_tokens: c_int) -> llama_batch;

    #[link_name = "llama_batch_free"]
    #[allow(dead_code)]
    pub fn llama_batch_free(batch: llama_batch);

    #[link_name = "llama_decode"]
    pub fn llama_decode(ctx: *mut std::ffi::c_void, batch: llama_batch) -> c_int;

    #[link_name = "llama_get_logits"]
    pub fn llama_get_logits(ctx: *mut std::ffi::c_void) -> *mut c_float;

    #[link_name = "llama_sample_repetition_penalties"]
    pub fn llama_sample_repetition_penalties(
        ctx: *mut std::ffi::c_void,
        candidates: *mut llama_token_data_array,
        last_tokens: *const llama_token,
        last_tokens_size: usize,
        penalty_repeat: c_float,
        penalty_freq: c_float,
        penalty_present: c_float,
    );

    #[link_name = "llama_sample_temperature"]
    pub fn llama_sample_temperature(
        ctx: *mut std::ffi::c_void,
        candidates: *mut llama_token_data_array,
        temp: c_float,
    );

    #[link_name = "llama_sample_top_p"]
    pub fn llama_sample_top_p(
        ctx: *mut std::ffi::c_void,
        candidates: *mut llama_token_data_array,
        p: c_float,
        min_keep: usize,
    );

    #[link_name = "llama_sample_token"]
    pub fn llama_sample_token(
        ctx: *mut std::ffi::c_void,
        candidates: *mut llama_token_data_array,
    ) -> llama_token;

    #[link_name = "llama_token_bos"]
    pub fn llama_token_bos(ctx: *mut std::ffi::c_void) -> llama_token;

    #[link_name = "llama_token_eos"]
    pub fn llama_token_eos(ctx: *mut std::ffi::c_void) -> llama_token;

    #[link_name = "llama_n_ctx"]
    pub fn llama_n_ctx(ctx: *mut std::ffi::c_void) -> c_int;

    #[link_name = "llama_n_vocab"]
    pub fn llama_n_vocab(ctx: *mut std::ffi::c_void) -> c_int;

    #[link_name = "llama_kv_cache_clear"]
    #[allow(dead_code)]
    pub fn llama_kv_cache_clear(ctx: *mut std::ffi::c_void);
}
