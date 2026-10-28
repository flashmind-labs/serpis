//! System prompt construction and project context loading (pi `system-prompt.ts`).

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use crate::tools::ToolRef;

#[derive(Debug, Clone, Default)]
pub struct ContextFile {
    pub path: PathBuf,
    pub content: String,
}

#[derive(Default)]
pub struct PromptOptions {
    /// Replaces the default preamble/tools/rules.
    pub custom_prompt: Option<String>,
    /// Extra rule bullets.
    pub guidelines: Vec<String>,
    /// Appended before project context.
    pub append: Option<String>,
    /// Extra tagged sections (`<name>…</name>`).
    pub sections: BTreeMap<String, String>,
    pub context_files: Vec<ContextFile>,
}

const CONTEXT_FILE_NAMES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];

/// Global `~/.serpis/agent/AGENTS.md`, then every ancestor of `cwd` from the root down.
pub fn load_context_files(cwd: &Path, agent_dir: &Path) -> Vec<ContextFile> {
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    let mut push = |dir: &Path| {
        for name in CONTEXT_FILE_NAMES {
            let p = dir.join(name);
            if let Ok(content) = std::fs::read_to_string(&p) {
                let key = std::fs::canonicalize(&p).unwrap_or(p.clone());
                if seen.insert(key) {
                    files.push(ContextFile { path: p, content });
                }
                break;
            }
        }
    };
    push(agent_dir);
    let ancestors: Vec<&Path> = cwd.ancestors().collect();
    for dir in ancestors.into_iter().rev() {
        push(dir);
    }
    files
}

fn build_rules(tools: &[ToolRef], extra: &[String]) -> String {
    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    let mut rules: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    let mut add = |r: &str| {
        let r = r.trim();
        if !r.is_empty() && seen.insert(r.to_string()) {
            rules.push(r.to_string());
        }
    };
    if names.contains(&"bash") && !names.contains(&"grep") && !names.contains(&"find") && !names.contains(&"ls") {
        add("Use bash for file operations like ls, rg, find");
    }
    for t in tools {
        for g in t.prompt_guidelines() {
            add(&g);
        }
    }
    for g in extra {
        add(g);
    }
    add("Be concise in your responses");
    add("Show file paths clearly when working with files");
    rules.iter().map(|r| format!("- {r}")).collect::<Vec<_>>().join("\n")
}

/// What the agent knows of serpis itself: with `serpis_api` (an editor around the chat), that
/// requests about serpis are about the editor it runs in and how to act on them.
fn docs(tools: &[ToolRef]) -> String {
    if tools.iter().any(|t| t.name() == "serpis_api") {
        "You run inside serpis, the editor the user is talking to you from: an extensible, Emacs-like harness, configured and extended in Lua. \
         A request about serpis itself (its fonts, theme, keys, windows, sidebar, settings or commands; how to use it; changing how it behaves) is about this editor, not the project in the working directory. For one:\n\
         - Call serpis_api first, not serpis's source code: topic \"guide\" says how to change the user's config and write extensions, \"usage\" how serpis is used (with the user's keys), \"variables\" and \"commands\" what can be set and run (`filter` narrows them).\n\
         - Make lasting changes in the user's config files, then load and check them with serpis_lua.".into()
    } else {
        "Serpis, the harness running you, is an extensible, Emacs-like editor configured in Lua (~/.config/serpis: init.lua, lua/, extensions/).".into()
    }
}

/// Build the full system prompt: preamble, `<tools>`, `<rules>`, `<docs>`, addendum, project context, cwd.
pub fn build_system_prompt(tools: &[ToolRef], cwd: &Path, opts: &PromptOptions) -> String {
    let mut sections: Vec<(String, String)> = Vec::new();
    let preamble = match &opts.custom_prompt {
        Some(p) => p.clone(),
        None => {
            let list: Vec<String> = tools
                .iter()
                .filter_map(|t| t.prompt_snippet().map(|s| format!("- {}: {s}", t.name())))
                .collect();
            let list = if list.is_empty() { "(none)".to_string() } else { list.join("\n") };
            sections.push((
                "tools".into(),
                format!("{list}\n\nIn addition to the tools above, you may have access to other custom tools depending on the project."),
            ));
            sections.push(("rules".into(), build_rules(tools, &opts.guidelines)));
            sections.push(("docs".into(), docs(tools)));
            "You are an expert coding assistant operating inside serpis, a coding agent harness. You help users by reading files, executing commands, editing code, and writing new files.".to_string()
        }
    };
    if let Some(a) = opts.append.as_ref().filter(|a| !a.is_empty()) {
        sections.push(("addendum".into(), a.clone()));
    }
    if !opts.context_files.is_empty() {
        let mut s = String::from("Project-specific instructions and guidelines:");
        for f in &opts.context_files {
            s.push_str(&format!(
                "\n\n<project_instructions path=\"{}\">\n{}\n</project_instructions>",
                f.path.display(),
                f.content
            ));
        }
        sections.push(("project_context".into(), s));
    }
    sections.push(("cwd".into(), cwd.to_string_lossy().replace('\\', "/")));
    for (k, v) in &opts.sections {
        if !v.is_empty() {
            sections.push((k.clone(), v.clone()));
        }
    }
    let mut out = preamble;
    for (name, content) in sections {
        out.push_str(&format!("\n\n<{name}>\n{content}\n</{name}>"));
    }
    out
}
