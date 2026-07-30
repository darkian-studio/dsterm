use crate::ai::inference_request::InferenceMessage;
use serde_json::Value;

/// A chat template implementation for a specific model architecture.
pub trait ChatTemplate: Send + Sync {
    fn build(&self, messages: &[InferenceMessage]) -> String;
}

/// Selects the appropriate ChatTemplate implementation based on the model architecture string
/// from GGUF metadata. Falls back to the GGUF's tokenizer.ggml.chat_template if available,
/// then to the architecture-specific template, and finally to the fallback format.
pub fn for_architecture(arch: &str) -> Box<dyn ChatTemplate> {
    let lower = arch.to_lowercase();
    if lower.contains("llama") {
        Box::new(Llama3Template)
    } else if lower.contains("qwen") {
        Box::new(QwenTemplate)
    } else if lower.contains("deepseek") {
        Box::new(DeepSeekTemplate)
    } else if lower.contains("codestral") || lower.contains("mistral") {
        Box::new(CodestralTemplate)
    } else if lower.contains("chatglm") || lower.contains("glm") {
        Box::new(ChatMLTemplate)
    } else if lower.contains("starcoder") || lower.contains("instruct") {
        Box::new(GenericInstructTemplate)
    } else {
        Box::new(FallbackTemplate)
    }
}

/// Generic Instruct-style: `[INST] {messages} [/INST]`
pub struct GenericInstructTemplate;

impl ChatTemplate for GenericInstructTemplate {
    fn build(&self, messages: &[InferenceMessage]) -> String {
        let mut buf = String::new();
        for msg in messages {
            match msg.role.as_str() {
                "system" => buf.push_str(&format!("<<SYS>>\n{}\n<</SYS>>\n\n", msg.content)),
                "user" => buf.push_str(&format!("[INST] {} [/INST]", msg.content)),
                "assistant" => buf.push_str(&format!("{} ", msg.content)),
                _ => buf.push_str(&format!("{}: {}\n", msg.role, msg.content)),
            }
        }
        buf
    }
}

/// ChatML: `<|im_start|>role\ncontent<|im_end|>\n`
pub struct ChatMLTemplate;

impl ChatTemplate for ChatMLTemplate {
    fn build(&self, messages: &[InferenceMessage]) -> String {
        let mut buf = String::new();
        for msg in messages {
            buf.push_str(&format!(
                "<|im_start|>{}\n{}<|im_end|>\n",
                msg.role, msg.content
            ));
        }
        buf.push_str("<|im_start|>assistant\n");
        buf
    }
}

/// Llama 3: uses special tokens `<|start_header_id|>role<|end_header_id|>\n\ncontent<|eot_id|>`
pub struct Llama3Template;

impl ChatTemplate for Llama3Template {
    fn build(&self, messages: &[InferenceMessage]) -> String {
        let mut buf = String::new();
        buf.push_str("<|begin_of_text|>");
        for msg in messages {
            buf.push_str(&format!(
                "<|start_header_id|>{}<|end_header_id|>\n\n{}<|eot_id|>",
                msg.role, msg.content
            ));
        }
        buf.push_str("<|start_header_id|>assistant<|end_header_id|>\n\n");
        buf
    }
}

/// Qwen: `<|im_start|>role\ncontent<|im_end|>\n` (same as ChatML but with Qwen-specific tokens)
pub struct QwenTemplate;

impl ChatTemplate for QwenTemplate {
    fn build(&self, messages: &[InferenceMessage]) -> String {
        let mut buf = String::new();
        buf.push_str("<|im_start|>system\nYou are a helpful assistant.<|im_end|>\n");
        for msg in messages {
            if msg.role == "system" {
                continue;
            }
            buf.push_str(&format!(
                "<|im_start|>{}\n{}<|im_end|>\n",
                msg.role, msg.content
            ));
        }
        buf.push_str("<|im_start|>assistant\n");
        buf
    }
}

/// DeepSeek: similar to ChatML but with DeepSeek-specific formatting
pub struct DeepSeekTemplate;

