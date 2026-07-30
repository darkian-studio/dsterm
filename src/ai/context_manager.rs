/// Opaque handle to an allocated inference context.
/// Backend-specific implementations own the underlying resources
/// and handle cleanup in their Drop impls.
pub trait InferenceContext: Send {
    fn n_ctx(&self) -> u32;
}
