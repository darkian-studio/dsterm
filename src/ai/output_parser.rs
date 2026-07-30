use crate::ai::stream_event::StreamEvent;

pub trait OutputParser: Send {
    fn push(&mut self, token: &str) -> Vec<StreamEvent>;
    fn finish(&mut self) -> Vec<StreamEvent>;
    fn reset(&mut self);
}

pub struct TextParser {
    buffer: String,
}

impl TextParser {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
        }
    }
}

impl OutputParser for TextParser {
    fn push(&mut self, token: &str) -> Vec<StreamEvent> {
        self.buffer.push_str(token);
        vec![StreamEvent::Token {
            event_version: 1,
            text: token.to_string(),
        }]
    }

    fn finish(&mut self) -> Vec<StreamEvent> {
        if self.buffer.is_empty() {
            return vec![];
        }
        vec![StreamEvent::Done {
            event_version: 1,
        }]
    }

    fn reset(&mut self) {
        self.buffer.clear();
    }
}

pub struct ReasoningParser {
    inner: TextParser,
    reasoning_tag: String,
    in_reasoning: bool,
}

impl ReasoningParser {
    pub fn new(tag: &str) -> Self {
        Self {
            inner: TextParser::new(),
            reasoning_tag: tag.to_string(),
            in_reasoning: false,
        }
    }
}

