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

impl OutputParser for ToolCallParser {
    fn push(&mut self, token: &str) -> Vec<StreamEvent> {
        self.buffer.push_str(token);
        // Tool calls are detected at finish time by downstream JSON parsing
        vec![]
    }

    fn finish(&mut self) -> Vec<StreamEvent> {
        // Attempt to extract tool calls from accumulated text
        vec![]
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
        if let StreamEvent::Token(token) = &events[0] {
            assert_eq!(token.text, "Hello");
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
        assert_eq!(parser.text(), "Hello World");
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
    fn test_tool_call_parser() {
        let mut parser = ToolCallParser::new();
        parser.push("<tool_call>");
        let events = parser.push("{\"name\":\"test\",\"args\":{}}");
        assert!(events.is_empty());
        let events = parser.push("</tool_call>");
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::ToolCall { .. }));
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
        assert_eq!(parser.text(), "");
    }
}
