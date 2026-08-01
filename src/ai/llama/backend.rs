#![allow(dead_code)]

use std::ffi::CString;
use std::ptr::NonNull;

use super::bindings::*;

pub struct LlamaModel {
    ptr: NonNull<std::ffi::c_void>,
    vocab: *const std::ffi::c_void,
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

        Ok(Self {
            ptr: NonNull::new(ptr).unwrap(),
            vocab,
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

        let n = unsafe {
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

        if n < 0 {
            return Err(format!("dsterm_llama_tokenize failed with {n}"));
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