impl OutputParser for ReasoningParser {
    fn push(&mut self, token: &str) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        if token.contains(&self.reasoning_tag) {
            self.in_reasoning = !self.in_reasoning;
            if self.in_reasoning {
                events.push(StreamEvent::Reasoning {
                    event_version: 1,
                    text: "".into(),
                });
            }
            return events;
        }
        if self.in_reasoning {
            events.push(StreamEvent::Reasoning {
                event_version: 1,
                text: token.to_string(),
            });
        } else {
            events.extend(self.inner.push(token));
        }
        events
    }

    fn finish(&mut self) -> Vec<StreamEvent> {
        self.inner.finish()
    }

    fn reset(&mut self) {
        self.inner.reset();
        self.in_reasoning = false;
    }
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
    // (some models use this format instead of XML tags)
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
                // Avoid duplicates when Pattern 1 already matched
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
    // e.g. `{"name": "get_weather", "arguments": {"city": "NYC"}}`
    for line in remaining.lines() {
        let trimmed = line.trim();
        // Skip lines already matched by Pattern 1 or 2
        if trimmed.starts_with("<tool_call>") || trimmed.starts_with("[TOOL_CALL]") {
            continue;
        }
        if trimmed.starts_with('{') && trimmed.ends_with('}') && !events.is_empty() {
            // Only match if we already have at least one tool call detected
            // (to avoid false positives from regular JSON in conversation)
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
        // Try incremental extraction — if we see a complete tool_call found in the buffer, emit it
        let events = extract_tool_calls(&self.buffer);
        if !events.is_empty() {
            // Remove extracted tool calls from the buffer to avoid re-emitting
            // Find the last closing tag/brace and trim everything before it
            let mut last_end = 0;
            let mut search_from = 0;
            loop {
                if let Some(start) = self.buffer[search_from..].find("<tool_call>") {
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

pub struct ChainedParser {
    parsers: Vec<Box<dyn OutputParser>>,
}

impl ChainedParser {
    pub fn new(parsers: Vec<Box<dyn OutputParser>>) -> Self {
        Self { parsers }
    }
}

impl OutputParser for ChainedParser {
    fn push(&mut self, token: &str) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        for parser in &mut self.parsers {
            events.extend(parser.push(token));
        }
        events
    }

    fn finish(&mut self) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        for parser in &mut self.parsers {
            events.extend(parser.finish());
        }
        events
    }

    fn reset(&mut self) {
        for parser in &mut self.parsers {
            parser.reset();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_parser_simple() {
        let mut parser = TextParser::new();
        let events = parser.push("Hello");
        assert_eq!(events.len(), 1);
        if let StreamEvent::Token { text, .. } = &events[0] {
            assert_eq!(text, "Hello");
        } else {
            panic!("expected Token event");
        }
    }

    #[test]
    fn test_text_parser_multiple_tokens() {
        let mut parser = TextParser::new();
        parser.push("Hello ");
        parser.push("World");
        let events = parser.finish();
        assert_eq!(events.len(), 1);
        if let StreamEvent::Done { .. } = &events[0] {
            // ok
        } else {
            panic!("expected Done event");
        }
    }

    #[test]
    fn test_reasoning_parser_capture() {
        let mut parser = ReasoningParser::new("reasoning");
        let events = parser.push("normal text ");
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::Token { .. }));

        let events = parser.push("<reasoning>");
        assert!(events.is_empty()); // token consumed by opening tag

        let events = parser.push("thinking...");
        assert!(events.is_empty()); // captured as reasoning

        let events = parser.push("</reasoning>");
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::Reasoning { .. }));
    }

    #[test]
    fn test_tool_call_parser_xml_tag() {
        let mut parser = ToolCallParser::new();
        let events = parser.push("Let me check the weather.");
        assert!(events.is_empty());
        let events = parser.push("<tool_call>{\"name\":\"get_weather\",\"arguments\":{\"city\":\"NYC\"}}</tool_call>");
        assert_eq!(events.len(), 1);
        if let StreamEvent::ToolCall { function_name, arguments, .. } = &events[0] {
            assert_eq!(function_name, "get_weather");
            assert!(arguments.contains("NYC"));
        } else {
            panic!("expected ToolCall event");
        }
    }

    #[test]
    fn test_tool_call_parser_marker_format() {
        let mut parser = ToolCallParser::new();
        parser.push("I'll look that up for you.\n");
        let events = parser.push("[TOOL_CALL] {\"name\":\"search_web\",\"arguments\":{\"query\":\"rust async\"}}");
        assert_eq!(events.len(), 1);
        if let StreamEvent::ToolCall { function_name, arguments, .. } = &events[0] {
            assert_eq!(function_name, "search_web");
        } else {
            panic!("expected ToolCall event");
        }
    }

    #[test]
    fn test_tool_call_parser_no_tool_call() {
        let mut parser = ToolCallParser::new();
        parser.push("Hello, how can I help you today?");
        let events = parser.finish();
        assert!(events.is_empty());
    }

    #[test]
    fn test_tool_call_parser_multiple_calls() {
        let mut parser = ToolCallParser::new();
        parser.push("Let me check both.\n");
        parser.push("<tool_call>{\"name\":\"get_weather\",\"arguments\":{\"city\":\"NYC\"}}</tool_call>");
        let events = parser.push("<tool_call>{\"name\":\"get_time\",\"arguments\":{\"timezone\":\"EST\"}}</tool_call>");
        // Second push should detect the second call (first was already emitted and cleared from buffer)
        assert_eq!(events.len(), 1);
        // finish should return any remaining
        let remaining = parser.finish();
        assert_eq!(remaining.len(), 0);
    }

    #[test]
    fn test_extract_tool_calls_standalone() {
        let text = r#"I'll search for that.
{"name": "search_web", "arguments": {"query": "test"}}"#;
        let events = extract_tool_calls(text);
        assert!(events.is_empty(), "standalone JSON without prior context should not match");
    }

    #[test]
    fn test_chained_parser() {
        let parsers: Vec<Box<dyn OutputParser>> = vec![
            Box::new(TextParser::new()),
            Box::new(ReasoningParser::new("think")),
        ];
        let mut chained = ChainedParser::new(parsers);
        let events = chained.push("hello");
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::Token { .. }));
    }

    #[test]
    fn test_reset() {
        let mut parser = TextParser::new();
        parser.push("Hello");
        parser.reset();
        parser.push("World");
        let events = parser.finish();
        assert!(events.len() == 1, "reset should clear buffer");
    }
}
