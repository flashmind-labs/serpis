//! Compaction (pi's `core/compaction`): when a conversation nears its model's context window, the
//! messages before a cut point are replaced by a summary and the recent ones are kept as they are.
//!
//! Unlike pi, the summary is asked for in the conversation itself: the messages before the cut
//! plus one instruction, sent as the agent sends everything (`crate::complete`). That request is a
//! prefix of what the chat already sent, so the provider reads it from the prompt cache instead of
//! billing the whole history again as new input; and it never includes the kept messages, so it
//! fits even when the conversation no longer does.

use serpis_ai::{Message, UserContent};

/// Opens the message that replaces the compacted history (pi's wording, so pi and serpis resume
/// each other's sessions with the same bytes).
pub const SUMMARY_PREFIX: &str = "The conversation history before this point was compacted into the following summary:";

/// The message a summary becomes at the start of the compacted conversation.
pub fn summary_message(summary: &str) -> Message {
    Message::User {
        content: vec![UserContent::Text { text: format!("{SUMMARY_PREFIX}\n\n<summary>\n{summary}\n</summary>") }],
        timestamp: 0,
    }
}

/// Whether a conversation of `tokens` should be compacted to keep `reserve` tokens of a
/// `window`-token context free.
pub fn should_compact(tokens: u64, window: u64, reserve: u64) -> bool {
    window > 0 && tokens > window.saturating_sub(reserve)
}

/// Tokens the conversation filled at its last response (prompt plus output).
pub fn context_tokens(history: &[Message]) -> Option<u64> {
    history.iter().rev().find_map(|m| match m {
        Message::Assistant(a) if a.usage.input + a.usage.cache_read + a.usage.cache_write > 0 => {
            Some(a.usage.input + a.usage.cache_read + a.usage.cache_write + a.usage.output)
        }
        _ => None,
    })
}

/// A rough token count for a message (pi's estimate: 4 characters a token, images 1,200).
pub fn estimate_tokens(m: &Message) -> u64 {
    let images = match m {
        Message::User { content, .. } => content.iter().filter(|c| matches!(c, UserContent::Image { .. })).count(),
        Message::ToolResult(r) => r.content.iter().filter(|c| matches!(c, UserContent::Image { .. })).count(),
        Message::Assistant(_) => 0,
    } as u64;
    let text = serde_json::to_string(m).map(|s| s.len()).unwrap_or(0) as u64;
    text.div_ceil(4) + images * 1200
}

/// Where to cut `history` so at least `keep` tokens of recent messages stay: an index whose
/// message is a user or assistant message (a tool result never loses its call), not the first.
/// None when there is nothing before such a point to compact.
pub fn cut_point(history: &[Message], keep: u64) -> Option<usize> {
    let candidates: Vec<usize> = (1..history.len()).filter(|&i| !matches!(history[i], Message::ToolResult(_))).collect();
    let last = *candidates.last()?;
    let mut kept = 0;
    for i in (0..history.len()).rev() {
        kept += estimate_tokens(&history[i]);
        if kept >= keep {
            return Some(candidates.iter().copied().find(|&c| c >= i).unwrap_or(last));
        }
    }
    // The whole conversation is within `keep`: there is nothing to gain.
    None
}

/// The request for a summary, sent after the messages it summarizes.
pub fn instruction(custom: Option<&str>) -> Message {
    let mut text = String::from(
        "Stop here: do not continue the conversation, answer anything in it, or call tools. This message only asks for a summary.\n\n\
         The conversation above is about to be compacted: it will be replaced by your summary, and the conversation continues after it. \
         Create a structured context checkpoint summary that another LLM will use to continue the work. \
         If the conversation starts with an earlier summary, carry its information over.\n\n",
    );
    text.push_str(SUMMARY_FORMAT);
    if let Some(c) = custom.filter(|c| !c.trim().is_empty()) {
        text.push_str("\n\nAdditional focus: ");
        text.push_str(c);
    }
    Message::User { content: vec![UserContent::Text { text }], timestamp: 0 }
}

/// pi's checkpoint format.
const SUMMARY_FORMAT: &str = "Use this EXACT format:

## Goal
[What is the user trying to accomplish? Can be multiple items if the session covers different tasks.]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned by user]
- [Or \"(none)\" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Current work]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [Ordered list of what should happen next]

## Critical Context
- [Any data, examples, or references needed to continue]
- [Or \"(none)\" if not applicable]

Keep each section concise. Preserve exact file paths, function names, and error messages.";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use serpis_ai::*;

    fn model() -> Model {
        serde_json::from_value(json!({"id": "m", "api": "anthropic-messages", "provider": "p"})).unwrap()
    }

    fn user(t: &str) -> Message {
        Message::User { content: vec![UserContent::Text { text: t.into() }], timestamp: 1 }
    }

    fn step(n: usize) -> Vec<Message> {
        let mut a = AssistantMessage::empty(&model());
        a.content = (0..n)
            .map(|i| AssistantContent::ToolCall(ToolCall { id: format!("c{i}"), name: "read".into(), arguments: json!({}), thought_signature: None, arguments_error: None }))
            .collect();
        let mut out = vec![Message::Assistant(a)];
        out.extend((0..n).map(|i| {
            Message::ToolResult(ToolResultMessage { tool_call_id: format!("c{i}"), tool_name: "read".into(), content: vec![UserContent::Text { text: "x".repeat(400) }], is_error: false, details: Value::Null, timestamp: 1 })
        }));
        out
    }

    #[test]
    fn compaction_starts_when_the_reserve_is_reached() {
        assert!(!should_compact(150_000, 200_000, 16_384));
        assert!(should_compact(190_000, 200_000, 16_384));
        assert!(!should_compact(190_000, 0, 16_384), "unknown window: never");
    }

    #[test]
    fn the_cut_keeps_recent_messages_and_never_separates_a_result_from_its_call() {
        let mut h = vec![user("first")];
        for i in 0..6 {
            h.push(user(&format!("turn {i}")));
            h.extend(step(3));
        }
        let cut = cut_point(&h, 500).unwrap();
        assert!(!matches!(h[cut], Message::ToolResult(_)));
        let kept: u64 = h[cut..].iter().map(estimate_tokens).sum();
        assert!(kept >= 500, "{kept}");
        assert!(cut > 1, "older turns go");
        // Everything fits in `keep`: nothing to compact.
        assert_eq!(cut_point(&h, 1_000_000), None);
        // One huge turn: the cut falls inside it, before an assistant step.
        let mut h = vec![user("do it")];
        for _ in 0..10 {
            h.extend(step(2));
        }
        let cut = cut_point(&h, 300).unwrap();
        assert!(matches!(h[cut], Message::Assistant(_)), "{cut}");
    }

    #[test]
    fn the_summary_message_is_what_a_resumed_session_loads() {
        let Message::User { content, .. } = summary_message("S") else { panic!() };
        assert_eq!(content, vec![UserContent::Text { text: format!("{SUMMARY_PREFIX}\n\n<summary>\nS\n</summary>") }]);
    }

    #[test]
    fn the_context_is_the_last_response_s_prompt_and_output() {
        let mut a = AssistantMessage::empty(&model());
        a.usage = Usage { input: 10, cache_read: 1000, cache_write: 200, output: 50, ..Default::default() };
        assert_eq!(context_tokens(&[user("x"), Message::Assistant(a)]), Some(1260));
        assert_eq!(context_tokens(&[user("x")]), None);
    }
}
