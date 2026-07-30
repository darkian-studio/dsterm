use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationStatus {
    Pending,
    Running,
    Cancelling,
    Completed,
    Failed,
}

pub struct GenerationHandle {
    id: String,
    cancel_flag: Arc<AtomicBool>,
    status: Arc<std::sync::Mutex<GenerationStatus>>,
    start_time: Instant,
    tokens_generated: Arc<AtomicU32>,
    model_id: String,
}

impl GenerationHandle {
    pub fn new(id: String, model_id: String) -> Self {
        Self {
            id,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            status: Arc::new(std::sync::Mutex::new(GenerationStatus::Pending)),
            start_time: Instant::now(),
            tokens_generated: Arc::new(AtomicU32::new(0)),
            model_id,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    pub fn cancel(&self) {
        self.cancel_flag.store(true, Ordering::SeqCst);
        if let Ok(mut status) = self.status.lock() {
            if *status == GenerationStatus::Running {
                *status = GenerationStatus::Cancelling;
            }
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel_flag.load(Ordering::Relaxed)
    }

    pub fn set_running(&self) {
        if let Ok(mut status) = self.status.lock() {
            if *status == GenerationStatus::Pending {
                *status = GenerationStatus::Running;
            }
        }
    }

    pub fn set_completed(&self) {
        if let Ok(mut status) = self.status.lock() {
            *status = GenerationStatus::Completed;
        }
    }

    pub fn set_failed(&self) {
        if let Ok(mut status) = self.status.lock() {
            *status = GenerationStatus::Failed;
        }
    }

    pub fn status(&self) -> GenerationStatus {
        self.status.lock().map(|s| *s).unwrap_or(GenerationStatus::Failed)
    }

    pub fn record_token(&self) {
        self.tokens_generated.fetch_add(1, Ordering::Relaxed);
    }

    pub fn tokens_generated(&self) -> u32 {
        self.tokens_generated.load(Ordering::Relaxed)
    }

    pub fn elapsed_secs(&self) -> f64 {
        self.start_time.elapsed().as_secs_f64()
    }

    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.cancel_flag.clone()
    }
}

pub type GenerationHandleRef = Arc<GenerationHandle>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_handle_is_pending() {
        let h = GenerationHandle::new("test-1".into(), "model-x".into());
        assert_eq!(h.status(), GenerationStatus::Pending);
        assert!(!h.is_cancelled());
        assert_eq!(h.tokens_generated(), 0);
        assert_eq!(h.id(), "test-1");
        assert_eq!(h.model_id(), "model-x");
    }

    #[test]
    fn test_lifecycle() {
        let h = GenerationHandle::new("test-2".into(), "model-y".into());
        assert_eq!(h.status(), GenerationStatus::Pending);

        h.set_running();
        assert_eq!(h.status(), GenerationStatus::Running);

        h.set_completed();
        assert_eq!(h.status(), GenerationStatus::Completed);
    }

    #[test]
    fn test_cancel() {
        let h = GenerationHandle::new("test-3".into(), "model-z".into());
        h.set_running();
        h.cancel();
        assert!(h.is_cancelled());
        assert_eq!(h.status(), GenerationStatus::Cancelling);
    }

    #[test]
    fn test_failed() {
        let h = GenerationHandle::new("test-4".into(), "model-w".into());
        h.set_running();
        h.set_failed();
        assert_eq!(h.status(), GenerationStatus::Failed);
    }

    #[test]
    fn test_token_counting() {
        let h = GenerationHandle::new("test-5".into(), "model-v".into());
        assert_eq!(h.tokens_generated(), 0);
        h.record_token();
        assert_eq!(h.tokens_generated(), 1);
        h.record_token();
        h.record_token();
        assert_eq!(h.tokens_generated(), 3);
    }

    #[test]
    fn test_cancel_flag() {
        let h = GenerationHandle::new("test-6".into(), "model-u".into());
        let flag = h.cancel_flag();
        assert!(!flag.load(Ordering::Relaxed));
        h.cancel();
        assert!(flag.load(Ordering::Relaxed));
    }

    #[test]
    fn test_elapsed_time() {
        let h = GenerationHandle::new("test-7".into(), "model-t".into());
        let elapsed = h.elapsed_secs();
        assert!(elapsed >= 0.0);
        assert!(elapsed < 10.0); // should be nearly instant
    }

    #[test]
    fn test_arc_wrapper() {
        let handle = GenerationHandle::new("arc-test".into(), "arc-model".into());
        let handle_ref: GenerationHandleRef = Arc::new(handle);
        assert_eq!(handle_ref.id(), "arc-test");
        assert_eq!(handle_ref.status(), GenerationStatus::Pending);
    }
}
