use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    Protocol {
        protocol_version: u32,
        backend: String,
        model: String,
    },
    Token {
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
    ToolResult {
        id: String,
        result: String,
    },
    Progress {
        tokens_generated: u32,
        tokens_per_second: f64,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_event() {
        let event = StreamEvent::Token {
            text: "hello".into(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "token");
        assert_eq!(json["text"], "hello");
    }

    #[test]
    fn test_done_event() {
        let event = StreamEvent::Done;
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "done");
    }

    #[test]
    fn test_usage_event() {
        let event = StreamEvent::Usage {
            prompt_tokens: 10,
            completion_tokens: 20,
            total_tokens: 30,
            tokens_per_second: 15.5,
            first_token_ms: 100,
            generation_ms: 2000,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "usage");
        assert_eq!(json["prompt_tokens"], 10);
        assert_eq!(json["completion_tokens"], 20);
    }

    #[test]
    fn test_error_event() {
        let event = StreamEvent::Error {
            message: "oops".into(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "error");
        assert_eq!(json["message"], "oops");
    }

    #[test]
    fn test_reasoning_event() {
        let event = StreamEvent::Reasoning {
            text: "thinking...".into(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "reasoning");
        assert_eq!(json["text"], "thinking...");
    }
}
