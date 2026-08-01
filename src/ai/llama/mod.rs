#[cfg(feature = "llama")]
pub mod backend;
#[cfg(feature = "llama")]
pub mod bindings;
#[cfg(feature = "llama")]
pub mod chat_template;

#[cfg(feature = "llama")]
pub use backend::LlamaModel;
