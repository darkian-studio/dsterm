#![allow(dead_code)]

use std::ffi::{c_char, CStr, CString};
use std::ptr::NonNull;
use std::sync::Mutex;

use super::backend::LlamaModel;
use super::bindings::*;

/// Opaque handle to the model's chat template: llama.cpp's native Jinja2
/// engine (common/chat.cpp), initialized once per model load. `apply` is
/// serialized because the shared template handle is not guaranteed
/// thread-safe across concurrent renders.
pub struct ChatTemplates {
    ptr: NonNull<std::ffi::c_void>,
    lock: Mutex<()>,
}

unsafe impl Send for ChatTemplates {}
unsafe impl Sync for ChatTemplates {}

#[derive(Debug, Clone)]
pub struct ChatTemplateOutput {
    pub prompt: String,
    pub supports_thinking: bool,
    pub thinking_start_tag: Option<String>,
    pub thinking_end_tags: Vec<String>,
    pub additional_stops: Vec<String>,
}

impl ChatTemplates {
    pub fn init(model: &LlamaModel) -> Result<Self, String> {
        let ptr = unsafe { dsterm_chat_templates_init(model.ptr()) };
        if ptr.is_null() {
            return Err("dsterm_chat_templates_init failed".to_string());
        }
        Ok(Self {
            ptr: NonNull::new(ptr).unwrap(),
            lock: Mutex::new(()),
        })
    }

    /// Wrap an already-initialized, non-NULL handle (used by
    /// `LlamaModel::load`, which validates the pointer first).
    pub(crate) fn from_ptr(ptr: *mut std::ffi::c_void) -> Self {
        Self {
            ptr: NonNull::new(ptr).unwrap(),
            lock: Mutex::new(()),
        }
    }

    pub fn supports_thinking(&self) -> bool {
        unsafe { dsterm_chat_supports_thinking(self.ptr.as_ptr()) }
    }

    pub fn apply(
        &self,
        messages: &[(String, String)],
        enable_thinking: bool,
    ) -> Result<ChatTemplateOutput, String> {
        let _guard = self
            .lock
            .lock()
            .map_err(|e| format!("chat template lock poisoned: {e}"))?;

        let owned: Vec<(CString, CString)> = messages
            .iter()
            .map(|(role, content)| {
                (
                    CString::new(role.as_str()).unwrap_or_default(),
                    CString::new(content.as_str()).unwrap_or_default(),
                )
            })
            .collect();

        let raw: Vec<DstermChatMessage> = owned
            .iter()
            .map(|(role, content)| DstermChatMessage {
                role: role.as_ptr(),
                content: content.as_ptr(),
            })
            .collect();

        let result = unsafe {
            dsterm_chat_apply_template(
                self.ptr.as_ptr(),
                raw.as_ptr(),
                raw.len() as i32,
                enable_thinking,
            )
        };
        if result.is_null() {
            return Err("dsterm_chat_apply_template failed".to_string());
        }

        // Read every field while the result is still alive, then free it.
        let output = unsafe { read_result(result) };
        unsafe { dsterm_chat_result_free(result) };
        Ok(output)
    }
}

impl Drop for ChatTemplates {
    fn drop(&mut self) {
        unsafe { dsterm_chat_templates_free(self.ptr.as_ptr()) }
    }
}

unsafe fn read_cstr(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    Some(CStr::from_ptr(ptr).to_string_lossy().into_owned())
}

unsafe fn read_str_vec(ptr: *mut *mut c_char, n: i32) -> Vec<String> {
    if ptr.is_null() || n <= 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..n as usize {
        if let Some(s) = read_cstr(*ptr.add(i)) {
            out.push(s);
        }
    }
    out
}

unsafe fn read_result(result: *mut DstermChatResult) -> ChatTemplateOutput {
    let r = &*result;
    ChatTemplateOutput {
        prompt: read_cstr(r.prompt).unwrap_or_default(),
        supports_thinking: r.supports_thinking,
        thinking_start_tag: read_cstr(r.thinking_start_tag),
        thinking_end_tags: read_str_vec(r.thinking_end_tags, r.n_thinking_end_tags),
        additional_stops: read_str_vec(r.additional_stops, r.n_additional_stops),
    }
}
