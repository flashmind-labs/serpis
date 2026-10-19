//! `write` tool (pi `write.ts`).

use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::{Value, json};

use super::path_utils::resolve_to_cwd;
use super::*;

pub struct WriteTool;

impl AgentTool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }
    fn description(&self) -> String {
        "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Automatically creates parent directories.".into()
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path to the file to write (relative or absolute)"},
                "content": {"type": "string", "description": "Content to write to the file"}
            },
            "required": ["path", "content"]
        })
    }
    fn prompt_snippet(&self) -> Option<String> {
        Some("Create or overwrite files".into())
    }
    fn prompt_guidelines(&self) -> Vec<String> {
        vec!["Use write only for new files or complete rewrites.".into()]
    }

    fn execute(&self, _id: String, args: Value, ctx: ToolContext) -> BoxFuture<'static, anyhow::Result<ToolResult>> {
        async move {
            let path = arg_str(&args, "path").unwrap_or("").to_string();
            let content = arg_str(&args, "content").unwrap_or("").to_string();
            let abs = resolve_to_cwd(&path, &ctx.cwd);
            with_file_mutation_queue(&abs, async {
                ctx.check_cancelled()?;
                if let Some(dir) = abs.parent() {
                    tokio::fs::create_dir_all(dir).await?;
                }
                ctx.check_cancelled()?;
                let previous = tokio::fs::read_to_string(&abs).await.ok();
                tokio::fs::write(&abs, &content).await?;
                ctx.check_cancelled()?;
                // Renderers show a diff against the previous content when there was one.
                let details = match previous {
                    Some(old) if old != content => {
                        let d = super::edit_diff::generate_diff_string(&old, &content, 4);
                        json!({"diff": d.diff, "existed": true})
                    }
                    Some(_) => json!({"existed": true}),
                    None => json!({"existed": false}),
                };
                Ok(ToolResult::text(format!("Successfully wrote to {path}")).with_details(details))
            })
            .await
        }
        .boxed()
    }
}
