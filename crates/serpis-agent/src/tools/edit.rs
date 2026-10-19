//! `edit` tool (pi `edit.ts`): one or more exact replacements against the original file.

use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::{Value, json};

use super::edit_diff::*;
use super::path_utils::resolve_to_cwd;
use super::*;

pub struct EditTool;

fn is_single_edit(v: &Value) -> bool {
    v.get("oldText").is_some_and(Value::is_string) && v.get("newText").is_some_and(Value::is_string)
}

impl AgentTool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }
    fn description(&self) -> String {
        "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. If two changes affect the same block or nearby lines, merge them into one edit instead of emitting overlapping edits. Do not include large unchanged regions just to connect distant changes.".into()
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path to the file to edit (relative or absolute)"},
                "edits": {
                    "type": "array",
                    "description": "One or more targeted replacements. Each edit is matched against the original file, not incrementally. Do not include overlapping or nested edits. If two changes touch the same block or nearby lines, merge them into one edit instead.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "oldText": {"type": "string", "description": "Exact text for one targeted replacement. It must be unique in the original file and must not overlap with any other edits[].oldText in the same call."},
                            "newText": {"type": "string", "description": "Replacement text for this targeted edit."}
                        },
                        "required": ["oldText", "newText"]
                    }
                }
            },
            "required": ["path", "edits"]
        })
    }
    fn prompt_snippet(&self) -> Option<String> {
        Some("Make precise file edits with exact text replacement, including multiple disjoint edits in one call".into())
    }
    fn prompt_guidelines(&self) -> Vec<String> {
        vec![
            "Use edit for precise changes (edits[].oldText must match exactly)".into(),
            "When changing multiple separate locations in one file, use one edit call with multiple entries in edits[] instead of multiple edit calls".into(),
            "Each edits[].oldText is matched against the original file, not after earlier edits are applied. Do not emit overlapping or nested edits. Merge nearby changes into one edit.".into(),
            "Keep edits[].oldText as small as possible while still being unique in the file. Do not pad with large unchanged regions.".into(),
        ]
    }

    /// Some models send `edits` as a JSON string, a single object, or legacy top-level oldText/newText.
    fn prepare_arguments(&self, mut args: Value) -> Value {
        let Some(obj) = args.as_object_mut() else { return args };
        if let Some(Value::String(s)) = obj.get("edits") {
            if let Ok(parsed) = serde_json::from_str::<Value>(s) {
                if parsed.is_array() {
                    obj.insert("edits".into(), parsed);
                } else if is_single_edit(&parsed) {
                    obj.insert("edits".into(), Value::Array(vec![parsed]));
                }
            }
        } else if obj.get("edits").is_some_and(is_single_edit) {
            let single = obj.remove("edits").unwrap();
            obj.insert("edits".into(), Value::Array(vec![single]));
        }
        if let (Some(Value::String(o)), Some(Value::String(n))) = (obj.get("oldText").cloned(), obj.get("newText").cloned()) {
            let mut edits = obj.get("edits").and_then(Value::as_array).cloned().unwrap_or_default();
            edits.push(json!({"oldText": o, "newText": n}));
            obj.remove("oldText");
            obj.remove("newText");
            obj.insert("edits".into(), Value::Array(edits));
        }
        args
    }

    fn execute(&self, _id: String, args: Value, ctx: ToolContext) -> BoxFuture<'static, anyhow::Result<ToolResult>> {
        async move {
            let path = arg_str(&args, "path").unwrap_or("").to_string();
            let edits: Vec<Edit> = args
                .get("edits")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|e| Edit {
                            old_text: e["oldText"].as_str().unwrap_or("").to_string(),
                            new_text: e["newText"].as_str().unwrap_or("").to_string(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            if edits.is_empty() {
                anyhow::bail!("Edit tool input is invalid. edits must contain at least one replacement.");
            }
            let abs = resolve_to_cwd(&path, &ctx.cwd);
            with_file_mutation_queue(&abs, async {
                ctx.check_cancelled()?;
                let meta = tokio::fs::metadata(&abs).await;
                let writable = meta.as_ref().is_ok_and(|m| !m.permissions().readonly());
                if !writable {
                    let code = match &meta {
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => "ENOENT".to_string(),
                        Err(e) => e.to_string(),
                        Ok(_) => "EACCES".to_string(),
                    };
                    anyhow::bail!("Could not edit file: {path}. Error code: {code}.");
                }
                let raw = tokio::fs::read(&abs).await?;
                ctx.check_cancelled()?;
                let raw = String::from_utf8_lossy(&raw).into_owned();
                let (bom, content) = split_bom(&raw);
                let ending = detect_line_ending(content);
                let normalized = normalize_to_lf(content);
                let applied = apply_edits_to_normalized_content(&normalized, &edits, &path).map_err(anyhow::Error::msg)?;
                ctx.check_cancelled()?;
                let final_content = format!("{bom}{}", restore_line_endings(&applied.new_content, ending));
                tokio::fs::write(&abs, final_content).await?;
                ctx.check_cancelled()?;
                let d = generate_diff_string(&applied.base_content, &applied.new_content, 4);
                let patch = generate_unified_patch(&path, &applied.base_content, &applied.new_content, 4);
                Ok(ToolResult::text(format!("Successfully replaced {} block(s) in {path}.", edits.len())).with_details(json!({
                    "diff": d.diff,
                    "patch": patch,
                    "firstChangedLine": d.first_changed_line,
                })))
            })
            .await
        }
        .boxed()
    }
}
