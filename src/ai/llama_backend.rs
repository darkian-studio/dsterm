#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::ai::backend_trait::*;
use crate::ai::context_config::ContextConfig;
use crate::ai::context_manager::InferenceContext;
use crate::ai::inference_error::InferenceError;
use crate::ai::inference_request::{InferenceMode, InferenceRequest};
use crate::ai::llama::bindings::*;
use crate::ai::llama::{self, backend::LlamaContext, LlamaModel};
use crate::ai::sampler::{LlamaSampler, Sampler, SamplingConfig};

/// D1: reasoning off by default -- direct answers for an inline coding
/// assistant. Flip to true to surface the model's chain of thought (which
/// then gets stripped per D2).
const ENABLE_THINKING: bool = false;

/// D3: pocketpal-ai's static cross-model-family stop list, ported verbatim.
/// Costs nothing to carry; dsterm may load other model families later.
const FALLBACK_STOPS: &[&str] = &[
    "</s>",
    "<|eot_id|>",
    "<|end_of_text|>",
    "<|im_end|>",
    "<|EOT|>",
    "<|END_OF_TURN_TOKEN|>",
    "<|end_of_turn|>",
    "<end_of_turn>",
    "<|endoftext|>",
    "<|return|>",
    "<|END_RESPONSE|>",
];

/// What `run_generation` needs beyond the raw prompt string: template-implied
/// stop strings and the model's thinking tags (for D2 stripping).
pub struct GenerationPrompt {
    pub text: String,
    pub stops: Vec<String>,
    pub thinking_start_tag: Option<String>,
    pub thinking_end_tags: Vec<String>,
}

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
        let params = config.to_dsterm_config();
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
        request: &InferenceRequest,
        context_config: ContextConfig,
        sampling_config: SamplingConfig,
        max_tokens: i32,
    ) -> BackendResult<GenerateOutput> {
        context_config.validate()?;

        let model = self.model.clone();
        let prompt = self.resolve_prompt(request, &context_config)?;
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
        request: &InferenceRequest,
        context_config: ContextConfig,
        sampling_config: SamplingConfig,
        max_tokens: i32,
        sink: Box<dyn TokenSink + Send>,
    ) -> BackendResult<GenerateOutput> {
        context_config.validate()?;

        let model = self.model.clone();
        let prompt = self.resolve_prompt(request, &context_config)?;
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
    let params = ContextConfig::default().to_dsterm_config();
    model
        .create_context(params)
        .map_err(InferenceError::context_creation_failed)
}

impl LlamaBackend {
    /// Resolve the request into the prompt that actually gets tokenized.
    /// Chat mode uses the model's native chat template (llama.cpp's Jinja2
    /// engine, wrapped by the shim); everything else keeps the legacy
    /// resolution (prompt / FIM prefix-suffix).
    fn resolve_prompt(
        &self,
        request: &InferenceRequest,
        context_config: &ContextConfig,
    ) -> BackendResult<GenerationPrompt> {
        if !matches!(request.mode, InferenceMode::Chat) {
            return Ok(GenerationPrompt {
                text: request.resolved_prompt_fim(None, Some(&request.architecture)),
                stops: Vec::new(),
                thinking_start_tag: None,
                thinking_end_tags: Vec::new(),
            });
        }
        self.resolve_chat_prompt(request, context_config)
    }

    fn resolve_chat_prompt(
        &self,
        request: &InferenceRequest,
        context_config: &ContextConfig,
    ) -> BackendResult<GenerationPrompt> {
        let legacy = || GenerationPrompt {
            text: request.resolved_prompt(None),
            stops: FALLBACK_STOPS.iter().map(|s| s.to_string()).collect(),
            thinking_start_tag: None,
            thinking_end_tags: Vec::new(),
        };

        let Some(templates) = self.model.chat_templates() else {
            return Ok(legacy());
        };

        let messages = assemble_messages(request);
        // Native chat templates know nothing about tool definitions, so
        // mirror the legacy path: append the tool instructions to the
        // (merged) system message before applying the template.
        let mut messages = if let Some(tool_msg) = request.tool_instruction_message() {
            let mut out = messages;
            match out.first_mut() {
                Some((role, content)) if role.as_str() == "system" => {
                    content.push('\n');
                    content.push_str(&tool_msg);
                }
                _ => out.insert(0, ("system".to_string(), tool_msg)),
            }
            out
        } else {
            messages
        };

        // Drop the oldest turns until the rendered prompt fits the context
        // window (llama-server style). A counting context with the same
        // config as the generation context keeps the budgets consistent.
        let ctx = self
            .model
            .create_context(context_config.to_dsterm_config())
            .map_err(InferenceError::context_creation_failed)?;

        loop {
            match templates.apply(&messages, ENABLE_THINKING) {
                Ok(out) => {
                    let tokens = ctx
                        .tokenize(&out.prompt, true)
                        .map_err(InferenceError::tokenization_failed)?;
                    if tokens.len() <= ctx.n_ctx() as usize || !drop_oldest_turn(&mut messages) {
                        let mut stops = out.additional_stops;
                        stops.extend(FALLBACK_STOPS.iter().map(|s| s.to_string()));
                        return Ok(GenerationPrompt {
                            text: out.prompt,
                            stops,
                            thinking_start_tag: out.thinking_start_tag,
                            thinking_end_tags: out.thinking_end_tags,
                        });
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "chat template apply failed ({e}); falling back to legacy formatting"
                    );
                    return Ok(legacy());
                }
            }
        }
    }
}

