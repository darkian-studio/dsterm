#![allow(dead_code)]

use std::ffi::CString;
use std::ptr::NonNull;

use super::bindings::*;
use super::chat_template::ChatTemplates;

/// Upper bound for the tokenize output buffer (~8 MiB of i32). Prevents a
/// runaway prompt from allocating the full i32 range.
const MAX_TOKENIZE_BUFFER: usize = 2_000_000;

pub struct LlamaModel {
    ptr: NonNull<std::ffi::c_void>,
    vocab: *const std::ffi::c_void,
    chat_templates: Option<ChatTemplates>,
}

unsafe impl Send for LlamaModel {}
unsafe impl Sync for LlamaModel {}

impl LlamaModel {
    pub fn load(path: &str) -> Result<Self, String> {
        let c_path = CString::new(path).map_err(|e| format!("Invalid model path: {e}"))?;

        let ptr = unsafe { dsterm_llama_model_load(c_path.as_ptr()) };
        if ptr.is_null() {
            return Err(format!("dsterm_llama_model_load failed: {path}"));
        }

        let vocab = unsafe { dsterm_llama_model_vocab(ptr) };
        if vocab.is_null() {
            unsafe { dsterm_llama_model_free(ptr) };
            return Err(format!("model loaded but vocab is null: {path}"));
        }

        // Chat templates are parsed once per model load. A failure here is
        // non-fatal: requests then fall back to legacy formatting.
        let chat_templates = unsafe { dsterm_chat_templates_init(ptr) };
        let chat_templates = if chat_templates.is_null() {
            tracing::warn!(
                "chat templates unavailable for {path}; chat requests will use legacy formatting"
            );
            None
        } else {
            Some(ChatTemplates::from_ptr(chat_templates))
        };

        Ok(Self {
            ptr: NonNull::new(ptr).unwrap(),
            vocab,
            chat_templates,
        })
    }

    pub fn create_context(&self, config: DstermCtxConfig) -> Result<LlamaContext, String> {
        let ctx = unsafe { dsterm_llama_ctx_new(self.ptr.as_ptr(), &config) };
        if ctx.is_null() {
            return Err("dsterm_llama_ctx_new failed".to_string());
        }

        Ok(LlamaContext {
            ptr: NonNull::new(ctx).unwrap(),
            vocab: self.vocab,
        })
    }

    pub fn ptr(&self) -> *mut std::ffi::c_void {
        self.ptr.as_ptr()
    }

    pub fn n_embd(&self) -> i32 {
        unsafe { dsterm_llama_n_embd(self.ptr.as_ptr()) }
    }

    pub fn chat_templates(&self) -> Option<&ChatTemplates> {
        self.chat_templates.as_ref()
    }
}

impl Drop for LlamaModel {
    fn drop(&mut self) {
        unsafe { dsterm_llama_model_free(self.ptr.as_ptr()) }
    }
}

pub struct LlamaContext {
    ptr: NonNull<std::ffi::c_void>,
    vocab: *const std::ffi::c_void,
}

unsafe impl Send for LlamaContext {}
unsafe impl Sync for LlamaContext {}

impl LlamaContext {
    pub fn n_ctx(&self) -> i32 {
        unsafe { dsterm_llama_n_ctx(self.ptr.as_ptr()) as i32 }
    }

    pub fn n_vocab(&self) -> i32 {
        unsafe { dsterm_llama_n_vocab(self.vocab) }
    }

    pub fn tokenize(&self, text: &str, add_bos: bool) -> Result<Vec<llama_token>, String> {
        let c_text = CString::new(text).map_err(|e| format!("Invalid text: {e}"))?;
        let n_ctx = self.n_ctx() as usize;
        let mut tokens: Vec<llama_token> = vec![0i32; n_ctx];

        // llama_tokenize reports the required token count as a negative
        // number when the output buffer is too small (INT32_MIN on true
        // overflow). Grow the buffer and retry so prompts longer than the
        // context window can be counted (and then truncated by the caller).
        let mut n = unsafe {
            dsterm_llama_tokenize(
                self.vocab,
                c_text.as_ptr(),
                text.len() as i32,
                tokens.as_mut_ptr(),
                n_ctx as i32,
                add_bos,
                false,
            )
        };

        if n == i32::MIN {
            return Err("tokenizer overflow: result exceeds i32 token count".into());
        }
        if n < 0 {
            let required = n.unsigned_abs() as usize;
            if required > MAX_TOKENIZE_BUFFER {
                return Err(format!(
                    "prompt too long: tokenization would produce {required} tokens (cap {MAX_TOKENIZE_BUFFER})"
                ));
            }
            tokens.resize(required, 0);
            n = unsafe {
                dsterm_llama_tokenize(
                    self.vocab,
                    c_text.as_ptr(),
                    text.len() as i32,
                    tokens.as_mut_ptr(),
                    required as i32,
                    add_bos,
                    false,
                )
            };
            if n < 0 {
                return Err(format!("dsterm_llama_tokenize failed with {n}"));
            }
        }

        tokens.truncate(n as usize);
        Ok(tokens)
    }

    pub fn token_to_piece(&self, token: llama_token) -> Result<String, String> {
        let mut buf = vec![0i8; 32];

        let n = unsafe {
            dsterm_llama_token_to_piece(
                self.vocab,
                token,
                buf.as_mut_ptr() as *mut std::ffi::c_char,
                buf.len() as i32,
                0,
                false,
            )
        };

        if n < 0 {
            // buffer too small, resize and retry
            let size = (-n) as usize;
            buf.resize(size, 0);
            let n = unsafe {
                dsterm_llama_token_to_piece(
                    self.vocab,
                    token,
                    buf.as_mut_ptr() as *mut std::ffi::c_char,
                    buf.len() as i32,
                    0,
                    false,
                )
            };
            if n < 0 {
                return Err(format!("dsterm_llama_token_to_piece failed with {n}"));
            }
            buf.truncate(n as usize);
        } else {
            buf.truncate(n as usize);
        }

        let bytes: Vec<u8> = buf.iter().map(|&b| b as u8).collect();
        String::from_utf8(bytes).map_err(|e| format!("Invalid UTF-8: {e}"))
    }

    pub fn token_bos(&self) -> llama_token {
        unsafe { dsterm_llama_token_bos(self.vocab) }
    }

    pub fn token_eos(&self) -> llama_token {
        unsafe { dsterm_llama_token_eos(self.vocab) }
    }

    pub fn ptr_mut(&mut self) -> *mut std::ffi::c_void {
        self.ptr.as_ptr()
    }
}

impl Drop for LlamaContext {
    fn drop(&mut self) {
        unsafe { dsterm_llama_ctx_free(self.ptr.as_ptr()) }
    }
}
