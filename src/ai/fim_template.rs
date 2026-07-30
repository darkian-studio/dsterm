pub trait FimTemplate: Send + Sync {
    fn build_prompt(&self, prefix: &str, suffix: &str) -> String;
    fn stop_sequences(&self) -> Vec<String>;
}

pub fn for_architecture(arch: &str) -> Box<dyn FimTemplate> {
    let lower = arch.to_lowercase();
    if lower.contains("codestral") || lower.contains("mistral") {
        Box::new(CodestralFimTemplate)
    } else if lower.contains("deepseek") {
        Box::new(DeepSeekFimTemplate)
    } else if lower.contains("qwen") || lower.contains("codeqwen") {
        Box::new(QwenFimTemplate)
    } else if lower.contains("starcoder") || lower.contains("llama") {
        Box::new(CodestralFimTemplate)
    } else {
        Box::new(ChatFallbackFimTemplate)
    }
}

/// Codestral / Mistral: [PREFIX]prefix[SUFFIX]suffix[MIDDLE]
pub struct CodestralFimTemplate;

impl FimTemplate for CodestralFimTemplate {
    fn build_prompt(&self, prefix: &str, suffix: &str) -> String {
        format!("[PREFIX]{prefix}[SUFFIX]{suffix}[MIDDLE]")
    }

    fn stop_sequences(&self) -> Vec<String> {
        vec!["[PREFIX]".into(), "[SUFFIX]".into(), "[MIDDLE]".into()]
    }
}

/// DeepSeek Coder: [DS-BEGIN]prefix[DS-END]suffix[DS-MIDDLE]
pub struct DeepSeekFimTemplate;

impl FimTemplate for DeepSeekFimTemplate {
    fn build_prompt(&self, prefix: &str, suffix: &str) -> String {
        format!("[DS-BEGIN]{prefix}[DS-END]{suffix}[DS-MIDDLE]")
    }

    fn stop_sequences(&self) -> Vec<String> {
        vec!["[DS-BEGIN]".into(), "[DS-END]".into(), "[DS-MIDDLE]".into()]
    }
}

/// Qwen Coder: <|fim_prefix|>prefix<|fim_suffix|>suffix<|fim_middle|>
pub struct QwenFimTemplate;

impl FimTemplate for QwenFimTemplate {
    fn build_prompt(&self, prefix: &str, suffix: &str) -> String {
        format!("<|fim_prefix|>{prefix}<|fim_suffix|>{suffix}<|fim_middle|>")
    }

    fn stop_sequences(&self) -> Vec<String> {
        vec![
            "<|fim_prefix|>".into(),
            "<|fim_suffix|>".into(),
            "<|fim_middle|>".into(),
            "<|fim_pad|>".into(),
        ]
    }
}

/// Fallback for models without native FIM support.
pub struct ChatFallbackFimTemplate;

impl FimTemplate for ChatFallbackFimTemplate {
    fn build_prompt(&self, prefix: &str, suffix: &str) -> String {
        format!(
            "You are a code completion engine. Complete the code at the \
             <CURSOR> marker. Output only the replacement code with no \
             explanation and no markdown code fences.\n\n\
             {prefix}<CURSOR>{suffix}"
        )
    }

    fn stop_sequences(&self) -> Vec<String> {
        vec!["<CURSOR>".into()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_codestral() {
        let t = CodestralFimTemplate;
        assert_eq!(t.build_prompt("a", "b"), "[PREFIX]a[SUFFIX]b[MIDDLE]");
    }

    #[test]
    fn test_deepseek() {
        let t = DeepSeekFimTemplate;
        let r = t.build_prompt("a", "b");
        assert!(r.contains("[DS-BEGIN]"));
        assert!(r.contains("[DS-MIDDLE]"));
    }

    #[test]
    fn test_qwen() {
        let t = QwenFimTemplate;
        assert_eq!(
            t.build_prompt("a", "b"),
            "<|fim_prefix|>a<|fim_suffix|>b<|fim_middle|>"
        );
    }

    #[test]
    fn test_fallback() {
        let t = ChatFallbackFimTemplate;
        let r = t.build_prompt("a", "b");
        assert!(r.contains("code completion engine"));
    }

    #[test]
    fn test_architecture_selection() {
        assert_ne!(
            for_architecture("codestral").build_prompt("a", "b"),
            for_architecture("qwen").build_prompt("a", "b"),
        );
    }
}
