use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceMode {
    Chat,
    Completion,
    Fim,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallData>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallData {
    pub id: String,
    pub function_name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceRequest {
    pub mode: InferenceMode,
    pub model_id: String,
    pub messages: Vec<InferenceMessage>,

    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub suffix: String,

    #[serde(default)]
    pub temperature: f32,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: i32,
    #[serde(default)]
    pub top_p: f32,
    #[serde(default)]
    pub top_k: i32,
    #[serde(default)]
    pub min_p: f32,
    #[serde(default)]
    pub repeat_penalty: f32,
    #[serde(default)]
    pub frequency_penalty: f32,
    #[serde(default)]
    pub presence_penalty: f32,

    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub n_ctx: u32,
    #[serde(default)]
    pub n_batch: u32,
    #[serde(default)]
    pub n_threads: i32,
    #[serde(default)]
    pub tools: Vec<ToolCallData>,
    /// Tool definitions in OpenAI-compatible JSON schema format.
    /// Passed from the client so the model knows what tools are available.
    #[serde(default)]
    pub tool_definitions: Vec<serde_json::Value>,
    /// Model architecture (e.g. "llama", "qwen2", "deepseek2").
    /// Used for template selection. Set by the backend after model resolution.
    #[serde(default)]
    pub architecture: String,
    #[serde(default)]
    pub metadata: Value,
    #[serde(default)]
    pub stream: bool,
}

fn default_max_tokens() -> i32 {
    512
}

impl InferenceRequest {
    /// Convert a `serde_json::Value` message to `InferenceMessage`.
    fn message_from_value(msg: &Value) -> InferenceMessage {
        InferenceMessage {
            role: msg["role"].as_str().unwrap_or("user").to_string(),
            content: msg["content"].as_str().unwrap_or("").to_string(),
            tool_call_id: msg
                .get("tool_call_id")
                .and_then(|v| v.as_str())
                .map(String::from),
            tool_calls: msg.get("tool_calls").and_then(|v| {
                v.as_array().map(|arr| {
                    arr.iter()
                        .map(|tc| ToolCallData {
                            id: tc["id"].as_str().unwrap_or("").to_string(),
                            function_name: tc["function"]["name"]
                                .as_str()
                                .unwrap_or("")
                                .to_string(),
                            arguments: tc["function"]["arguments"]
                                .as_str()
                                .unwrap_or("")
                                .to_string(),
                        })
                        .collect()
                })
            }),
        }
    }

    /// Build an `InferenceRequest` from a generic JSON value (e.g. HTTP or WS body).
    /// Detects mode based on available fields.
    pub fn from_value(body: &Value) -> Self {
        let mode = if body.get("prefix").is_some() || body.get("suffix").is_some() {
            InferenceMode::Fim
        } else if body
            .get("messages")
            .and_then(|v| v.as_array())
            .map_or(false, |a| !a.is_empty())
        {
            InferenceMode::Chat
        } else {
            InferenceMode::Completion
        };

        let messages: Vec<InferenceMessage> = body
            .get("messages")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().map(Self::message_from_value).collect())
            .unwrap_or_default();

        let prompt = body
            .get("prompt")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let prefix = body
            .get("prefix")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let suffix = body
            .get("suffix")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let model_id = body
            .get("model_id")
            .or_else(|| body.get("model"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        Self {
            mode,
            model_id,
            messages,
            prompt,
            prefix,
            suffix,
            temperature: body
                .get("temperature")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.7) as f32,
            max_tokens: body
                .get("max_tokens")
                .and_then(|v| v.as_i64())
                .unwrap_or(512) as i32,
            top_p: body.get("top_p").and_then(|v| v.as_f64()).unwrap_or(0.9) as f32,
            top_k: body.get("top_k").and_then(|v| v.as_i64()).unwrap_or(40) as i32,
            min_p: body.get("min_p").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
            repeat_penalty: body
                .get("repeat_penalty")
                .and_then(|v| v.as_f64())
                .unwrap_or(1.1) as f32,
            frequency_penalty: body
                .get("frequency_penalty")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0) as f32,
            presence_penalty: body
                .get("presence_penalty")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0) as f32,
            session_id: body
                .get("session_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            priority: body.get("priority").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
            n_ctx: body.get("n_ctx").and_then(|v| v.as_u64()).unwrap_or(2048) as u32,
            n_batch: body.get("n_batch").and_then(|v| v.as_u64()).unwrap_or(512) as u32,
            n_threads: body.get("n_threads").and_then(|v| v.as_i64()).unwrap_or(-1) as i32,
            tools: Vec::new(),
            tool_definitions: body
                .get("tool_definitions")
                .or_else(|| body.get("tools"))
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|tc| {
                            // Accept both {"function": {...}} and direct tool schemas
                            let obj = tc.get("function").unwrap_or(tc);
                            if obj.get("name").and_then(|v| v.as_str()).is_some() {
                                Some(obj.clone())
                            } else {
                                None
                            }
                        })
                        .collect()
                })
                .unwrap_or_default(),
            architecture: body
                .get("architecture")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            metadata: json!({}),
            stream: body
                .get("stream")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        }
    }

    /// Build from a chat endpoint body that always contains `messages`.
    pub fn resolved_prompt(&self, template: Option<&str>) -> String {
        self.resolved_prompt_fim(template, None)
    }

    pub fn resolved_prompt_fim(&self, template: Option<&str>, arch: Option<&str>) -> String {
        match self.mode {
            InferenceMode::Fim => {
                if !self.prompt.is_empty() {
                    self.prompt.clone()
                } else {
                    let a = arch.unwrap_or(&self.architecture);
                    let fim_tmpl = if a.is_empty() {
                        Box::new(super::fim_template::CodestralFimTemplate)
                            as Box<dyn super::fim_template::FimTemplate>
                    } else {
                        super::fim_template::for_architecture(a)
                    };
                    fim_tmpl.build_prompt(&self.prefix, &self.suffix)
                }
            }
            InferenceMode::Completion => {
                if !self.prompt.is_empty() {
                    self.prompt.clone()
                } else {
                    self.messages
                        .iter()
                        .map(|m| format!("{}: {}", m.role, m.content))
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            }
            InferenceMode::Chat => {
                if !self.prompt.is_empty() {
                    return self.prompt.clone();
                }
                let mut messages_json: Vec<Value> = self
                    .messages
                    .iter()
                    .map(|m| {
                        let mut map = serde_json::Map::new();
                        map.insert("role".to_string(), Value::String(m.role.clone()));
                        map.insert("content".to_string(), Value::String(m.content.clone()));
                        if let Some(ref id) = m.tool_call_id {
                            map.insert("tool_call_id".to_string(), Value::String(id.clone()));
                        }
                        Value::Object(map)
                    })
                    .collect();

                // Inject tool definitions as a system message so the model
                // knows which tools are available. The message describes each
                // tool's JSON schema and instructs the model to use
                // <tool_call> XML syntax (matching ToolCallParser).
                if !self.tool_definitions.is_empty() {
                    let tool_list: Vec<Value> = self
                        .tool_definitions
                        .iter()
                        .map(|def| {
                            let name = def
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown");
                            let description = def
                                .get("description")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let parameters = def
                                .get("parameters")
                                .cloned()
                                .unwrap_or(serde_json::Value::Null);
                            json!({
                                "name": name,
                                "description": description,
                                "parameters": parameters
                            })
                        })
                        .collect();

                    let tools_json = serde_json::to_string(&tool_list).unwrap_or_default();
                    let tool_msg = format!(
                        r#"You have access to the following tools:

{}

To call a tool, respond with EXACTLY:
<tool_call>{{"name": "tool_name", "arguments": {{...}}}}</tool_call>

Do not add any other text before or after the tool call."#,
                        tools_json
                    );

                    let mut tool_system = serde_json::Map::new();
                    tool_system.insert("role".to_string(), Value::String("system".into()));
                    tool_system.insert("content".to_string(), Value::String(tool_msg));
                    messages_json.insert(0, Value::Object(tool_system));
                }

                super::chat_template::format_messages(&messages_json, template)
            }
        }
    }

    pub fn to_sampling_config(&self) -> super::sampler::SamplingConfig {
        super::sampler::SamplingConfig {
            temperature: self.temperature,
            top_p: self.top_p,
            top_k: self.top_k,
            min_p: self.min_p,
            repeat_penalty: self.repeat_penalty,
            frequency_penalty: self.frequency_penalty,
            presence_penalty: self.presence_penalty,
        }
    }

    pub fn to_context_config(&self) -> super::context_config::ContextConfig {
        super::context_config::ContextConfig {
            n_ctx: if self.n_ctx > 0 { self.n_ctx } else { 2048 },
            n_batch: if self.n_batch > 0 { self.n_batch } else { 512 },
            n_ubatch: 512,
            n_threads: if self.n_threads > 0 {
                self.n_threads
            } else {
                4
            },
            n_threads_batch: 4,
            flash_attn: false,
            offload_kqv: false,
            rope_scaling_type: 0,
            no_kv_offload: false,
            pooling_type: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_from_value_completion() {
        let body = json!({
            "prompt": "Hello world",
            "model_id": "test-model",
            "temperature": 0.5,
            "max_tokens": 100,
        });
        let req = InferenceRequest::from_value(&body);
        assert_eq!(req.model_id, "test-model");
        assert_eq!(req.prompt, "Hello world");
        assert!((req.temperature - 0.5).abs() < 0.001);
        assert_eq!(req.max_tokens, 100);
    }

    #[test]
    fn test_from_value_chat() {
        let body = json!({
            "messages": [
                {"role": "user", "content": "Hi"},
                {"role": "assistant", "content": "Hello"}
            ],
            "model_id": "chat-model",
        });
        let req = InferenceRequest::from_value(&body);
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[0].role, "user");
        assert_eq!(req.messages[0].content, "Hi");
        assert_eq!(req.messages[1].role, "assistant");
    }

    #[test]
    fn test_from_value_fim() {
        let body = json!({
            "prefix": "def foo():",
            "suffix": "    pass",
        });
        let req = InferenceRequest::from_value(&body);
        let prompt = req.resolved_prompt(None);
        assert!(prompt.contains("def foo():"));
        assert!(prompt.contains("<FIM>"));
        assert!(prompt.contains("    pass"));
    }

    #[test]
    fn test_from_value_with_messages_overrides_chat() {
        let body = json!({
            "messages": [{"role": "user", "content": "hello"}],
            "prompt": "should not be used",
        });
        let req = InferenceRequest::from_value(&body);
        assert_eq!(req.messages.len(), 1);
    }

    #[test]
    fn test_resolved_prompt_completion() {
        let body = json!({"prompt": "hello"});
        let req = InferenceRequest::from_value(&body);
        assert_eq!(req.resolved_prompt(None), "hello");
    }

    #[test]
    fn test_message_from_value_with_tool_calls() {
        let body = json!({
            "messages": [{
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": "call_1",
                    "function": {"name": "get_weather", "arguments": "{\"city\":\"NYC\"}"}
                }]
            }]
        });
        let req = InferenceRequest::from_value(&body);
        assert_eq!(req.messages.len(), 1);
        let msg = &req.messages[0];
        assert_eq!(msg.role, "assistant");
        assert!(msg.tool_calls.is_some());
        let calls = msg.tool_calls.as_ref().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].function_name, "get_weather");
    }

    #[test]
    fn test_to_sampling_config() {
        let body = json!({
            "temperature": 0.8,
            "top_p": 0.95,
            "top_k": 50,
            "repeat_penalty": 1.2,
        });
        let req = InferenceRequest::from_value(&body);
        let sc = req.to_sampling_config();
        assert!((sc.temperature - 0.8).abs() < 0.001);
        assert!((sc.top_p - 0.95).abs() < 0.001);
        assert_eq!(sc.top_k, 50);
        assert!((sc.repeat_penalty - 1.2).abs() < 0.001);
    }

    #[test]
    fn test_to_context_config() {
        let body = json!({"n_ctx": 4096, "n_batch": 256});
        let req = InferenceRequest::from_value(&body);
        let cc = req.to_context_config();
        assert_eq!(cc.n_ctx, 4096);
        assert_eq!(cc.n_batch, 256);
    }

}
