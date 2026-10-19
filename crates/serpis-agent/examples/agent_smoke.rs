use std::sync::Arc;
use serpis_agent::*;
use serpis_ai::*;

#[tokio::main]
async fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (provider, id, cwd, prompt) = (&a[1], &a[2], std::path::PathBuf::from(&a[3]), a[4].clone());
    let reg = Registry::new(Paths::default_paths());
    let model = reg.find(provider, id).expect("model");
    let tools = builtin_tools();
    let system_prompt = system_prompt::build_system_prompt(&tools, &cwd, &Default::default());
    let cfg = AgentConfig { model, reasoning: ThinkingLevel::Low, tools, system_prompt, api_key: reg.api_key(provider),
        headers: Default::default(), session_id: "smoke".into(), cwd, native_web_search: false, max_tokens: None, hooks: None, sequential_tools: false };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    run(vec![], vec![Message::user_text(prompt)], cfg, Arc::new(move |e| { let _ = tx.send(e); }));
    while let Some(e) = rx.recv().await {
        match e {
            AgentEvent::MessageDelta(StreamEvent::TextDelta { delta, .. }) => print!("{delta}"),
            AgentEvent::ToolStart { name, args, .. } => println!("\n>> {name} {args}"),
            AgentEvent::ToolEnd { result, .. } => println!("<< err={} {}", result.is_error, result.content.iter().map(|c| match c { UserContent::Text{text} => text.chars().take(300).collect::<String>(), _ => "[img]".into() }).collect::<String>()),
            AgentEvent::AgentEnd { error } => { println!("\nEND {error:?}"); break; }
            _ => {}
        }
    }
}
