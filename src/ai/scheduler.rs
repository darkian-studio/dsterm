use std::sync::Arc;

use crate::ai::backend_trait::{GenerateOutput, InferenceBackend};
use crate::ai::generation_handle::{GenerationHandle, GenerationHandleRef};
use crate::ai::inference_error::InferenceError;
use crate::ai::inference_request::InferenceRequest;

pub type SchedulerResult<T> = Result<T, InferenceError>;

#[async_trait::async_trait]
pub trait Scheduler: Send + Sync {
    async fn submit(
        &self,
        request: InferenceRequest,
        backend: Arc<dyn super::backend_trait::InferenceBackend>,
    ) -> SchedulerResult<GenerationHandleRef>;
    async fn cancel(&self, id: &str) -> SchedulerResult<()>;
    fn status(&self, id: &str) -> Option<GenerationHandleRef>;
}

pub struct ImmediateScheduler {
    handles: tokio::sync::RwLock<std::collections::HashMap<String, GenerationHandleRef>>,
}

impl ImmediateScheduler {
    pub fn new() -> Self {
        Self {
            handles: tokio::sync::RwLock::new(std::collections::HashMap::new()),
        }
    }

    /// Run a generation synchronously (blocking) and return the output.
    /// This is for HTTP endpoints that need a single response.
    pub async fn execute_sync(
        request: &InferenceRequest,
        backend: Arc<dyn InferenceBackend>,
    ) -> SchedulerResult<GenerateOutput> {
        let config = request.to_context_config();
        let sampling = request.to_sampling_config();
        let prompt = request.resolved_prompt(None);

        config.validate()?;

        let handle = Arc::new(GenerationHandle::new(
            uuid::Uuid::new_v4().to_string(),
            request.model_id.clone(),
        ));

        handle.set_running();

        let cancel_flag = handle.cancel_flag();
        let _sink = Box::new(ImmediateSink {
            handle: handle.clone(),
            cancel: cancel_flag,
        });

        let result = backend
            .generate(&prompt, config, sampling, request.max_tokens)
            .await;

        match result {
            Ok(output) => {
                handle.set_completed();
                Ok(output)
            }
            Err(e) => {
                handle.set_failed();
                Err(e)
            }
        }
    }

    /// Run a streaming generation (blocking until done).
    pub async fn execute_sync_stream(
        request: &InferenceRequest,
        backend: Arc<dyn InferenceBackend>,
    ) -> SchedulerResult<GenerateOutput> {
        let config = request.to_context_config();
        let sampling = request.to_sampling_config();
        let prompt = request.resolved_prompt(None);

        config.validate()?;

        let handle = Arc::new(GenerationHandle::new(
            uuid::Uuid::new_v4().to_string(),
            request.model_id.clone(),
        ));

        handle.set_running();

        let cancel_flag = handle.cancel_flag();
        let sink = Box::new(ImmediateSink {
            handle: handle.clone(),
            cancel: cancel_flag.clone(),
        });

        let result = backend
            .generate_streaming(&prompt, config, sampling, request.max_tokens, sink)
            .await;

        match result {
            Ok(output) => {
                handle.set_completed();
                Ok(output)
            }
            Err(e) => {
                handle.set_failed();
                Err(e)
            }
        }
    }

    async fn execute_generation(
        handle: GenerationHandleRef,
        request: InferenceRequest,
        backend: Arc<dyn super::backend_trait::InferenceBackend>,
    ) -> SchedulerResult<()> {
        handle.set_running();
        let config = request.to_context_config();
        let sampling = request.to_sampling_config();
        let prompt = request.resolved_prompt(None);
        let _ctx = backend.create_context(&config)?;

        let result = if request.stream {
            let cancel_flag = handle.cancel_flag();
            let handle_clone = handle.clone();
            let sink = Box::new(crate::ai::scheduler::ImmediateSink {
                handle: handle_clone,
                cancel: cancel_flag,
            });
            backend
                .generate_streaming(&prompt, config, sampling, request.max_tokens, sink)
                .await
        } else {
            backend
                .generate(&prompt, config, sampling, request.max_tokens)
                .await
        };

        match result {
            Ok(_) => handle.set_completed(),
            Err(e) => {
                handle.set_failed();
                return Err(e);
            }
        }
        Ok(())
    }
}

pub struct ImmediateSink {
    handle: GenerationHandleRef,
    cancel: Arc<std::sync::atomic::AtomicBool>,
}

impl super::backend_trait::TokenSink for ImmediateSink {
    fn on_token(&mut self, _token: &str) -> Result<(), String> {
        if self.cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        self.handle.record_token();
        Ok(())
    }

    fn on_error(&mut self, err: &str) {
        self.handle.set_failed();
        tracing::error!("Generation error: {err}");
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[async_trait::async_trait]
impl Scheduler for ImmediateScheduler {
    async fn submit(
        &self,
        request: InferenceRequest,
        backend: Arc<dyn super::backend_trait::InferenceBackend>,
    ) -> SchedulerResult<GenerationHandleRef> {
        let id = uuid::Uuid::new_v4().to_string();
        let handle = Arc::new(GenerationHandle::new(
            id.clone(),
            request.model_id.clone(),
        ));

        self.handles.write().await.insert(id.clone(), handle.clone());

        let h = handle.clone();
        let req = request;
        let bk = backend;
        tokio::spawn(async move {
            let _ = Self::execute_generation(h, req, bk).await;
        });

        Ok(handle)
    }

    async fn cancel(&self, id: &str) -> SchedulerResult<()> {
        let handle = {
            let guard = self.handles.read().await;
            guard.get(id).cloned()
        };
        if let Some(h) = handle {
            h.cancel();
            Ok(())
        } else {
            Err(InferenceError::Internal(format!(
                "generation not found: {id}"
            )))
        }
    }

    fn status(&self, id: &str) -> Option<GenerationHandleRef> {
        let guard = self.handles.try_read().ok()?;
        guard.get(id).cloned()
    }
}