/// Drop the oldest non-system turn. If that leaves an assistant reply whose
/// preceding user turn was removed, drop it too (it would be orphaned). Never
/// drops the final user message. Returns false when nothing can be dropped.
fn drop_oldest_turn(messages: &mut Vec<(String, String)>) -> bool {
    let drop_start = if messages.first().is_some_and(|(r, _)| r == "system") {
        1
    } else {
        0
    };
    if messages.len() - drop_start <= 1 {
        return false;
    }
    messages.drain(drop_start..drop_start + 1);
    while messages
        .first()
        .is_some_and(|(role, _)| role.as_str() == "assistant")
    {
        messages.remove(0);
    }
    true
}

/// Merge every system message into a single leading one (strict chat
/// templates raise on a second system message), then pass the turns through
/// in order.
fn assemble_messages(request: &InferenceRequest) -> Vec<(String, String)> {
    let mut system = String::new();
    let mut turns = Vec::with_capacity(request.messages.len());
    for m in &request.messages {
        if m.role == "system" {
            if !system.is_empty() {
                system.push('\n');
            }
            system.push_str(&m.content);
        } else {
            turns.push((m.role.clone(), m.content.clone()));
        }
    }
    if !system.is_empty() {
        let mut out = Vec::with_capacity(turns.len() + 1);
        out.push(("system".to_string(), system));
        out.extend(turns);
        out
    } else {
        turns
    }
}

/// If `text` ends with any stop string, return the index where the stop
/// begins so the caller can truncate it away.
fn match_stop(text: &str, stops: &[String]) -> Option<usize> {
    for stop in stops {
        if stop.is_empty() {
            continue;
        }
        if text.ends_with(stop.as_str()) {
            return Some(text.len() - stop.len());
        }
    }
    None
}

/// D2: strip a thinking block using the model's actual tags (not
/// pattern-guessed ones). Removes [start .. first end tag]; an unclosed
/// block is removed to the end of the text.
fn strip_thinking(mut text: String, start_tag: &Option<String>, end_tags: &[String]) -> String {
    let Some(start) = start_tag else {
        return text;
    };
    let Some(start_pos) = text.find(start.as_str()) else {
        return text;
    };

    let after = &text[start_pos + start.len()..];
    let end_pos = end_tags
        .iter()
        .filter_map(|t| {
            after
                .find(t.as_str())
                .map(|i| start_pos + start.len() + i + t.len())
        })
        .min()
        .unwrap_or(text.len());

    text.replace_range(start_pos..end_pos, "");
    text
}

