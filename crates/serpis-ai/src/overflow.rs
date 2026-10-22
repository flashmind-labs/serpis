//! Recognizing a context-overflow error (pi-ai `utils/overflow.ts`): the prompt no longer fits
//! the model's window, so the chat should compact and retry.

use std::sync::OnceLock;

use regex::RegexSet;

use crate::types::{AssistantMessage, StopReason};

const OVERFLOW: &[&str] = &[
    r"(?i)prompt (?:is )?too long",                                         // Anthropic, z.ai
    r"(?i)prompt exceeds max length",                                       // z.ai CN
    r"(?i)request_too_large",                                               // Anthropic HTTP 413
    r"(?i)input is too long for requested model",                           // Amazon Bedrock
    r"(?i)exceeds the context window",                                      // OpenAI
    r"(?i)exceeds (?:the )?(?:model'?s )?maximum context length(?: of [\d,]+ tokens?|\s*\([\d,]+\))", // LiteLLM
    r"(?i)input token count.*exceeds the maximum",                          // Gemini
    r"(?i)maximum prompt length is \d+",                                    // xAI
    r"(?i)reduce the length of the messages",                               // Groq
    r"(?i)maximum context length is \d+ tokens",                            // OpenRouter
    r"(?i)exceeds (?:the )?maximum allowed input length of [\d,]+ tokens?", // OpenRouter/Poolside
    r"(?i)input \(\d+ tokens\) is longer than the model'?s context length \(\d+ tokens\)", // Together
    r"(?i)exceeds the limit of \d+",                                        // GitHub Copilot
    r"(?i)exceeds the available context size",                              // llama.cpp
    r"(?i)greater than the context length",                                 // LM Studio
    r"(?i)context window exceeds limit",                                    // MiniMax
    r"(?i)exceeded model token limit",                                      // Kimi
    r"(?i)too large for model with \d+ maximum context length",            // Mistral
    r"(?i)prompt has [\d,]+ tokens?, but the configured context size is [\d,]+ tokens?",
    r"(?i)model_context_window_exceeded",                                   // z.ai finish reason
    r"(?i)prompt too long; exceeded (?:max )?context length",               // Ollama
    r"(?i)range of input length should be",                                 // DashScope
    r"(?i)context[_ ]length[_ ]exceeded",
    r"(?i)too many tokens",
    r"(?i)token limit exceeded",
];
const NOT_OVERFLOW: &[&str] = &[r"(?i)^(Throttling error|Service unavailable):", r"(?i)rate limit", r"(?i)too many requests"];

/// Whether `m` failed (or came back) because its prompt doesn't fit a `context_window`-token window.
pub fn is_context_overflow(m: &AssistantMessage, context_window: Option<u64>) -> bool {
    static SETS: OnceLock<(RegexSet, RegexSet)> = OnceLock::new();
    let (overflow, not) = SETS.get_or_init(|| (RegexSet::new(OVERFLOW).unwrap(), RegexSet::new(NOT_OVERFLOW).unwrap()));
    if m.stop_reason == StopReason::Error {
        if let Some(e) = &m.error_message {
            if !not.is_match(e) && (overflow.is_match(e) || (m.provider == "cerebras" && e.trim_start().starts_with("40") && e.contains("(no body)"))) {
                return true;
            }
        }
    }
    let input = m.usage.input + m.usage.cache_read;
    match (context_window, m.stop_reason) {
        (Some(w), StopReason::Stop) => input > w,
        (Some(w), StopReason::Length) => m.usage.output == 0 && input as f64 >= w as f64 * 0.99,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed(provider: &str, error: &str) -> AssistantMessage {
        let model: crate::Model = serde_json::from_value(serde_json::json!({"id": "m", "api": "x", "provider": provider})).unwrap();
        let mut a = AssistantMessage::empty(&model);
        a.stop_reason = StopReason::Error;
        a.error_message = Some(error.into());
        a
    }

    #[test]
    fn provider_overflow_errors_are_recognized_and_rate_limits_are_not() {
        for e in [
            "400 prompt is too long: 213456 tokens > 200000 maximum",
            "This model's maximum context length is 128000 tokens. However, you requested 140000 tokens",
            "Your input exceeds the context window of this model.",
            "input token count (1100000) exceeds the maximum number of tokens allowed (1048576)",
            "the request exceeds the available context size, try increasing it",
        ] {
            assert!(is_context_overflow(&failed("p", e), None), "{e}");
        }
        assert!(!is_context_overflow(&failed("p", "429 rate limit: too many tokens per minute"), None));
        assert!(!is_context_overflow(&failed("p", "500 internal error"), None));
        assert!(is_context_overflow(&failed("cerebras", "400 status code (no body)"), None));
    }
}
