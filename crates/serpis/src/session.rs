//! Chat sessions on disk, in pi's session format (v3), so serpis and pi can resume each
//! other's conversations.
//!
//! A session is a JSONL file: a `session` header (`id`, `cwd`, `timestamp`), then entries that
//! each name their `parentId`, forming a tree. The conversation is the path from the last
//! entry back to the root: `message` entries (pi-ai messages), `model_change`,
//! `thinking_level_change`, `session_info` (a name), `compaction` and `branch_summary`.
//! Files live in `<dir>/--<cwd with / as ->--/<timestamp>_<id>.jsonl`.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::{Value, json};
use serpis_ai::{Message, UserContent};

pub const VERSION: u64 = 3;

/// `2026-10-12T12:02:33.258Z` for a time.
pub fn iso(t: SystemTime) -> String {
    let ms = t.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
    let (secs, milli) = (ms.div_euclid(1000), ms.rem_euclid(1000));
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since the epoch (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{milli:03}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// pi's directory name for sessions started in `cwd`.
pub fn dir_for(cwd: &Path) -> String {
    let s = cwd.to_string_lossy();
    let s = s.trim_start_matches(['/', '\\']);
    format!("--{}--", s.replace(['/', '\\', ':'], "-"))
}

/// A JSON object with `head` keys first, in order, then `rest` (pi writes `type` first, and
/// readers skim for it).
fn ordered(head: &[(&str, Value)], rest: Value) -> String {
    let mut parts: Vec<String> = head.iter().map(|(k, v)| format!("{}:{v}", Value::from(*k))).collect();
    if let Value::Object(m) = rest {
        parts.extend(m.iter().filter(|(k, _)| !head.iter().any(|(h, _)| h == k)).map(|(k, v)| format!("{}:{v}", Value::from(k.as_str()))));
    }
    format!("{{{}}}", parts.join(","))
}

fn short_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}

/// Appends entries to a session file, creating it (with its header) on the first write.
#[derive(Debug, Clone)]
pub struct SessionWriter {
    pub id: String,
    pub path: PathBuf,
    pub cwd: PathBuf,
    /// The entry new entries hang under (the current leaf).
    pub leaf: Option<String>,
    created: bool,
    /// Last model and thinking level written, so changes are recorded once.
    model: Option<(String, String)>,
    thinking: Option<String>,
}

impl SessionWriter {
    /// A new session `id` for `cwd` under `root` (nothing is written until the first entry). The
    /// id is the chat's session id, so a resumed chat keeps routing to the same provider cache.
    pub fn new(root: &Path, cwd: &Path, id: &str) -> Self {
        let id = id.to_string();
        let stamp = iso(SystemTime::now()).replace([':', '.'], "-");
        let path = root.join(dir_for(cwd)).join(format!("{stamp}_{id}.jsonl"));
        SessionWriter { id, path, cwd: cwd.to_path_buf(), leaf: None, created: false, model: None, thinking: None }
    }

    /// Continue an existing session file after its last entry.
    pub fn resume(loaded: &Loaded) -> Self {
        SessionWriter {
            id: loaded.id.clone(),
            path: loaded.path.clone(),
            cwd: loaded.cwd.clone(),
            leaf: loaded.leaf.clone(),
            created: true,
            model: loaded.model.clone(),
            thinking: loaded.thinking.clone(),
        }
    }

    pub fn exists(&self) -> bool {
        self.created
    }

    fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        if !self.created {
            if let Some(d) = self.path.parent() {
                std::fs::create_dir_all(d)?;
            }
            let header = ordered(&[("type", json!("session")), ("version", json!(VERSION)), ("id", json!(self.id)), ("timestamp", json!(iso(SystemTime::now()))), ("cwd", json!(self.cwd.to_string_lossy()))], Value::Null);
            std::fs::write(&self.path, format!("{header}\n"))?;
            self.created = true;
        }
        let mut f = std::fs::OpenOptions::new().append(true).open(&self.path)?;
        writeln!(f, "{line}")
    }

    /// Append an entry of `kind` with `fields`; returns its id.
    pub fn append(&mut self, kind: &str, fields: Value) -> std::io::Result<String> {
        let id = short_id();
        let line = ordered(&[("type", json!(kind)), ("id", json!(id)), ("parentId", json!(self.leaf)), ("timestamp", json!(iso(SystemTime::now())))], fields);
        self.write_line(&line)?;
        self.leaf = Some(id.clone());
        Ok(id)
    }

    /// Record the model and thinking level if they changed.
    pub fn note_settings(&mut self, provider: &str, model: &str, thinking: &str) -> std::io::Result<()> {
        let m = (provider.to_string(), model.to_string());
        if self.model.as_ref() != Some(&m) {
            self.append("model_change", json!({"provider": provider, "modelId": model}))?;
            self.model = Some(m);
        }
        if self.thinking.as_deref() != Some(thinking) {
            self.append("thinking_level_change", json!({"thinkingLevel": thinking}))?;
            self.thinking = Some(thinking.to_string());
        }
        Ok(())
    }

    pub fn message(&mut self, m: &Message) -> std::io::Result<String> {
        self.append("message", json!({"message": m}))
    }

    pub fn set_name(&mut self, name: &str) -> std::io::Result<String> {
        self.append("session_info", json!({"name": name}))
    }
}

