#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::ai::backend_trait::*;
use crate::ai::context_config::ContextConfig;
use crate::ai::context_manager::InferenceContext;
use crate::ai::inference_error::InferenceError;
use crate::ai::llama::bindings::*;
use crate::ai::llama::{self, backend::LlamaContext, LlamaModel};
use crate::ai::sampler::{LlamaSampler, Sampler, SamplingConfig};

pub struct LlamaBackend {
    model: Arc<LlamaModel>,
}

impl LlamaBackend {
    pub fn new(model: Arc<LlamaModel>) -> Self {
        Self { model }
    }
}

impl InferenceContext for LlamaContext {
    fn n_ctx(&self) -> u32 {
        LlamaContext::n_ctx(self) as u32
    }
}

#[async_trait::async_trait]
impl InferenceBackend for LlamaBackend {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::default()
    }

    fn create_context(&self, config: &ContextConfig) -> BackendResult<Box<dyn InferenceContext>> {
        let params = config.to_llama_params();
        let ctx = self
            .model
            .create_context(params)
            .map_err(InferenceError::context_creation_failed)?;
        Ok(Box::new(ctx))
    }

    async fn validate(&self, _ctx_config: &ContextConfig) -> BackendResult<()> {
        Ok(())
    }

    async fn tokenize(&self, text: &str, add_bos: bool) -> BackendResult<Vec<i32>> {
        let model = self.model.clone();
        let text = text.to_string();
        let ctx = create_default_context(&model).await?;
        ctx.tokenize(&text, add_bos)
            .map_err(InferenceError::tokenization_failed)
    }

    async fn detokenize(&self, token: i32) -> BackendResult<String> {
        let model = self.model.clone();
        let ctx = create_default_context(&model).await?;
        ctx.token_to_piece(token)
            .map_err(InferenceError::tokenization_failed)
    }

    async fn generate(
        &self,
        prompt: &str,
        context_config: ContextConfig,
        sampling_config: SamplingConfig,
        max_tokens: i32,
    ) -> BackendResult<GenerateOutput> {
        context_config.validate()?;

        let model = self.model.clone();
        let prompt = prompt.to_string();
        let cancel = Arc::new(AtomicBool::new(false));

        let handle = tokio::task::spawn_blocking(move || {
            run_generation(
                model,
                prompt,
                context_config,
                sampling_config,
                max_tokens,
                cancel,
                None,
            )
        });
        let inner = handle
            .await
            .map_err(|e| InferenceError::backend_failure(format!("task failed: {e}")))?;
        inner
    }

    async fn generate_streaming(
        &self,
        prompt: &str,
        context_config: ContextConfig,
        sampling_config: SamplingConfig,
        max_tokens: i32,
        sink: Box<dyn TokenSink + Send>,
    ) -> BackendResult<GenerateOutput> {
        context_config.validate()?;

        let model = self.model.clone();
        let prompt = prompt.to_string();
        let cancel = Arc::new(AtomicBool::new(false));

        let handle = tokio::task::spawn_blocking(move || {
            run_generation(
                model,
                prompt,
                context_config,
                sampling_config,
                max_tokens,
                cancel,
                Some(sink),
            )
        });
        let inner = handle
            .await
            .map_err(|e| InferenceError::backend_failure(format!("task failed: {e}")))?;
        inner
    }

    async fn embed(&self, texts: &[String]) -> BackendResult<Vec<Vec<f32>>> {
        let model = self.model.clone();
        let texts = texts.to_vec();
        let handle = tokio::task::spawn_blocking(move || run_embedding(model, &texts));
        handle
            .await
            .map_err(|e| InferenceError::backend_failure(format!("task failed: {e}")))?
    }
}

async fn create_default_context(model: &LlamaModel) -> BackendResult<LlamaContext> {
    let params = ContextConfig::default().to_llama_params();
    model
        .create_context(params)
        .map_err(InferenceError::context_creation_failed)
}

