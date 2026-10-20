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

/// The template an `InferenceRequest` prompts through: mirror of the
/// selection inside `resolved_prompt_fim` so prompt building and stop
/// extraction can never disagree. An empty architecture defaults to the
/// Codestral shape rather than falling through to the chat fallback,
/// because a caller with no architecture still wants plain FIM.
pub fn fim_template_for_prompt(arch: &str) -> Box<dyn FimTemplate> {
    if arch.is_empty() {
        Box::new(CodestralFimTemplate)
    } else {
        for_architecture(arch)
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

    /// The prompt-path selection treats an empty architecture as plain
    /// Codestral FIM, not the chat fallback; these two selections take
    /// different shapes and resolved_prompt_fim / stop extraction must
    /// agree on this.
    #[test]
    fn empty_architecture_defaults_to_codestral() {
        assert_eq!(
            fim_template_for_prompt("").build_prompt("a", "b"),
            "[PREFIX]a[SUFFIX]b[MIDDLE]"
        );
    }

    /// Stop extraction must cover the markers its prompt inserts, or the
    /// model echoes them back unchecked.
    #[test]
    fn stops_cover_prompt_markers() {
        let cases = [
            "[PREFIX]a[SUFFIX]b[MIDDLE]",
            "[DS-BEGIN]a[DS-END]b[DS-MIDDLE]",
            "<|fim_prefix|>a<|fim_suffix|>b<|fim_middle|>",
        ];
        for (arch, prompt) in [
            ("mistral-7b-fim", cases[0]),
            ("deepseek-coder-v2", cases[1]),
            ("qwen3-coder-30b", cases[2]),
        ] {
            let t = fim_template_for_prompt(arch);
            assert_eq!(t.build_prompt("a", "b"), prompt, "{arch}");
            let stops = t.stop_sequences();
            assert!(!stops.is_empty(), "{arch}: no stop sequences");
            // Every shape here inserts at least its first two markers into
            // the prompt, so emitting any of them back from the model must
            // be caught.
            for stop in &stops[0..2] {
                assert!(
                    prompt.contains(stop.as_str()),
                    "{arch}: {stop} not in prompt"
                );
            }
        }
    }
}
