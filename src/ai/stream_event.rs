use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    Token {
        event_version: u32,
        text: String,
    },
    Reasoning {
        event_version: u32,
        text: String,
    },
    ToolCall {
        event_version: u32,
        id: String,
        function_name: String,
        arguments: String,
    },
    Done {
        event_version: u32,
    },
}
