use serde_json::Value;

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
    // Rough estimate: 1 token ≈ 4 chars
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
}