/// A session read back from disk: the conversation on its active branch.
#[derive(Debug, Clone, Default)]
pub struct Loaded {
    pub path: PathBuf,
    pub id: String,
    pub cwd: PathBuf,
    pub messages: Vec<Message>,
    /// (provider, model id) last in effect.
    pub model: Option<(String, String)>,
    pub thinking: Option<String>,
    pub name: Option<String>,
    pub leaf: Option<String>,
    /// `custom` entries on the active branch: (customType, data), oldest first.
    pub entries: Vec<(String, Value)>,
}

/// pi-ai message JSON → [`Message`], accepting pi's variants (string user content, bash
/// executions run with `!`). Other custom roles are skipped.
pub fn parse_message(v: &Value) -> Option<Message> {
    let mut v = v.clone();
    match v.get("role").and_then(Value::as_str)? {
        "user" => {
            if let Some(s) = v.get("content").and_then(Value::as_str).map(str::to_string) {
                v["content"] = json!([{"type": "text", "text": s}]);
            }
        }
        "bashExecution" => {
            let cmd = v.get("command").and_then(Value::as_str).unwrap_or("");
            let out = v.get("output").and_then(Value::as_str).unwrap_or("");
            return Some(Message::user_text(format!("Ran `{cmd}`\n```\n{}\n```", out.trim_end())));
        }
        "assistant" | "toolResult" => {}
        _ => return None,
    }
    if v.get("timestamp").is_none_or(|t| !t.is_u64()) {
        v["timestamp"] = json!(0);
    }
    serde_json::from_value(v).ok()
}

fn summary_message(prefix: &str, summary: &str) -> Message {
    Message::User {
        content: vec![UserContent::Text { text: format!("{prefix}\n\n<summary>\n{summary}\n</summary>") }],
        timestamp: 0,
    }
}