fn run_generation(
    model: Arc<LlamaModel>,
    prompt: String,
    context_config: ContextConfig,
    sampling_config: SamplingConfig,
    max_tokens: i32,
    cancel: Arc<AtomicBool>,
    mut sink: Option<Box<dyn TokenSink + Send>>,
) -> BackendResult<GenerateOutput> {
    let ctx_params = context_config.to_llama_params();
    let mut ctx = model
        .create_context(ctx_params)
        .map_err(InferenceError::context_creation_failed)?;

    let n_ctx = ctx.n_ctx() as usize;
    let eos = ctx.token_eos();
    let bos = ctx.token_bos();

    let mut tokens = ctx
        .tokenize(&prompt, true)
        .map_err(InferenceError::tokenization_failed)?;

    if tokens.is_empty() {
        return Err(InferenceError::new(
            "EMPTY_PROMPT",
            "No tokens generated from prompt",
        ));
    }

    if tokens.len() > n_ctx {
        return Err(InferenceError::max_context_exceeded(n_ctx, tokens.len()));
    }

    let prompt_tokens = tokens.len() as i32;
    let mut generated = 0i32;
    let mut full_text = String::new();
    let mut stopped_by_eos = false;
    let mut stopped_by_max = false;

    let batch =
        unsafe { llama::bindings::llama_batch_get_one(tokens.as_mut_ptr(), tokens.len() as i32) };
    let ret = unsafe { llama::bindings::llama_decode(ctx.ptr_mut(), batch) };
    if ret != 0 {
        return Err(InferenceError::decode_failed(format!(
            "prompt decode: {ret}"
        )));
    }

    let mut n_past = tokens.len();
    let sampler = LlamaSampler::new(ctx.ptr_mut(), sampling_config);

    for _ in 0..max_tokens {
        if cancel.load(Ordering::Relaxed) {
            if let Some(ref mut s) = sink {
                s.on_error("cancelled");
            }
            return Err(InferenceError::cancelled());
        }

        if let Some(ref mut s) = sink {
            if s.is_cancelled() {
                return Err(InferenceError::cancelled());
            }
        }

        if n_past >= n_ctx {
            break;
        }

        let logits = unsafe { llama::bindings::llama_get_logits(ctx.ptr_mut()) };
        if logits.is_null() {
            return Err(InferenceError::decode_failed(
                "llama_get_logits returned null",
            ));
        }

        let n_vocab = ctx.n_vocab();
        let token_id = sampler.sample(logits, n_vocab)?;
        generated += 1;

        if token_id == eos && generated > 1 {
            stopped_by_eos = true;
            break;
        }

        if token_id == bos {
            continue;
        }

        match ctx.token_to_piece(token_id) {
            Ok(piece) => {
                full_text.push_str(&piece);
                if let Some(ref mut s) = sink {
                    let _ = s.on_token(&piece);
                }
            }
            Err(e) => {
                return Err(InferenceError::decode_failed(format!(
                    "token_to_piece: {e}"
                )))
            }
        }

        if generated >= max_tokens {
            stopped_by_max = true;
            break;
        }

        let mut next_tokens = [token_id];
        let batch = unsafe { llama::bindings::llama_batch_get_one(next_tokens.as_mut_ptr(), 1) };
        let ret = unsafe { llama::bindings::llama_decode(ctx.ptr_mut(), batch) };
        if ret != 0 {
            return Err(InferenceError::decode_failed(format!("decode: {ret}")));
        }
        n_past += 1;
    }

    Ok(GenerateOutput {
        text: full_text,
        prompt_tokens,
        completion_tokens: generated,
        stopped_by_eos,
        stopped_by_max_tokens: stopped_by_max,
    })
}

fn run_embedding(model: Arc<LlamaModel>, texts: &[String]) -> BackendResult<Vec<Vec<f32>>> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }

    // Create context with mean pooling for embeddings
    let mut ctx_params = unsafe { llama_context_default_params() };
    ctx_params.n_ctx = 512;
    ctx_params.n_batch = 512;
    ctx_params.n_ubatch = 512;
    ctx_params.pooling_type = 1; // LLAMA_POOLING_TYPE_MEAN
    let mut ctx = model
        .create_context(ctx_params)
        .map_err(InferenceError::context_creation_failed)?;

    let n_embd = unsafe { llama_n_embd(model.ptr()) };
    if n_embd <= 0 {
        return Err(InferenceError::new(
            "EMBEDDING_ERROR",
            "model does not support embeddings (n_embd <= 0)",
        ));
    }

    let mut results: Vec<Vec<f32>> = Vec::with_capacity(texts.len());

    for text in texts {
        let mut tokens = ctx
            .tokenize(text, true)
            .map_err(InferenceError::tokenization_failed)?;

        if tokens.is_empty() {
            results.push(vec![0.0; n_embd as usize]);
            continue;
        }

        let batch =
            unsafe { llama_batch_get_one(tokens.as_mut_ptr(), tokens.len() as i32) };
        let ret = unsafe { llama_decode(ctx.ptr_mut(), batch) };
        if ret != 0 {
            return Err(InferenceError::decode_failed(format!(
                "embedding decode: {ret}"
            )));
        }

        let embeddings = unsafe { llama_get_embeddings(ctx.ptr_mut()) };
        if embeddings.is_null() {
            return Err(InferenceError::new(
                "EMBEDDING_ERROR",
                "llama_get_embeddings returned null",
            ));
        }

        let embd_slice = unsafe { std::slice::from_raw_parts(embeddings, n_embd as usize) };
        results.push(embd_slice.to_vec());
    }

    Ok(results)
}
