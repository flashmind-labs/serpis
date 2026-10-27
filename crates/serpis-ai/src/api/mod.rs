//! API adapters, one per wire protocol, dispatched by `Model::api`.

pub mod anthropic_messages;
pub mod google;
pub mod openai_completions;
pub mod openai_responses;

use std::collections::HashMap;

use serde_json::Value;

use crate::stream::{Emitter, EventStream, StreamOptions, http_client, send_with_retry};
use crate::types::{Context, Model, ThinkingLevel};

pub const SUPPORTED_APIS: &[&str] = &[
    "openai-completions",
    "anthropic-messages",
    "openai-responses",
    "google-generative-ai",
];

/// Whether a chat can run models of `api`: serpis's own adapters, or Claude Code.
pub fn chat_api(api: &str) -> bool {
    SUPPORTED_APIS.contains(&api) || api == "azure-openai-responses" || api == crate::claude_code::PROVIDER
}

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

/// The HTTP request an adapter sends, auth included. Building it is pure, so tests and the
/// prompt inspector see exactly the bytes a provider would.
#[derive(Debug, Clone)]
pub struct Request {
    pub url: String,
    pub headers: HashMap<String, String>,
    pub body: Value,
}

impl Request {
    /// A header by case-insensitive name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    /// Add an auth header unless it is empty or the configured headers already set it.
    fn with_auth(mut self, name: &str, value: String) -> Self {
        if !value.is_empty() && self.header(name).is_none() {
            self.headers.insert(name.into(), value);
        }
        self
    }

    async fn send(&self, opts: &StreamOptions) -> anyhow::Result<reqwest::Response> {
        let client = http_client();
        send_with_retry(
            || {
                let mut rb = client.post(&self.url).json(&self.body);
                for (k, v) in &self.headers {
                    rb = rb.header(k, v);
                }
                rb
            },
            opts.max_retries,
            &opts.cancel,
        )
        .await
    }
}

/// The request `stream` would send for this model, context and options.
pub fn build_request(model: &Model, ctx: &Context, opts: &StreamOptions) -> anyhow::Result<Request> {
    match model.api.as_str() {
        "openai-completions" => openai_completions::build_request(model, ctx, opts),
        "anthropic-messages" => anthropic_messages::build_request(model, ctx, opts),
        "openai-responses" | "azure-openai-responses" => openai_responses::build_request(model, ctx, opts),
        "google-generative-ai" => google::build_request(model, ctx, opts),
        other => Err(anyhow::anyhow!(
            "API \"{other}\" is not supported yet in serpis (supported: {})",
            SUPPORTED_APIS.join(", ")
        )),
    }
}

/// Start streaming a response. Never fails synchronously: errors arrive as `StreamEvent::Error`.
pub fn stream(model: &Model, ctx: &Context, opts: StreamOptions) -> EventStream {
    let (mut em, rx) = Emitter::new(model);
    let model = model.clone();
    let ctx = ctx.clone();
    tokio::spawn(async move {
        let cancel = opts.cancel.clone();
        let result = match build_request(&model, &ctx, &opts) {
            Err(e) => Err(e),
            Ok(req) => match req.send(&opts).await {
                Err(e) => Err(e),
                Ok(resp) => match model.api.as_str() {
                    "openai-completions" => openai_completions::stream_inner(resp, model, opts, &mut em).await,
                    "anthropic-messages" => anthropic_messages::stream_inner(resp, model, ctx, opts, &mut em).await,
                    "google-generative-ai" => google::stream_inner(resp, model, opts, &mut em).await,
                    _ => openai_responses::stream_inner(resp, model, opts, &mut em).await,
                },
            },
        };
        match result {
            Ok(()) => em.done(),
            Err(e) => em.fail(e, &cancel),
        }
    });
    rx
}