impl ChatTemplate for DeepSeekTemplate {
    fn build(&self, messages: &[InferenceMessage]) -> String {
        let mut buf = String::new();
        for msg in messages {
            match msg.role.as_str() {
                "system" => buf.push_str(&format!("<｜begin▁of▁sentence｜>{}\n", msg.content)),
                "user" => buf.push_str(&format!("User: {}\n\n", msg.content)),
                "assistant" => buf.push_str(&format!("Assistant: {}\n", msg.content)),
                _ => buf.push_str(&format!("{}: {}\n", msg.role, msg.content)),
            }
        }
        buf.push_str("Assistant: ");
        buf
    }
}

/// Codestral / Mistral: uses `[INST]` / `[/INST]` tokens
pub struct CodestralTemplate;

impl ChatTemplate for CodestralTemplate {
    fn build(&self, messages: &[InferenceMessage]) -> String {
        let mut buf = String::new();
        buf.push_str("<s>");
        for msg in messages {
            match msg.role.as_str() {
                "system" => buf.push_str(&format!("[INST] {} [/INST]", msg.content)),
                "user" => buf.push_str(&format!("[INST] {} [/INST]", msg.content)),
                "assistant" => buf.push_str(&format!("{} ", msg.content)),
                _ => buf.push_str(&format!("{}: {}\n", msg.role, msg.content)),
            }
        }
        buf
    }
}

/// Fallback: simple `role: content` format with `Assistant:` trailing prompt
pub struct FallbackTemplate;

impl ChatTemplate for FallbackTemplate {
    fn build(&self, messages: &[InferenceMessage]) -> String {
        let mut buf = String::new();
        for msg in messages {
            match msg.role.as_str() {
                "system" => {
                    buf.push_str("System: ");
                    buf.push_str(&msg.content);
                    buf.push_str("\n\n");
                }
                "user" => {
                    buf.push_str("User: ");
                    buf.push_str(&msg.content);
                    buf.push_str("\n\n");
                }
                "assistant" => {
                    if !msg.content.is_empty() {
                        buf.push_str("Assistant: ");
                        buf.push_str(&msg.content);
                    } else {
                        buf.push_str("Assistant:");
                    }
                    buf.push_str("\n\n");
                }
                "tool" => {
                    let name = msg
                        .tool_call_id
                        .as_deref()
                        .unwrap_or("tool");
                    buf.push_str(&format!("Tool ({name}): {}\n\n", msg.content));
                }
                _ => {
                    buf.push_str(&format!("{}: {}\n\n", msg.role, msg.content));
                }
            }
        }
        buf.push_str("Assistant:");
        buf
    }
}

/// Formats messages using either a GGUF chat_template string or an architecture-specific template.
/// This is the main entry point for chat formatting.
pub fn format_messages(messages: &[Value], chat_template: Option<&str>) -> String {
    if let Some(template) = chat_template {
        if !template.is_empty() {
            if let Ok(rendered) = render_template(template, messages) {
                return rendered;
            }
        }
    }
    fallback_format(messages)
}

fn render_template(template: &str, messages: &[Value]) -> Result<String, String> {
    let cleaned = template
        .replace("{{ bos_token }}", "")
        .replace("{{ eos_token }}", "");

    if cleaned.contains("{% for message in messages %}")
        || cleaned.contains("{% for msg in messages %}")
    {
        return render_for_loop(&cleaned, messages);
    }

    Err("unrecognized template".into())
}

fn render_for_loop(template: &str, messages: &[Value]) -> Result<String, String> {
    let parts: Vec<&str> = template.split("{% endfor %}").collect();
    if parts.len() < 2 {
        return Err("no endfor".into());
    }

    let prefix = parts[0].split("{% for").next().unwrap_or("");
    let body_template = parts[0]
        .split("%}")
        .skip(1)
        .collect::<Vec<&str>>()
        .join("%}");
    let suffix = parts[1..].join("{% endfor %}");

    let add_gen = suffix.contains("add_generation_prompt");

    let mut result = String::new();
    result.push_str(prefix);

    for msg in messages {
        let role = msg["role"].as_str().unwrap_or("user");
        let content = msg["content"].as_str().unwrap_or("");
        let line = body_template
            .replace("{{ message.role }}", role)
            .replace("{{ message.content }}", content)
            .replace("{{ msg.role }}", role)
            .replace("{{ msg.content }}", content)
            .replace("{{ message['role'] }}", role)
            .replace("{{ message['content'] }}", content);
        result.push_str(&line);
    }

    let mut suffix = suffix.to_string();
    if add_gen {
        suffix = suffix
            .replace("{% if add_generation_prompt %}", "")
            .replace("{% endif %}", "")
            .replace("{{ '<|im_start|>assistant\n' }}", "assistant:\n")
            .replace("{{ '<|im_start|>assistant\\n' }}", "assistant:\n")
            .replace("{{ '<|im_start|>assistant' }}", "assistant:");
        if !suffix.contains("assistant") {
            suffix.push_str("assistant:");
        }
    }

    result.push_str(&suffix);
    Ok(result)
}

