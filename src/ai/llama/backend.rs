use std::ffi::CString;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::bindings::*;

const DEFAULT_MAX_TOKENS: i32 = 512;
const DEFAULT_TEMPERATURE: f32 = 0.7;
const DEFAULT_TOP_P: f32 = 0.9;

#[derive(Debug, Clone)]
pub struct GenerateConfig {
    pub max_tokens: i32,
    pub temperature: f32,
    pub top_p: f32,
    pub repeat_penalty: f32,
    pub frequency_penalty: f32,
    pub presence_penalty: f32,
}

impl Default for GenerateConfig {
    fn default() -> Self {
        Self {
            max_tokens: DEFAULT_MAX_TOKENS,
            temperature: DEFAULT_TEMPERATURE,
            top_p: DEFAULT_TOP_P,
            repeat_penalty: 1.1,
            frequency_penalty: 0.0,
            presence_penalty: 0.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GenerateResult {
    pub text: String,
    pub prompt_tokens: i32,
    pub completion_tokens: i32,
    pub stopped_by_eos: bool,
    pub stopped_by_max_tokens: bool,
}

pub struct LlamaModel {
    ptr: NonNull<std::ffi::c_void>,
}

unsafe impl Send for LlamaModel {}
unsafe impl Sync for LlamaModel {}

impl LlamaModel {
    pub fn load(path: &str) -> Result<Self, String> {
        let c_path = CString::new(path).map_err(|e| format!("Invalid model path: {e}"))?;

        let params = llama_model_default_params();

        let ptr = unsafe { llama_load_model_from_file(c_path.as_ptr(), params) };
        if ptr.is_null() {
            return Err(format!("llama_load_model_from_file failed: {path}"));
        }

        Ok(Self {
            ptr: NonNull::new(ptr).unwrap(),
        })
    }

    pub fn create_context(&self, params: llama_context_params) -> Result<LlamaContext, String> {
        let ctx = unsafe { llama_new_context_with_model(self.ptr.as_ptr(), params) };
        if ctx.is_null() {
            return Err("llama_new_context_with_model failed".to_string());
        }

        Ok(LlamaContext {
            ptr: NonNull::new(ctx).unwrap(),
        })
    }

    pub fn create_default_context(&self) -> Result<LlamaContext, String> {
        self.create_context(llama_context_default_params())
    }

    pub fn ptr(&self) -> *mut std::ffi::c_void {
        self.ptr.as_ptr()
    }
}

impl Drop for LlamaModel {
    fn drop(&mut self) {
        unsafe { llama_free_model(self.ptr.as_ptr()) }
    }
}

pub struct LlamaContext {
    ptr: NonNull<std::ffi::c_void>,
}

unsafe impl Send for LlamaContext {}
unsafe impl Sync for LlamaContext {}

impl LlamaContext {
    pub fn n_ctx(&self) -> i32 {
        unsafe { llama_n_ctx(self.ptr.as_ptr()) }
    }

    pub fn n_vocab(&self) -> i32 {
        unsafe { llama_n_vocab(self.ptr.as_ptr()) }
    }

    pub fn tokenize(&self, text: &str, add_bos: bool) -> Result<Vec<llama_token>, String> {
        let c_text = CString::new(text).map_err(|e| format!("Invalid text: {e}"))?;
        let n_ctx = self.n_ctx() as usize;
        let mut tokens: Vec<llama_token> = vec![0i32; n_ctx];

        let n = unsafe {
            llama_tokenize(
                self.ptr.as_ptr(),
                c_text.as_ptr(),
                text.len() as i32,
                tokens.as_mut_ptr(),
                n_ctx as i32,
                add_bos,
                false,
            )
        };

        if n < 0 {
            return Err(format!("llama_tokenize failed with {n}"));
        }

        tokens.truncate(n as usize);
        Ok(tokens)
    }

    pub fn token_to_piece(&self, token: llama_token) -> Result<String, String> {
        let mut buf = vec![0i8; 32];

        let n = unsafe {
            llama_token_to_piece(
                self.ptr.as_ptr(),
                token,
                buf.as_mut_ptr() as *mut std::ffi::c_char,
                buf.len() as i32,
                false,
                false,
            )
        };

        if n < 0 {
            // buffer too small, resize and retry
            let size = (-n) as usize;
            buf.resize(size, 0);
            let n = unsafe {
                llama_token_to_piece(
                    self.ptr.as_ptr(),
                    token,
                    buf.as_mut_ptr() as *mut std::ffi::c_char,
                    buf.len() as i32,
                    false,
                    false,
                )
            };
            if n < 0 {
                return Err(format!("llama_token_to_piece failed with {n}"));
            }
            buf.truncate(n as usize);
        } else {
            buf.truncate(n as usize);
        }

        let bytes: Vec<u8> = buf.iter().map(|&b| b as u8).collect();
        String::from_utf8(bytes).map_err(|e| format!("Invalid UTF-8: {e}"))
    }

    pub fn token_bos(&self) -> llama_token {
        unsafe { llama_token_bos(self.ptr.as_ptr()) }
    }

    pub fn token_eos(&self) -> llama_token {
        unsafe { llama_token_eos(self.ptr.as_ptr()) }
    }

    pub fn generate(
        &mut self,
        prompt: &str,
        config: &GenerateConfig,
    ) -> Result<GenerateResult, String> {
        let cancel = Arc::new(AtomicBool::new(false));
        self.generate_with_cancel(prompt, config, &cancel)
    }

    pub fn generate_with_cancel(
        &mut self,
        prompt: &str,
        config: &GenerateConfig,
        cancel: &AtomicBool,
    ) -> Result<GenerateResult, String> {
        let tokens = self.tokenize(prompt, true)?;
        if tokens.is_empty() {
            return Err("No tokens generated from prompt".to_string());
        }

        let bos = self.token_bos();
        let eos = self.token_eos();
        let n_ctx = self.n_ctx() as usize;

        let mut full_text = String::new();
        let prompt_tokens = tokens.len() as i32;
        let mut generated_tokens = 0i32;
        let mut stopped_by_eos = false;
        let mut stopped_by_max_tokens = false;

        // Evaluate prompt
        let batch = unsafe { llama_batch_get_one(tokens.as_mut_ptr(), tokens.len() as i32) };
        let ret = unsafe { llama_decode(self.ptr.as_ptr(), batch) };
        if ret != 0 {
            unsafe { llama_batch_free(batch) };
            return Err(format!("llama_decode prompt failed: {ret}"));
        }
        unsafe { llama_batch_free(batch) };

        let mut n_past = tokens.len();

        // Generation loop
        for _ in 0..config.max_tokens {
            if cancel.load(Ordering::Relaxed) {
                break;
            }

            if n_past >= n_ctx {
                break;
            }

            // Sample
            let logits = unsafe { llama_get_logits(self.ptr.as_ptr()) };
            if logits.is_null() {
                return Err("llama_get_logits returned null".to_string());
            }

            let n_vocab = self.n_vocab() as usize;
            let mut candidates_vec: Vec<llama_token_data> = (0..n_vocab)
                .map(|i| llama_token_data {
                    id: i as i32,
                    logit: unsafe { *logits.add(i) },
                    p: 0.0,
                })
                .collect();

            let mut candidates = llama_token_data_array {
                data: candidates_vec.as_mut_ptr(),
                size: n_vocab,
                sorted: false,
            };

            // Apply penalties
            unsafe {
                llama_sample_repetition_penalties(
                    self.ptr.as_ptr(),
                    &mut candidates,
                    std::ptr::null(),
                    0,
                    config.repeat_penalty,
                    config.frequency_penalty,
                    config.presence_penalty,
                );
            }

            // Temperature
            if (config.temperature - 0.0).abs() > f32::EPSILON {
                unsafe {
                    llama_sample_temperature(
                        self.ptr.as_ptr(),
                        &mut candidates,
                        config.temperature,
                    );
                }
            }

            // Top-p
            if (config.top_p - 1.0).abs() > f32::EPSILON && config.top_p > 0.0 {
                unsafe {
                    llama_sample_top_p(self.ptr.as_ptr(), &mut candidates, config.top_p, 1);
                }
            }

            let token_id = unsafe { llama_sample_token(self.ptr.as_ptr(), &mut candidates) };

            generated_tokens += 1;

            if token_id == eos && generated_tokens > 1 {
                stopped_by_eos = true;
                break;
            }

            if token_id == bos {
                // Skip BOS in output
                continue;
            }

            // Decode token
            match self.token_to_piece(token_id) {
                Ok(piece) => {
                    full_text.push_str(&piece);
                }
                Err(e) => {
                    return Err(format!("Failed to decode token: {e}"));
                }
            }

            if generated_tokens >= config.max_tokens {
                stopped_by_max_tokens = true;
                break;
            }

            // Evaluate next token
            let mut next_tokens = [token_id];
            let batch = unsafe { llama_batch_get_one(next_tokens.as_mut_ptr(), 1) };
            let ret = unsafe { llama_decode(self.ptr.as_ptr(), batch) };
            if ret != 0 {
                unsafe { llama_batch_free(batch) };
                return Err(format!("llama_decode failed: {ret}"));
            }
            unsafe { llama_batch_free(batch) };
            n_past += 1;
        }

        Ok(GenerateResult {
            text: full_text,
            prompt_tokens,
            completion_tokens: generated_tokens,
            stopped_by_eos,
            stopped_by_max_tokens,
        })
    }

    pub fn kv_cache_clear(&mut self) {
        unsafe { llama_kv_cache_clear(self.ptr.as_ptr()) }
    }
}

impl Drop for LlamaContext {
    fn drop(&mut self) {
        unsafe { llama_free(self.ptr.as_ptr()) }
    }
}
