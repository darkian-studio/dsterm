use crate::ai::context_config::ContextConfig;
use crate::ai::inference_error::InferenceError;

/// Opaque handle to an allocated inference context.
/// Backend-specific implementations own the underlying resources
/// and handle cleanup in their Drop impls.
pub trait InferenceContext: Send {
    fn n_ctx(&self) -> u32;
}

/// Allocates and manages inference contexts for a backend.
/// Initially simple; later (M14) can pool/reuse contexts.
pub struct ContextManager {
    backend_type: String,
}

impl ContextManager {
    pub fn new(backend_type: &str) -> Self {
        Self {
            backend_type: backend_type.to_string(),
        }
    }

    pub fn allocate(
        &self,
        _base: &dyn super::backend_trait::InferenceBackend,
        config: &ContextConfig,
    ) -> Result<Box<dyn InferenceContext>, InferenceError> {
        config.validate()?;
        Err(InferenceError::UnsupportedCapability(
            "ContextManager::allocate requires a llama backend".into(),
        ))
    }

    pub fn backend_type(&self) -> &str {
        &self.backend_type
    }
}