fn fallback_format(messages: &[Value]) -> String {
    let mut buf = String::new();
    for msg in messages {
        let role = msg["role"].as_str().unwrap_or("user");
        let content = msg["content"].as_str().unwrap_or("");
        match role {
            "system" => {
                buf.push_str("System: ");
                buf.push_str(content);
                buf.push_str("\n\n");
            }
            "user" => {
                buf.push_str("User: ");
                buf.push_str(content);
                buf.push_str("\n\n");
            }
            "assistant" => {
                if !content.is_empty() {
                    buf.push_str("Assistant: ");
                    buf.push_str(content);
                } else {
                    buf.push_str("Assistant:");
                }
                buf.push_str("\n\n");
            }
            "tool" => {
                let name = msg["tool_call_id"].as_str().unwrap_or("tool");
                buf.push_str(&format!("Tool ({name}): {content}\n\n"));
            }
            _ => {
                buf.push_str(&format!("{role}: {content}\n\n"));
            }
        }
    }
    buf.push_str("Assistant:");
    buf
}

pub fn validate_context(messages: &[Value], max_context: u32) -> Result<(), String> {
    let mut total_chars: usize = 0;
    for msg in messages {
        if let Some(content) = msg["content"].as_str() {
            total_chars += content.len();
        }
    }
    let estimated_tokens = (total_chars / 4) as u32;
    if estimated_tokens > max_context {
        return Err(format!(
            "Estimated {} tokens exceeds max context {}",
            estimated_tokens, max_context
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fallback() {
        let msgs = serde_json::from_str::<Vec<Value>>(
            r#"[
                {"role":"system","content":"You are helpful."},
                {"role":"user","content":"Hi"}
            ]"#,
        )
        .unwrap();
        let result = fallback_format(&msgs);
        assert!(result.contains("System: You are helpful."));
        assert!(result.contains("User: Hi"));
    }

    #[test]
    fn test_chatml_template() {
        let msgs = serde_json::from_str::<Vec<Value>>(
            r#"[
                {"role":"user","content":"Hello"},
                {"role":"assistant","content":"Hi"}
            ]"#,
        )
        .unwrap();
        let template =
            "{% for message in messages %}{{ message.role }}\n{{ message.content }}\n{% endfor %}";
        let result = render_template(template, &msgs).unwrap();
        assert!(result.contains("user\nHello\n"));
        assert!(result.contains("assistant\nHi\n"));
    }

    #[test]
    fn test_validate_context_ok() {
        let msgs =
            serde_json::from_str::<Vec<Value>>(r#"[{"role":"user","content":"short"}]"#).unwrap();
        assert!(validate_context(&msgs, 2048).is_ok());
    }

    #[test]
    fn test_validate_context_overflow() {
        let long = "a".repeat(10000);
        let msgs = serde_json::from_str::<Vec<Value>>(&format!(
            r#"[{{"role":"user","content":"{}"}}]"#,
            long
        ))
        .unwrap();
        assert!(validate_context(&msgs, 100).is_err());
    }

    #[test]
    fn test_inference_message_formatting() {
        let msgs = vec![
            InferenceMessage {
                role: "system".into(),
                content: "Be helpful.".into(),
                tool_call_id: None,
                tool_calls: None,
            },
            InferenceMessage {
                role: "user".into(),
                content: "Hello".into(),
                tool_call_id: None,
                tool_calls: None,
            },
        ];

        let template = for_architecture("llama");
        let result = template.build(&msgs);
        assert!(result.contains("system") || result.contains("header_id"));
    }

    #[test]
    fn test_architecture_selection() {
        assert!(for_architecture("llama-3-8b").as_ref() as *const _ !=
                for_architecture("qwen2-7b").as_ref() as *const _);
    }
}
