use serpis_ai::*;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (provider, id) = (args[1].as_str(), args[2].as_str());
    let reg = Registry::new(Paths::default_paths());
    let errs = reg.refresh_catalogs(false).await;
    if !errs.is_empty() { eprintln!("catalog errors: {errs:?}"); }
    println!("configured: {:?}, models: {}", reg.configured_providers(), reg.models().len());
    let model = reg.find(provider, id).expect("model not found");
    let ctx = Context {
        system_prompt: "You are terse.".into(),
        messages: vec![Message::user_text("Call the get_time tool with tz=UTC, then stop.")],
        tools: vec![Tool { name: "get_time".into(), description: "Get the time".into(),
            parameters: serde_json::json!({"type":"object","properties":{"tz":{"type":"string"}},"required":["tz"]}) }],
    };
    let mut rx = stream(&model, &ctx, StreamOptions { api_key: reg.api_key(provider), reasoning: Some(ThinkingLevel::Low), ..Default::default() });
    while let Some(ev) = rx.recv().await {
        match ev {
            StreamEvent::TextDelta { delta, .. } => print!("{delta}"),
            StreamEvent::ThinkingDelta { .. } => print!("."),
            StreamEvent::ToolCallEnd { tool_call, .. } => println!("\nTOOL {} {}", tool_call.name, tool_call.arguments),
            StreamEvent::Done(m) => { println!("\nDONE {:?} usage={:?}", m.stop_reason, m.usage); break; }
            StreamEvent::Error(m) => { println!("\nERROR {:?}", m.error_message); break; }
            _ => {}
        }
    }
}