/// Read a session: follow the last entry back to the root and rebuild the conversation.
pub fn load(path: &Path) -> anyhow::Result<Loaded> {
    let text = std::fs::read_to_string(path)?;
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let header: Value = serde_json::from_str(lines.next().ok_or_else(|| anyhow::anyhow!("empty session"))?)?;
    if header.get("type").and_then(Value::as_str) != Some("session") {
        anyhow::bail!("{} is not a session file", path.display());
    }
    let entries: Vec<Value> = lines.filter_map(|l| serde_json::from_str(l).ok()).collect();
    let by_id: HashMap<&str, &Value> = entries.iter().filter_map(|e| Some((e.get("id")?.as_str()?, e))).collect();
    // Version 1 files have no tree: every entry follows the previous one.
    let tree = entries.iter().any(|e| e.get("parentId").is_some());
    let path_entries: Vec<&Value> = if tree {
        let mut chain = Vec::new();
        let mut cur = entries.last().and_then(|e| e.get("id")).and_then(Value::as_str);
        while let Some(id) = cur {
            let Some(e) = by_id.get(id) else { break };
            if chain.len() > entries.len() {
                break;
            }
            chain.push(*e);
            cur = e.get("parentId").and_then(Value::as_str);
        }
        chain.reverse();
        chain
    } else {
        entries.iter().collect()
    };
    let mut out = Loaded {
        path: path.to_path_buf(),
        id: header.get("id").and_then(Value::as_str).unwrap_or_default().to_string(),
        cwd: PathBuf::from(header.get("cwd").and_then(Value::as_str).unwrap_or_default()),
        leaf: entries.last().and_then(|e| e.get("id")).and_then(Value::as_str).map(str::to_string),
        ..Default::default()
    };
    // The latest compaction replaces everything before its first kept entry with a summary.
    let compaction = path_entries.iter().rposition(|e| e.get("type").and_then(Value::as_str) == Some("compaction"));
    let mut start = 0;
    if let Some(ci) = compaction {
        let c = path_entries[ci];
        let kept = c.get("firstKeptEntryId").and_then(Value::as_str);
        start = kept.and_then(|k| path_entries.iter().position(|e| e.get("id").and_then(Value::as_str) == Some(k))).unwrap_or(ci + 1);
        if let Some(s) = c.get("summary").and_then(Value::as_str) {
            out.messages.push(summary_message("The conversation history before this point was compacted into the following summary:", s));
        }
    }
    for (i, e) in path_entries.iter().enumerate() {
        let kind = e.get("type").and_then(Value::as_str).unwrap_or("");
        let s = |k: &str| e.get(k).and_then(Value::as_str).map(str::to_string);
        match kind {
            "model_change" => {
                if let (Some(p), Some(m)) = (s("provider"), s("modelId")) {
                    out.model = Some((p, m));
                }
            }
            "thinking_level_change" => out.thinking = s("thinkingLevel"),
            "session_info" => out.name = s("name").filter(|n| !n.is_empty()),
            "custom" => {
                if let Some(t) = s("customType") {
                    out.entries.push((t, e.get("data").cloned().unwrap_or(Value::Null)));
                }
            }
            "message" if i >= start && Some(i) != compaction => {
                if let Some(m) = e.get("message").and_then(parse_message) {
                    if let Message::Assistant(a) = &m {
                        out.model = Some((a.provider.clone(), a.model.clone()));
                    }
                    out.messages.push(m);
                }
            }
            "branch_summary" if i >= start => {
                if let Some(sm) = s("summary") {
                    out.messages.push(summary_message("The following is a summary of a branch that this conversation came back from:", &sm));
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// What a session picker shows, read without parsing whole conversations.
#[derive(Debug, Clone, Default)]
pub struct Info {
    pub path: PathBuf,
    pub id: String,
    pub cwd: PathBuf,
    pub name: Option<String>,
    pub first_prompt: String,
    pub last_prompt: String,
    /// User and assistant messages.
    pub messages: usize,
    pub model: Option<String>,
    pub created: String,
    pub modified: Option<SystemTime>,
    pub size: u64,
    /// The project the session ran in (filled in by the scanner).
    pub project: Option<PathBuf>,
    /// Whether that project (or the session's directory) still exists, checked by the
    /// scanner: a lookup can block for a while (automounted `/home`, network volumes), so the
    /// UI never stats session paths itself.
    pub exists: bool,
}

impl Info {
    /// Name, else the first prompt.
    pub fn title(&self) -> String {
        self.name.clone().unwrap_or_else(|| self.first_prompt.lines().next().unwrap_or("").to_string())
    }
}

fn user_text(m: &Value) -> String {
    match m.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(a)) => a.iter().filter_map(|c| c.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join(" "),
        _ => String::new(),
    }
}

/// Summarize one session file.
pub fn info(path: &Path) -> Option<Info> {
    let meta = std::fs::metadata(path).ok()?;
    let f = std::fs::File::open(path).ok()?;
    let mut lines = std::io::BufReader::new(f).lines();
    let header: Value = serde_json::from_str(&lines.next()?.ok()?).ok()?;
    if header.get("type").and_then(Value::as_str) != Some("session") {
        return None;
    }
    let mut i = Info {
        path: path.to_path_buf(),
        id: header.get("id").and_then(Value::as_str).unwrap_or_default().to_string(),
        cwd: PathBuf::from(header.get("cwd").and_then(Value::as_str).unwrap_or_default()),
        created: header.get("timestamp").and_then(Value::as_str).unwrap_or_default().to_string(),
        modified: meta.modified().ok(),
        size: meta.len(),
        ..Default::default()
    };
    for line in lines.map_while(Result::ok) {
        // Cheap checks first: most lines are long tool results.
        let mut cut = line.len().min(160);
        while !line.is_char_boundary(cut) {
            cut -= 1;
        }
        let head = &line[..cut];
        if head.contains("\"type\":\"message\"") {
            let user = line.contains("\"role\":\"user\"");
            let assistant = !user && line.contains("\"role\":\"assistant\"");
            if user || assistant {
                i.messages += 1;
            }
            if user {
                if let Some(m) = serde_json::from_str::<Value>(&line).ok().and_then(|v| v.get("message").cloned()) {
                    let t = user_text(&m);
                    if i.first_prompt.is_empty() {
                        i.first_prompt = t.clone();
                    }
                    i.last_prompt = t;
                }
            }
        } else if head.contains("\"type\":\"session_info\"") {
            i.name = serde_json::from_str::<Value>(&line).ok().and_then(|v| v.get("name")?.as_str().map(str::to_string)).filter(|n| !n.is_empty());
        } else if head.contains("\"type\":\"model_change\"") {
            i.model = serde_json::from_str::<Value>(&line).ok().and_then(|v| v.get("modelId")?.as_str().map(str::to_string));
        }
    }
    Some(i)
}

/// Every session under `roots`, newest first. `cache` (by path) skips files whose size and
/// modification time are unchanged.
pub fn list(roots: &[PathBuf], cache: &mut HashMap<PathBuf, Info>) -> Vec<Info> {
    let mut out = Vec::new();
    for root in roots {
        let Ok(dirs) = std::fs::read_dir(root) else { continue };
        for d in dirs.flatten() {
            let Ok(files) = std::fs::read_dir(d.path()) else { continue };
            for f in files.flatten() {
                let p = f.path();
                if p.extension().is_none_or(|e| e != "jsonl") {
                    continue;
                }
                let meta = f.metadata().ok();
                let (size, mtime) = (meta.as_ref().map(|m| m.len()).unwrap_or(0), meta.and_then(|m| m.modified().ok()));
                let fresh = cache.get(&p).filter(|i| i.size == size && i.modified == mtime).cloned();
                let info = match fresh {
                    Some(i) => Some(i),
                    None => info(&p),
                };
                if let Some(i) = info {
                    cache.insert(p, i.clone());
                    // Sessions that never got a message aren't worth resuming.
                    if i.messages > 0 {
                        out.push(i);
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| b.modified.cmp(&a.modified));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serpis_ai::{AssistantContent, AssistantMessage, StopReason, ToolCall, ToolResultMessage, Usage};

    fn assistant(text: &str) -> Message {
        Message::Assistant(AssistantMessage {
            content: vec![
                AssistantContent::Text { text: text.into(), text_signature: None },
                AssistantContent::ToolCall(ToolCall { id: "t1".into(), name: "ls".into(), arguments: json!({}), thought_signature: None, arguments_error: None }),
            ],
            api: "openai-completions".into(),
            provider: "openrouter".into(),
            model: "m1".into(),
            response_id: None,
            response_model: None,
            upstream: None,
            usage: Usage::default(),
            stop_reason: StopReason::ToolUse,
            raw_stop_reason: None,
            error_message: None,
            timestamp: 1,
        })
    }

    #[test]
    fn iso_dates() {
        let t = SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(1_791_806_553_258);
        assert_eq!(iso(t), "2026-10-12T12:02:33.258Z");
        assert_eq!(dir_for(Path::new("/Users/ivan/projects/pi-web")), "--Users-ivan-projects-pi-web--");
    }

    #[test]
    fn write_load_resume_roundtrip() {
        let root = std::env::temp_dir().join(format!("serpis-sessions-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut w = SessionWriter::new(&root, Path::new("/tmp/proj"), &uuid::Uuid::now_v7().to_string());
        assert!(!w.path.exists(), "nothing written before the first entry");
        w.note_settings("openrouter", "m1", "high").unwrap();
        w.message(&Message::user_text("list files")).unwrap();
        w.message(&assistant("sure")).unwrap();
        w.message(&Message::ToolResult(ToolResultMessage { tool_call_id: "t1".into(), tool_name: "ls".into(), content: vec![UserContent::Text { text: "a\nb".into() }], is_error: false, details: Value::Null, timestamp: 2 })).unwrap();
        w.set_name("Listing").unwrap();
        let l = load(&w.path).unwrap();
        assert_eq!(l.messages.len(), 3);
        assert_eq!(l.cwd, PathBuf::from("/tmp/proj"));
        assert_eq!(l.model, Some(("openrouter".into(), "m1".into())));
        assert_eq!(l.thinking.as_deref(), Some("high"));
        assert_eq!(l.name.as_deref(), Some("Listing"));
        // Resuming appends to the same file, continuing the branch.
        let mut w2 = SessionWriter::resume(&l);
        w2.message(&Message::user_text("thanks")).unwrap();
        let l2 = load(&w.path).unwrap();
        assert_eq!(l2.messages.len(), 4);
        let infos = list(std::slice::from_ref(&root), &mut HashMap::new());
        assert_eq!(infos.len(), 1);
        assert_eq!((infos[0].messages, infos[0].first_prompt.as_str(), infos[0].last_prompt.as_str()), (3, "list files", "thanks"));
        assert_eq!(infos[0].title(), "Listing");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn loads_pi_branches_and_compaction() {
        let dir = std::env::temp_dir().join(format!("serpis-pi-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("s.jsonl");
        let lines = [
            json!({"type":"session","version":3,"id":"s1","timestamp":"t","cwd":"/x"}),
            json!({"type":"message","id":"a","parentId":null,"message":{"role":"user","content":"first","timestamp":1}}),
            json!({"type":"message","id":"b","parentId":"a","message":{"role":"system","content":""}}),
            json!({"type":"message","id":"c","parentId":"a","message":{"role":"user","content":[{"type":"text","text":"abandoned"}],"timestamp":2}}),
            json!({"type":"compaction","id":"d","parentId":"a","summary":"S","firstKeptEntryId":"e","tokensBefore":10}),
            json!({"type":"message","id":"e","parentId":"d","message":{"role":"bashExecution","command":"ls","output":"x\n","exitCode":0}}),
        ];
        std::fs::write(&p, lines.iter().map(|l| l.to_string() + "\n").collect::<String>()).unwrap();
        let l = load(&p).unwrap();
        // The active branch is a → d → e: the compaction summary, then the kept bash run.
        assert_eq!(l.messages.len(), 2);
        let text = |m: &Message| match m {
            Message::User { content, .. } => match &content[0] {
                UserContent::Text { text } => text.clone(),
                _ => String::new(),
            },
            _ => String::new(),
        };
        assert!(text(&l.messages[0]).contains("<summary>\nS\n</summary>"));
        assert!(text(&l.messages[1]).starts_with("Ran `ls`"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
