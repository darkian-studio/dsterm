#![allow(dead_code)]

use std::fmt;

#[derive(Debug, Clone)]
pub struct InferenceError {
    pub code: &'static str,
    pub message: String,
    pub recoverable: bool,
}

impl InferenceError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            recoverable: false,
        }
    }

    pub fn recoverable(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            recoverable: true,
        }
    }

    pub fn model_not_loaded(id: impl Into<String>) -> Self {
        Self::new(
            "MODEL_NOT_LOADED",
            format!("model not loaded: {}", id.into()),
        )
    }

    pub fn context_creation_failed(msg: impl Into<String>) -> Self {
        Self::new("CONTEXT_CREATION_FAILED", msg)
    }

    pub fn tokenization_failed(msg: impl Into<String>) -> Self {
        Self::new("TOKENIZATION_FAILED", msg)
    }

    pub fn decode_failed(msg: impl Into<String>) -> Self {
        Self::new("DECODE_FAILED", msg)
    }

    pub fn sampling_failed(msg: impl Into<String>) -> Self {
        Self::new("SAMPLING_FAILED", msg)
    }

    pub fn cancelled() -> Self {
        Self::recoverable("CANCELLED", "generation cancelled")
    }

    pub fn max_context_exceeded(ctx: usize, needed: usize) -> Self {
        Self::new(
            "MAX_CONTEXT_EXCEEDED",
            format!("context size {ctx} exceeds limit, needed {needed}"),
        )
    }

    pub fn backend_failure(msg: impl Into<String>) -> Self {
        Self::new("BACKEND_FAILURE", msg)
    }

    pub fn timeout(operation: &str, secs: u64) -> Self {
        Self::new("TIMEOUT", format!("{operation} timed out after {secs}s"))
    }

    pub fn compatibility(msg: impl Into<String>) -> Self {
        Self::new("COMPATIBILITY", msg)
    }
}

impl fmt::Display for InferenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}] {} (recoverable: {})",
            self.code, self.message, self.recoverable
        )
    }
}

impl std::error::Error for InferenceError {}
