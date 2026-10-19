//! API adapters, one per wire protocol, dispatched by `Model::api`.

pub mod anthropic_messages;
pub mod google;
pub mod openai_completions;
pub mod openai_responses;

use crate::stream::{Emitter, EventStream, StreamOptions};
use crate::types::{Context, Model, ThinkingLevel};

pub const SUPPORTED_APIS: &[&str] = &[
    "openai-completions",
    "anthropic-messages",
    "openai-responses",
    "google-generative-ai",
];

/// Default thinking budgets for token-budget providers (pi-ai `DEFAULT_THINKING_BUDGETS`).
pub fn thinking_budget_for_level(level: ThinkingLevel) -> u64 {
    match level {
        ThinkingLevel::Off => 0,
        ThinkingLevel::Minimal => 1024,
        ThinkingLevel::Low => 2048,
        ThinkingLevel::Medium => 8192,
        _ => 16384,
    }
}

/// Start streaming a response. Never fails synchronously: errors arrive as `StreamEvent::Error`.
pub fn stream(model: &Model, ctx: &Context, opts: StreamOptions) -> EventStream {
    let (mut em, rx) = Emitter::new(model);
    let model = model.clone();
    let ctx = ctx.clone();
    tokio::spawn(async move {
        let cancel = opts.cancel.clone();
        let result = match model.api.as_str() {
            "openai-completions" => openai_completions::stream_inner(model, ctx, opts, &mut em).await,
            "anthropic-messages" => anthropic_messages::stream_inner(model, ctx, opts, &mut em).await,
            "openai-responses" | "azure-openai-responses" => openai_responses::stream_inner(model, ctx, opts, &mut em).await,
            "google-generative-ai" => google::stream_inner(model, ctx, opts, &mut em).await,
            other => Err(anyhow::anyhow!(
                "API \"{other}\" is not supported yet in serpis (supported: {})",
                SUPPORTED_APIS.join(", ")
            )),
        };
        match result {
            Ok(()) => em.done(),
            Err(e) => em.fail(e, &cancel),
        }
    });
    rx
}
