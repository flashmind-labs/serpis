//! Example Rust plugin. Build with `cargo build -p hello` and copy (or symlink)
//! `target/debug/libhello.dylib` into `~/.config/serpis/plugins/`. Rebuilding reloads it.

use std::time::Instant;

use serpis::agent::{AgentTool, ToolContext, ToolResult};
use serpis::egui::{self, Rect};
use serpis::serde_json::{Value, json};
use serpis::{Buffer, Editor};

const GREETING: &str = "Hello from a Rust plugin";

/// A custom buffer kind: anything a buffer can do, a plugin can do.
struct Stopwatch {
    started: Instant,
}

impl Buffer for Stopwatch {
    fn kind(&self) -> &str {
        "stopwatch"
    }

    fn ui(&mut self, ui: &mut egui::Ui, rect: Rect, _focused: bool, ed: &mut Editor) {
        let t = self.started.elapsed().as_secs_f32();
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            format!("{t:.1}s"),
            egui::FontId::monospace(48.0),
            ed.theme.accent,
        );
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
    }

    serpis::impl_any!();
}

/// An agent tool implemented in Rust.
struct WordCount;

impl AgentTool for WordCount {
    fn name(&self) -> &str {
        "word_count"
    }
    fn description(&self) -> String {
        "Count the words in a piece of text.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]})
    }
    fn execute(&self, _id: String, args: Value, _ctx: ToolContext) -> serpis::agent::tools::BoxFutureResult {
        let n = args["text"].as_str().unwrap_or("").split_whitespace().count();
        Box::pin(async move { Ok(ToolResult::text(n.to_string())) })
    }
}

fn init(ed: &mut Editor, owner: &str) {
    ed.define_command("hello-plugin", "Greet from the example Rust plugin.", owner, |ed, _| {
        ed.message(GREETING);
        Ok(())
    });
    ed.bind("global", "C-c h", "hello-plugin", owner);
    ed.define_buffer_kind("stopwatch", owner, |_, _| Ok(Box::new(Stopwatch { started: Instant::now() })));
    ed.register_tool(owner, std::sync::Arc::new(WordCount));
    // Plugins see everything: e.g. react to every finished tool call.
    ed.add_hook("tool-finished", owner, |ed, p| {
        if p["name"] == "bash" {
            ed.message("hello plugin saw a bash call");
        }
        None
    });
}

serpis::plugin!(init);
