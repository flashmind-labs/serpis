//! Cross-provider transcript normalization (pi-ai `transform-messages.ts`).

use std::collections::{HashMap, HashSet};

use crate::types::*;

const NON_VISION_USER_IMAGE_PLACEHOLDER: &str = "(image omitted: model does not support images)";
const NON_VISION_TOOL_IMAGE_PLACEHOLDER: &str = "(tool image omitted: model does not support images)";

fn replace_images(content: &[UserContent], placeholder: &str) -> Vec<UserContent> {
    let mut out = Vec::new();
    let mut previous_was_placeholder = false;
    for block in content {
        match block {
            UserContent::Image { .. } => {
                if !previous_was_placeholder {
                    out.push(UserContent::Text {
                        text: placeholder.to_string(),
                    });
                }
                previous_was_placeholder = true;
            }
            UserContent::Text { text } => {
                previous_was_placeholder = text == placeholder;
                out.push(block.clone());
            }
        }
    }
    out
}

/// Prepare a transcript for `model`:
/// - drop images the model cannot see,
/// - drop thinking from other models (keep signed/redacted thinking of the same model),
/// - normalize tool call ids for the target API,
/// - drop errored/aborted assistant turns,
/// - synthesize "No result provided" for tool calls left without results.
pub fn transform_messages(
    messages: &[Message],
    model: &Model,
    normalize_tool_call_id: Option<&dyn Fn(&str) -> String>,
) -> Vec<Message> {
    let mut id_map: HashMap<String, String> = HashMap::new();
    let vision = model.supports_images();

    let transformed: Vec<Message> = messages
        .iter()
        .map(|msg| match msg {
            Message::User { content, timestamp } if !vision => Message::User {
                content: replace_images(content, NON_VISION_USER_IMAGE_PLACEHOLDER),
                timestamp: *timestamp,
            },
            Message::User { .. } => msg.clone(),
            Message::ToolResult(tr) => {
                let mut tr = tr.clone();
                if !vision {
                    tr.content = replace_images(&tr.content, NON_VISION_TOOL_IMAGE_PLACEHOLDER);
                }
                if let Some(id) = id_map.get(&tr.tool_call_id) {
                    tr.tool_call_id = id.clone();
                }
                Message::ToolResult(tr)
            }
            Message::Assistant(a) => {
                let same_model = a.provider == model.provider && a.api == model.api && a.model == model.id;
                let mut a = a.clone();
                a.content = a
                    .content
                    .into_iter()
                    .filter_map(|block| match block {
                        AssistantContent::Thinking {
                            ref thinking,
                            ref thinking_signature,
                            redacted,
                        } => {
                            if !same_model {
                                return None;
                            }
                            if redacted || thinking_signature.as_deref().is_some_and(|s| !s.is_empty()) {
                                return Some(block);
                            }
                            if thinking.trim().is_empty() {
                                return None;
                            }
                            Some(block)
                        }
                        AssistantContent::Text { text, text_signature } => Some(AssistantContent::Text {
                            text,
                            text_signature: if same_model { text_signature } else { None },
                        }),
                        AssistantContent::ToolCall(mut tc) => {
                            if !same_model {
                                tc.thought_signature = None;
                                if let Some(norm) = normalize_tool_call_id {
                                    let new_id = norm(&tc.id);
                                    if new_id != tc.id {
                                        id_map.insert(tc.id.clone(), new_id.clone());
                                        tc.id = new_id;
                                    }
                                }
                            }
                            Some(AssistantContent::ToolCall(tc))
                        }
                    })
                    .collect();
                Message::Assistant(a)
            }
        })
        .collect();

    let mut result = Vec::with_capacity(transformed.len());
    let mut pending: Vec<ToolCall> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let close = |result: &mut Vec<Message>, pending: &mut Vec<ToolCall>, seen: &mut HashSet<String>| {
        for tc in pending.drain(..) {
            if !seen.contains(&tc.id) {
                result.push(Message::ToolResult(ToolResultMessage {
                    tool_call_id: tc.id.clone(),
                    tool_name: tc.name.clone(),
                    content: vec![UserContent::Text {
                        text: "No result provided".into(),
                    }],
                    is_error: true,
                    details: serde_json::Value::Null,
                    timestamp: now_ms(),
                }));
            }
        }
        seen.clear();
    };

    for msg in transformed {
        match &msg {
            Message::Assistant(a) => {
                close(&mut result, &mut pending, &mut seen);
                if matches!(a.stop_reason, StopReason::Error | StopReason::Aborted) {
                    continue;
                }
                pending = a.tool_calls().cloned().collect();
                result.push(msg);
            }
            Message::ToolResult(tr) => {
                seen.insert(tr.tool_call_id.clone());
                result.push(msg);
            }
            Message::User { .. } => {
                close(&mut result, &mut pending, &mut seen);
                result.push(msg);
            }
        }
    }
    close(&mut result, &mut pending, &mut seen);
    result
}

/// Short stable hash for id shortening.
pub fn short_hash(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(s.as_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}
