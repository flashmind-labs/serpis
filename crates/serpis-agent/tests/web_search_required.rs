//! A run stops when every web search provider fails (`web_search_required`), and goes on
//! with the failure when that's off. Against the mock provider, with a search backend that
//! always fails: nothing leaves the machine.

use std::sync::Arc;

use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::json;
use serpis_agent::tools::web_search::{SearchBackend, SearchOptions, SearchResult, WebSearchTool};
use serpis_agent::{AgentConfig, AgentEvent, run};
use serpis_ai::*;
use serpis_testkit::{Mock, commands};

struct Down;
impl SearchBackend for Down {
    fn id(&self) -> &str {
        "down"
    }
    fn available(&self) -> bool {
        true
    }
    fn search(&self, _q: String, _o: SearchOptions) -> BoxFuture<'static, anyhow::Result<Vec<SearchResult>>> {
        async { anyhow::bail!("search service is down") }.boxed()
    }
}

/// The run's error, and the transcript's last message.
async fn search_with_providers_down(required: bool) -> (Option<String>, Message) {
    let mock = Mock::start(commands()).await;
    let model: Model = serde_json::from_value(json!({
        "id": "anthropic/claude-sonnet-4.5", "api": "openai-completions", "provider": "openrouter",
        "baseUrl": format!("{}/api/v1", mock.base), "reasoning": true, "input": ["text"],
    }))
    .unwrap();
    let cfg = AgentConfig {
        model,
        reasoning: ThinkingLevel::Off,
        tools: vec![Arc::new(WebSearchTool { backends: vec![Arc::new(Down)] })],
        system_prompt: "test".into(),
        api_key: Some("test-key".into()),
        headers: Default::default(),
        session_id: "chat-search".into(),
        cwd: std::env::temp_dir(),
        native_web_search: false,
        web_search_required: required,
        max_tokens: None,
        max_retries: Some(0),
        cache_retention: CacheRetention::None,
        hooks: None,
        sequential_tools: false,
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let prompt = Message::User { content: vec![UserContent::Text { text: r#"CALL web_search {"query": "today's news"}"#.into() }], timestamp: 1 };
    let _h = run(Vec::new(), vec![prompt], cfg, Arc::new(move |ev| drop(tx.send(ev))));
    let mut last = None;
    while let Some(ev) = rx.recv().await {
        match ev {
            AgentEvent::MessageEnd(m) => last = Some(m),
            AgentEvent::AgentEnd { error } => return (error, last.unwrap()),
            _ => {}
        }
    }
    panic!("the run never ended");
}

#[tokio::test]
async fn a_run_stops_when_every_search_provider_fails() {
    let (error, last) = search_with_providers_down(true).await;
    assert!(error.as_deref().is_some_and(|e| e.contains("Web search failed")), "{error:?}");
    assert!(matches!(last, Message::ToolResult(r) if r.is_error && r.details["unavailable"] == true));
}

#[tokio::test]
async fn with_search_not_required_the_run_goes_on() {
    let (error, last) = search_with_providers_down(false).await;
    assert_eq!(error, None);
    assert!(matches!(last, Message::Assistant(_)));
}
