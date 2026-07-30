#![allow(dead_code)]

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GenerationEvent {
    Protocol {
        protocol_version: u32,
        backend: String,
        model: String,
    },
    Text {
        text: String,
    },
    Reasoning {
        text: String,
    },
    ToolCall {
        id: String,
        function_name: String,
        arguments: String,
    },
    Usage {
        prompt_tokens: i32,
        completion_tokens: i32,
        total_tokens: i32,
        tokens_per_second: f64,
        first_token_ms: u64,
        generation_ms: u64,
    },
    Done,
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Idle,
    Generating,
    Cancelling,
    Closed,
}

impl SessionState {
    pub fn can_transition_to(&self, next: SessionState) -> bool {
        use SessionState::*;
        matches!(
            (self, next),
            (Idle, Generating)
                | (Generating, Cancelling | Closed | Idle)
                | (Cancelling, Closed | Idle)
        )
    }

    pub fn is_active(&self) -> bool {
        matches!(self, SessionState::Generating)
    }
}
