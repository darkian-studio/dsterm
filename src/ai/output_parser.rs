#![allow(dead_code)]

use crate::ai::stream_event::StreamEvent;

pub trait OutputParser: Send {
    fn push(&mut self, token: &str) -> Vec<StreamEvent>;
    fn finish(&mut self) -> Vec<StreamEvent>;
    fn reset(&mut self);
}

pub struct ToolCallParser {
    buffer: String,
}

impl ToolCallParser {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
        }
    }
}

/// Extract tool calls from model output text.
///
/// Supports three formats commonly emitted by local models:
///
/// 1. `<tool_call>{"name":"func","arguments":{...}}</tool_call>` — XML-style tags
/// 2. `[TOOL_CALL] {"name":"func","arguments":{...}}` — marker-prefixed JSON
/// 3. Standalone JSON objects containing `"name"` and `"arguments"` keys
///    (some models emit function calls as raw JSON on their own line)
pub fn extract_tool_calls(text: &str) -> Vec<StreamEvent> {
    let mut events = Vec::new();

    // Pattern 1: <tool_call>...</tool_call>
    let mut remaining = text;
    loop {
        let start_tag = "<tool_call>";
        let end_tag = "</tool_call>";
        if let Some(start) = remaining.find(start_tag) {
            let after_start = &remaining[start + start_tag.len()..];
            if let Some(end) = after_start.find(end_tag) {
                let json_str = &after_start[..end];
                if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(json_str) {
                    let name = json_val
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown_tool");
                    let args = json_val
                        .get("arguments")
                        .or_else(|| json_val.get("args"))
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "{}".to_string());
                    let id = json_val
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("call_")
                        .to_string();
                    events.push(StreamEvent::ToolCall {
                        event_version: 1,
                        id,
                        function_name: name.to_string(),
                        arguments: args,
                    });
                }
                remaining = &after_start[end + end_tag.len()..];
            } else {
                break;
            }
        } else {
            break;
        }
    }

    // Pattern 2: [TOOL_CALL] {"name":"...","arguments":{...}}
    let remaining = text;
    for line in remaining.lines() {
        let trimmed = line.trim();
        if let Some(marker) = trimmed.strip_prefix("[TOOL_CALL]") {
            let json_str = marker.trim();
            if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(json_str) {
                let name = json_val
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown_tool");
                let args = json_val
                    .get("arguments")
                    .or_else(|| json_val.get("args"))
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "{}".to_string());
                let id = json_val
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("call_")
                    .to_string();
                if !events.iter().any(|e| matches!(e, StreamEvent::ToolCall { function_name, .. } if function_name == name)) {
                    events.push(StreamEvent::ToolCall {
                        event_version: 1,
                        id,
                        function_name: name.to_string(),
                        arguments: args,
                    });
                }
            }
        }
    }

    // Pattern 3: standalone JSON on its own line with "name" and "arguments"
    for line in remaining.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("<tool_call>") || trimmed.starts_with("[TOOL_CALL]") {
            continue;
        }
        if trimmed.starts_with('{') && trimmed.ends_with('}') && !events.is_empty() {
            if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(trimmed) {
                if json_val.get("name").and_then(|v| v.as_str()).is_some()
                    && json_val.get("arguments").is_some()
                {
                    let name = json_val["name"].as_str().unwrap_or("unknown_tool");
                    let args = json_val["arguments"].to_string();
                    let id = json_val
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("call_")
                        .to_string();
                    if !events.iter().any(|e| matches!(e, StreamEvent::ToolCall { function_name, .. } if function_name == name)) {
                        events.push(StreamEvent::ToolCall {
                            event_version: 1,
                            id,
                            function_name: name.to_string(),
                            arguments: args,
                        });
                    }
                }
            }
        }
    }

    events
}

impl OutputParser for ToolCallParser {
    fn push(&mut self, token: &str) -> Vec<StreamEvent> {
        self.buffer.push_str(token);
        let events = extract_tool_calls(&self.buffer);
        if !events.is_empty() {
            let mut last_end = 0;
            let mut search_from = 0;
            while let Some(start) = self.buffer[search_from..].find("<tool_call>") {
                let abs_start = search_from + start;
                let after_start = abs_start + "<tool_call>".len();
                if let Some(end) = self.buffer[after_start..].find("</tool_call>") {
                    let abs_end = after_start + end + "</tool_call>".len();
                    if abs_end > last_end {
                        last_end = abs_end;
                    }
                    search_from = abs_end;
                } else {
                    break;
                }
            }
            if last_end > 0 {
                self.buffer = self.buffer[last_end..].to_string();
            }
            events
        } else {
            vec![]
        }
    }

    fn finish(&mut self) -> Vec<StreamEvent> {
        let events = extract_tool_calls(&self.buffer);
        self.buffer.clear();
        events
    }

    fn reset(&mut self) {
        self.buffer.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_tool_calls_standalone() {
        let text = r#"
Some text before.
<tool_call>{"name": "search_web", "arguments": {"query": "rust async"}}</tool_call>
Some text after."#;
        let events = extract_tool_calls(text);
        assert_eq!(events.len(), 1);
        if let StreamEvent::ToolCall {
            function_name,
            arguments,
            ..
        } = &events[0]
        {
            assert_eq!(function_name, "search_web");
            assert!(arguments.contains("rust async"));
        } else {
            panic!("expected ToolCall event");
        }
    }

    #[test]
    fn test_tool_call_parser_marker_format() {
        let mut parser = ToolCallParser::new();
        parser.push("I'll look that up for you.\n");
        let events = parser
            .push("[TOOL_CALL] {\"name\":\"search_web\",\"arguments\":{\"query\":\"rust async\"}}");
        assert_eq!(events.len(), 1);
        if let StreamEvent::ToolCall {
            function_name,
            ..
        } = &events[0]
        {
            assert_eq!(function_name, "search_web");
        } else {
            panic!("expected ToolCall event");
        }
    }

    #[test]
    fn test_tool_call_parser_xml_tag() {
        let mut parser = ToolCallParser::new();
        parser.push("Let me look that up.\n");
        let events = parser.push(
            r#"<tool_call>{"name": "search_web", "arguments": {"query": "test"}}</tool_call>"#,
        );
        assert_eq!(events.len(), 1);
        if let StreamEvent::ToolCall {
            function_name,
            arguments,
            ..
        } = &events[0]
        {
            assert_eq!(function_name, "search_web");
            assert!(arguments.contains("test"));
        } else {
            panic!("expected ToolCall event");
        }
    }

    #[test]
    fn test_tool_call_parser_multiple_calls() {
        let mut parser = ToolCallParser::new();
        let text = r#"<tool_call>{"name": "search_web", "arguments": {"query": "rust"}}</tool_call>
<tool_call>{"name": "get_weather", "arguments": {"city": "NYC"}}</tool_call>"#;
        let events = parser.push(text);
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn test_tool_call_parser_no_tool_call() {
        let mut parser = ToolCallParser::new();
        let events = parser.push("Hello, how are you?");
        assert!(events.is_empty());
    }

    #[test]
    fn test_reset() {
        let mut parser = ToolCallParser::new();
        parser.push("Hello");
        parser.reset();
        parser.push("World");
        let events = parser.finish();
        assert!(events.is_empty(), "reset should clear buffer");
    }
}
