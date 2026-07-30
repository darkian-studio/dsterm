use std::fmt;

#[derive(Debug, Clone)]
pub enum InferenceError {
    ModelNotLoaded(String),
    ModelBusy(String),
    Cancelled,
    OutOfMemory(String),
    InvalidPrompt(String),
    InvalidTemplate(String),
    InvalidSampling(String),
    BackendUnavailable(String),
    UnsupportedCapability(String),
    ContextOverflow { ctx: usize, needed: usize },
    ContextCreationFailed(String),
    TokenizationFailed(String),
    DecodeFailed(String),
    SamplingFailed(String),
    Timeout { operation: String, secs: u64 },
    Internal(String),
}

impl InferenceError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::ModelNotLoaded(_) => "MODEL_NOT_LOADED",
            Self::ModelBusy(_) => "MODEL_BUSY",
            Self::Cancelled => "CANCELLED",
            Self::OutOfMemory(_) => "OUT_OF_MEMORY",
            Self::InvalidPrompt(_) => "INVALID_PROMPT",
            Self::InvalidTemplate(_) => "INVALID_TEMPLATE",
            Self::InvalidSampling(_) => "INVALID_SAMPLING",
            Self::BackendUnavailable(_) => "BACKEND_UNAVAILABLE",
            Self::UnsupportedCapability(_) => "UNSUPPORTED_CAPABILITY",
            Self::ContextOverflow { .. } => "CONTEXT_OVERFLOW",
            Self::ContextCreationFailed(_) => "CONTEXT_CREATION_FAILED",
            Self::TokenizationFailed(_) => "TOKENIZATION_FAILED",
            Self::DecodeFailed(_) => "DECODE_FAILED",
            Self::SamplingFailed(_) => "SAMPLING_FAILED",
            Self::Timeout { .. } => "TIMEOUT",
            Self::Internal(_) => "INTERNAL_ERROR",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::ModelNotLoaded(id) => format!("model not loaded: {id}"),
            Self::ModelBusy(id) => format!("model busy: {id}"),
            Self::Cancelled => "generation cancelled".into(),
            Self::OutOfMemory(detail) => format!("out of memory: {detail}"),
            Self::InvalidPrompt(detail) => format!("invalid prompt: {detail}"),
            Self::InvalidTemplate(detail) => format!("invalid template: {detail}"),
            Self::InvalidSampling(detail) => format!("invalid sampling config: {detail}"),
            Self::BackendUnavailable(detail) => format!("backend unavailable: {detail}"),
            Self::UnsupportedCapability(cap) => format!("unsupported capability: {cap}"),
            Self::ContextOverflow { ctx, needed } => {
                format!("context size {ctx} exceeds limit, needed {needed}")
            }
            Self::ContextCreationFailed(detail) => format!("context creation failed: {detail}"),
            Self::TokenizationFailed(detail) => format!("tokenization failed: {detail}"),
            Self::DecodeFailed(detail) => format!("decoding failed: {detail}"),
            Self::SamplingFailed(detail) => format!("sampling failed: {detail}"),
            Self::Timeout { operation, secs } => {
                format!("{operation} timed out after {secs}s")
            }
            Self::Internal(detail) => format!("internal error: {detail}"),
        }
    }

    pub fn recoverable(&self) -> bool {
        matches!(
            self,
            Self::Cancelled
                | Self::ModelBusy(_)
                | Self::Timeout { .. }
                | Self::BackendUnavailable(_)
        )
    }
}

impl fmt::Display for InferenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}] {} (recoverable: {})",
            self.code(),
            self.message(),
            self.recoverable()
        )
    }
}

impl std::error::Error for InferenceError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display() {
        let err = InferenceError::Cancelled;
        let s = format!("{err}");
        assert!(s.contains("CANCELLED"));
        assert!(s.contains("recoverable: true"));
    }

    #[test]
    fn test_model_not_loaded() {
        let err = InferenceError::ModelNotLoaded("test-model".into());
        assert_eq!(err.code(), "MODEL_NOT_LOADED");
        assert!(err.message().contains("test-model"));
        assert!(!err.recoverable());
    }

    #[test]
    fn test_context_overflow() {
        let err = InferenceError::ContextOverflow {
            ctx: 2048,
            needed: 4096,
        };
        assert_eq!(err.code(), "CONTEXT_OVERFLOW");
        assert!(err.message().contains("2048"));
        assert!(err.message().contains("4096"));
    }

    #[test]
    fn test_recoverable_errors() {
        assert!(InferenceError::Cancelled.recoverable());
        assert!(InferenceError::ModelBusy("m".into()).recoverable());
        assert!(InferenceError::Timeout {
            operation: "gen".into(),
            secs: 30,
        }
        .recoverable());
        assert!(!InferenceError::ModelNotLoaded("m".into()).recoverable());
        assert!(!InferenceError::Internal("oops".into()).recoverable());
    }

    #[test]
    fn test_error_trait() {
        let err = InferenceError::Cancelled;
        let trait_obj: &dyn std::error::Error = &err;
        assert!(!trait_obj.source().is_some());
    }
}