fn run_generation(
    model: Arc<LlamaModel>,
    gen: GenerationPrompt,
    context_config: ContextConfig,
    sampling_config: SamplingConfig,
    max_tokens: i32,
    cancel: Arc<AtomicBool>,
    mut sink: Option<Box<dyn TokenSink + Send>>,
) -> BackendResult<GenerateOutput> {
    let ctx_params = context_config.to_dsterm_config();
    let mut ctx = model
        .create_context(ctx_params)
        .map_err(InferenceError::context_creation_failed)?;

    let n_ctx = ctx.n_ctx() as usize;
    let eos = ctx.token_eos();
    let bos = ctx.token_bos();

    // The chat template (if any) has already been applied by resolve_prompt;
    // BOS is added exactly once here (add_bos = true).
    let mut tokens = ctx
        .tokenize(&gen.text, true)
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

    // llama_decode asserts when a single batch exceeds n_batch, so decode the
    // prompt in n_batch-sized chunks (the KV cache appends them sequentially).
    let n_batch = context_config.n_batch.max(1) as usize;
    for chunk in tokens.chunks_mut(n_batch) {
        let batch =
            unsafe { llama::bindings::llama_batch_get_one(chunk.as_mut_ptr(), chunk.len() as i32) };
        let ret = unsafe { llama::bindings::llama_decode(ctx.ptr_mut(), batch) };
        if ret != 0 {
            return Err(InferenceError::decode_failed(format!(
                "prompt decode: {ret}"
            )));
        }
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

        let logits = unsafe { llama::bindings::dsterm_llama_get_logits(ctx.ptr_mut()) };
        if logits.is_null() {
            return Err(InferenceError::decode_failed(
                "dsterm_llama_get_logits returned null",
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
                // String-based stop matching against the growing decoded
                // text: template-implied stops plus the static fallback list.
                if let Some(cut) = match_stop(&full_text, &gen.stops) {
                    full_text.truncate(cut);
                    break;
                }
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
        // llama_decode takes the batch by value and frees its arrays itself
        // (llama-context.cpp: llama_decode -> llama_batch_free).
        let ret = unsafe { llama::bindings::llama_decode(ctx.ptr_mut(), batch) };
        if ret != 0 {
            return Err(InferenceError::decode_failed(format!("decode: {ret}")));
        }
        n_past += 1;
    }

    // Strip a thinking block (if any) before the text is returned, so it
    // never pollutes stored history for the next turn.
    let text = strip_thinking(full_text, &gen.thinking_start_tag, &gen.thinking_end_tags);

    Ok(GenerateOutput {
        text,
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
    // D4: modern llama.cpp configures pooling and embedding extraction through
    // the context params (llama_set_pooling_type no longer exists).
    let ctx_config = crate::ai::llama::bindings::DstermCtxConfig {
        n_ctx: 512,
        n_batch: 512,
        n_ubatch: 512,
        n_threads: 4,
        n_threads_batch: 4,
        pooling_type: 1, // LLAMA_POOLING_TYPE_MEAN
        embeddings: true,
        flash_attn: false,
        offload_kqv: true,
        rope_scaling_type: 0,
    };
    // Embedding contexts use their own config (512 batch); chunk the
    // decode so a long text never exceeds llama_decode's n_batch assert.
    let n_batch = ctx_config.n_batch.max(1) as usize;
    let mut ctx = model
        .create_context(ctx_config)
        .map_err(InferenceError::context_creation_failed)?;

    let n_embd = unsafe { llama::bindings::dsterm_llama_n_embd(model.ptr()) };
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

        // Embedding contexts use their own config (512 batch); chunk the
        // decode so a long text never exceeds llama_decode's n_batch assert.
        for chunk in tokens.chunks_mut(n_batch) {
            let batch = unsafe { llama_batch_get_one(chunk.as_mut_ptr(), chunk.len() as i32) };
            let ret = unsafe { llama_decode(ctx.ptr_mut(), batch) };
            if ret != 0 {
                return Err(InferenceError::decode_failed(format!(
                    "embedding decode: {ret}"
                )));
            }
        }

        let embeddings = unsafe { llama::bindings::dsterm_llama_get_embeddings(ctx.ptr_mut()) };
        if embeddings.is_null() {
            return Err(InferenceError::new(
                "EMBEDDING_ERROR",
                "dsterm_llama_get_embeddings returned null",
            ));
        }

        let embd_slice = unsafe { std::slice::from_raw_parts(embeddings, n_embd as usize) };
        results.push(embd_slice.to_vec());
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::drop_oldest_turn;

    fn turns() -> Vec<(String, String)> {
        vec![
            ("user".to_string(), "u1".to_string()),
            ("assistant".to_string(), "a1".to_string()),
            ("user".to_string(), "u2".to_string()),
        ]
    }

    #[test]
    fn drops_oldest_turn_and_orphaned_assistant() {
        let mut m = turns();
        assert!(drop_oldest_turn(&mut m));
        assert_eq!(m, vec![("user".to_string(), "u2".to_string())]);
    }

    #[test]
    fn keeps_system_prompt() {
        let mut m = turns();
        m.insert(0, ("system".to_string(), "sys".to_string()));
        assert!(drop_oldest_turn(&mut m));
        assert_eq!(
            m,
            vec![
                ("system".to_string(), "sys".to_string()),
                ("user".to_string(), "u2".to_string()),
            ]
        );
    }

    #[test]
    fn never_drops_last_user_message() {
        let mut m = vec![("user".to_string(), "u1".to_string())];
        assert!(!drop_oldest_turn(&mut m));
        assert_eq!(m, vec![("user".to_string(), "u1".to_string())]);
    }
}
