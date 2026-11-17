//! The report eval (evals/report/README.md): serpis's agent does real tasks on a fresh clone of
//! this repository, once per variant of the report skill, and the report it ends with is scored
//! as serpis reads it. It spends real money on OpenRouter: run it with scripts/report-eval.sh.
//!
//!   report_eval [--n 5] [--jobs 4] [--variants current,proposed] [--tasks a,b] [--commit HEAD]
//!               [--model openrouter/deepseek/deepseek-v4.1-flash-1017] [--thinking max]
//!               [--timeout-min 30] [--out DIR] [--no-prewarm]
//!   report_eval --score DIR      score a finished run again (no model calls)
//!
//! `current` is skills/report/SKILL.md as it is in the working tree; any other variant is
//! evals/report/variants/<name>.md. Chats run as the app runs them: on a serpis node (one per
//! variant, in this process), with the app's default settings, its lent tools and serpis's
//! default config, on a HOME of the run's own. Each run gets its own clone of the commit, so
//! nothing it does reaches this checkout or your serpis. Their cargo builds share
//! target/report-eval/cargo-target, so dependencies build once.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use serpis::buffers::chat::{check_mark, shaped_report, top_items};
use serpis::chat_review::{self, Ask, Comment, Framing};
use serpis_ai::*;

const SECTIONS: [&str; 4] = ["Needs you", "Changed", "Checked", "Not done"];

struct Args {
    n: usize,
    jobs: usize,
    variants: Vec<String>,
    tasks: Option<Vec<String>>,
    commit: String,
    provider: String,
    model: String,
    thinking: ThinkingLevel,
    timeout: Duration,
    out: Option<PathBuf>,
    prewarm: bool,
    score: Option<PathBuf>,
}

fn args() -> Args {
    let mut a = Args {
        n: 5,
        jobs: 4,
        variants: vec!["current".into(), "proposed".into()],
        tasks: None,
        commit: "HEAD".into(),
        provider: "openrouter".into(),
        model: "deepseek/deepseek-v4.1-flash-1017".into(),
        thinking: ThinkingLevel::Max,
        timeout: Duration::from_secs(30 * 60),
        out: None,
        prewarm: true,
        score: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut v = || it.next().unwrap_or_else(|| panic!("{k} needs a value"));
        match k.as_str() {
            "--n" => a.n = v().parse().expect("--n"),
            "--jobs" => a.jobs = v().parse().expect("--jobs"),
            "--variants" => a.variants = v().split(',').map(str::to_string).collect(),
            "--tasks" => a.tasks = Some(v().split(',').map(str::to_string).collect()),
            "--commit" => a.commit = v(),
            "--model" => {
                let m = v();
                let (p, id) = m.split_once('/').expect("--model provider/id");
                (a.provider, a.model) = (p.to_string(), id.to_string());
            }
            "--thinking" => a.thinking = serde_json::from_value(json!(v())).expect("--thinking: off, low, … max"),
            "--timeout-min" => a.timeout = Duration::from_secs(v().parse::<u64>().expect("--timeout-min") * 60),
            "--out" => a.out = Some(v().into()),
            "--no-prewarm" => a.prewarm = false,
            "--score" => a.score = Some(v().into()),
            _ => panic!("unknown argument {k}"),
        }
    }
    a
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git").arg("-C").arg(dir).args(args).output().expect("git");
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// A fresh clone of `commit` at `dir` (objects hardlinked: they never change).
fn clone(commit: &str, dir: &Path) {
    let o = Command::new("git").args(["clone", "--quiet", "--no-checkout"]).arg(root()).arg(dir).output().expect("git clone");
    assert!(o.status.success(), "git clone: {}", String::from_utf8_lossy(&o.stderr));
    git(dir, &["checkout", "--quiet", "--detach", commit]);
}

#[derive(Clone)]
struct Task {
    id: String,
    prompt: String,
    review: Option<Value>,
    /// A shell command run in the clone before the agent starts (a hook that blocks commits).
    setup: Option<String>,
}

fn tasks(only: &Option<Vec<String>>) -> Vec<Task> {
    let all: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(root().join("evals/report/tasks.json")).unwrap()).unwrap();
    all.into_iter()
        .map(|t| Task { id: t["id"].as_str().unwrap().into(), prompt: t["prompt"].as_str().unwrap().into(), review: t.get("review").cloned(), setup: t["setup"].as_str().map(Into::into) })
        .filter(|t| only.as_ref().is_none_or(|o| o.contains(&t.id)))
        .collect()
}

fn variant_text(name: &str) -> String {
    let path = if name == "current" { root().join("skills/report/SKILL.md") } else { root().join(format!("evals/report/variants/{name}.md")) };
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn text_of(m: &Message) -> String {
    match m {
        Message::Assistant(a) => a.content.iter().filter_map(|c| if let AssistantContent::Text { text, .. } = c { Some(text.as_str()) } else { None }).collect::<Vec<_>>().join(""),
        _ => String::new(),
    }
}

/// What one agent run left: its messages, and how it ended.
struct Ran {
    /// The whole conversation so far, as its session file has it.
    messages: Vec<Message>,
    error: Option<String>,
    secs: f64,
}

/// One serpis node per variant, in this process, as the app's own node runs chats: its agent
/// directory has the variant as the user's `report` skill, its config is serpis's default one.
struct Node {
    socket: PathBuf,
    _running: serpis_node::server::Running,
}

/// What the app gives its node (remote.rs, on connecting to this machine's node): every setting
/// at its default and the tools the window lends (serpis_lua, serpis_api and the default
/// config's). Read from a headless editor booted on this run's HOME, so the config is the
/// default one written there, never yours.
struct App {
    vars: serde_json::Map<String, Value>,
    tools: Vec<serpis_proto::wire::ToolSpec>,
}

fn boot_app(rt: &tokio::runtime::Runtime) -> App {
    let mut ed = serpis::editor::Editor::new(egui::Context::default(), rt.handle().clone());
    serpis::bootstrap(&mut ed);
    let vars = ed.vars.iter().map(|(k, v)| (k.clone(), v.value.clone())).collect();
    App { vars, tools: serpis::remote::lent_specs(&ed) }
}

/// The OpenRouter key reaches the nodes as an `auth.json` command (`!cat FILE`): serpis runs it
/// once and keeps the answer, so the file is gone before any agent starts, the key is in no
/// agent's environment and in no file of the run.
fn prime_key(key: &str) -> String {
    let dir = std::env::temp_dir().join(format!("report-eval-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("key");
    {
        use std::os::unix::fs::OpenOptionsExt;
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&file).unwrap();
        f.write_all(key.as_bytes()).unwrap();
    }
    let cmd = format!("!cat '{}'", file.display());
    assert_eq!(serpis_ai::registry::resolve_config_value(&cmd).as_deref(), Some(key), "the key command");
    std::fs::remove_file(&file).unwrap();
    std::fs::remove_dir(&dir).unwrap();
    cmd
}

fn start_node(out: &Path, variant: &str, skill: &str, (key_cmd, key_len): (&str, usize), provider: &str, model: &str, rt: &tokio::runtime::Runtime) -> Node {
    let dir = out.join("nodes").join(variant);
    let agent = dir.join("agent");
    std::fs::create_dir_all(agent.join("skills/report")).unwrap();
    std::fs::write(agent.join("skills/report/SKILL.md"), skill).unwrap();
    std::fs::write(agent.join("auth.json"), json!({provider: {"type": "api_key", "key": key_cmd}}).to_string()).unwrap();
    std::fs::write(dir.join("settings.json"), json!({"session-directory": dir.join("sessions")}).to_string()).unwrap();
    // The model catalog, as serpis fetches it (the public one, not your copy).
    let reg = Registry::new(Paths { agent_dir: agent.clone() });
    rt.block_on(reg.refresh_catalogs(false));
    let (p, id) = model.split_once('/').unwrap();
    assert!(reg.find(p, id).is_some(), "no model {model} in serpis's catalog");
    assert_eq!(reg.api_key(provider).as_deref().map(str::len), Some(key_len), "the node's key");
    // Unix socket paths are limited to about 100 bytes.
    let socket = std::env::temp_dir().join(format!("re-{}-{variant}", std::process::id())).join("node.sock");
    let config_dir = Some(out.join("home/.config/serpis"));
    let running = serpis_node::server::start(serpis_node::server::Config { socket: socket.clone(), dir, agent_dir: Some(agent), config_dir }, rt.handle().clone()).unwrap_or_else(|e| panic!("start a node at {}: {e:#}", socket.display()));
    Node { socket, _running: running }
}

/// A chat on a node, driven as the app drives it.
struct Drive {
    client: serpis_node::client::Client,
    events: tokio::sync::mpsc::UnboundedReceiver<serpis_proto::wire::Event>,
    chat: String,
    session: Option<String>,
    /// Calls of the lent tools (which only the app can run): answered with an error.
    lent_calls: Vec<String>,
}

impl Drive {
    async fn open(cx: &Ctx, node: &Node, repo: &Path) -> anyhow::Result<Drive> {
        use serpis_proto::wire::{Request, Response};
        let (client, events) = serpis_node::client::Client::connect(&serpis_node::client::Target::Local(node.socket.clone()), "report-eval").await?;
        client.call(Request::Lend { tools: cx.app.tools.clone() }).await?;
        client.call(Request::SetVars { vars: cx.app.vars.clone() }).await?;
        let r = client.call(Request::New { cwd: repo.to_string_lossy().into(), chat: None, model: Some(cx.model.clone()), thinking: Some(cx.thinking.clone()) }).await?;
        let Response::Chat { chat } = r else { anyhow::bail!("not a chat: {r:?}") };
        client.call(Request::Attach { chat: chat.clone() }).await?;
        Ok(Drive { client, events, chat, session: None, lent_calls: Vec::new() })
    }

    /// Send `text` and wait until the run it starts is over.
    async fn prompt(&mut self, text: String, timeout: Duration) -> Option<String> {
        use serpis_proto::wire::{Event, Request};
        if let Err(e) = self.client.call(Request::Prompt { chat: self.chat.clone(), text, images: vec![], follow_up: false, request_id: None }).await {
            return Some(format!("prompt: {e}"));
        }
        let (mut started, mut error) = (false, None);
        let wait = async {
            while let Some(e) = self.events.recv().await {
                match e {
                    Event::State { chat, state } if chat == self.chat => {
                        self.session = state.session.clone().or(self.session.take());
                        error = state.last_error.clone();
                        if state.running {
                            started = true;
                        } else if started {
                            return true;
                        }
                    }
                    Event::ToolCall { call, name, .. } => {
                        self.lent_calls.push(name.clone());
                        tokio::spawn(self.client.call(Request::ToolDone { call, text: format!("{name} is the serpis app's, and isn't available here."), is_error: true }));
                    }
                    _ => {}
                }
            }
            false
        };
        match tokio::time::timeout(timeout, wait).await {
            Ok(true) => error,
            Ok(false) => Some("the node closed the stream".into()),
            Err(_) => {
                let _ = self.client.call(Request::Abort { chat: self.chat.clone() }).await;
                Some(format!("timed out after {} min", timeout.as_secs() / 60))
            }
        }
    }

    fn loaded(&self) -> Option<serpis_chat::session::Loaded> {
        serpis_chat::session::load(Path::new(self.session.as_ref()?)).ok()
    }
}

async fn step(d: &mut Drive, text: String, timeout: Duration) -> Ran {
    let start = Instant::now();
    let error = d.prompt(text, timeout).await;
    // The session file is written as the run goes; give the last write a moment.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let messages = d.loaded().map(|l| l.messages).unwrap_or_default();
    Ran { messages, error, secs: start.elapsed().as_secs_f64() }
}

/// The quote and label of the `n`th (from 1) top-level item under `title`, as a comment on it
/// has them.
fn item(report: &str, title: &str, n: usize) -> Option<(String, String)> {
    let (_, body) = sections_of(report).into_iter().find(|(t, _)| t.eq_ignore_ascii_case(title))?;
    let line = body.lines().filter(|l| !l.starts_with(' ') && is_item(l)).nth(n - 1)?;
    let text = line.trim_start().trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == '-' || c == '*' || c == ')').trim();
    let text = text.replace("**", "").replace('`', "");
    Some((text, format!("{title} {n}")))
}

/// The review serpis sends for comments on `report` (chat_review::prompt, with its default
/// settings): a change asked on the first item under Needs you (else Changed), and a question
/// on the first under Changed not asked about already (else on the first line).
fn review_of(report: &str, r: &Value) -> String {
    let opening = chat_review::opening_of(report);
    let s = |k: &str| r[k].as_str().unwrap_or_default().to_string();
    let mut comments = Vec::new();
    let mut asked = 1;
    if let Some((quote, label)) = item(report, "Needs you", 1) {
        comments.push(Comment { quote, label, opening: opening.clone(), text: s("fix_needs_you"), ask: Ask::Fix, ..Default::default() });
    } else if let Some((quote, label)) = item(report, "Changed", 1) {
        comments.push(Comment { quote, label, opening: opening.clone(), text: s("fix"), ask: Ask::Fix, ..Default::default() });
        asked = 2;
    }
    let first_line = || (report.lines().next().unwrap_or("").replace("**", ""), String::new());
    let (quote, label) = item(report, "Changed", asked).or_else(|| item(report, "Changed", 1).filter(|_| asked == 1)).unwrap_or_else(first_line);
    comments.push(Comment { quote, label, opening, text: s("question"), ask: Ask::Question, ..Default::default() });
    let framing = Framing { reply: chat_review::REPLY_INSTRUCTIONS.into(), question: chat_review::QUESTION_INSTRUCTIONS.into(), fix: chat_review::FIX_INSTRUCTIONS.into() };
    chat_review::prompt(&comments.iter().collect::<Vec<_>>(), "", &framing)
}

// ---- Scoring: what serpis makes of a report, and its shape.

fn is_item(l: &str) -> bool {
    let t = l.trim_start();
    t.starts_with("- ") || t.starts_with("* ") || t.split_once(". ").is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// The text with fenced code blocks taken out.
fn prose(text: &str) -> String {
    let mut fence = false;
    text.lines()
        .filter(|l| {
            if l.trim_start().starts_with("```") {
                fence = !fence;
                return false;
            }
            !fence
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Its `##` sections (title, body), whatever its first line says.
fn sections_of(text: &str) -> Vec<(String, String)> {
    let p = prose(text);
    let mut out: Vec<(String, String)> = Vec::new();
    for l in p.lines() {
        if let Some(t) = l.strip_prefix("## ") {
            out.push((t.trim().to_string(), String::new()));
        } else if let Some((_, b)) = out.last_mut() {
            b.push_str(l);
            b.push('\n');
        }
    }
    out
}

fn words(s: &str) -> usize {
    s.split_whitespace().count()
}

/// Lines that act as headings but aren't the skill's: `#`, `###` and deeper, and a line that is
/// all bold ("**Why it fires**", "**Cause:**").
fn pseudo_headings(text: &str) -> Vec<String> {
    prose(text)
        .lines()
        .map(str::trim)
        .filter(|l| {
            let hashes = l.chars().take_while(|c| *c == '#').count();
            let heading = hashes > 0 && hashes != 2 && l[hashes..].starts_with(' ');
            let bold = l.len() > 4 && l.starts_with("**") && (l.ends_with("**") || l.ends_with("**:")) && l.matches("**").count() == 2;
            heading || bold
        })
        .map(str::to_string)
        .collect()
}

fn normalize(s: &str) -> String {
    s.replace(['*', '`', '>'], "").split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// A report's score. `question`: the quote a review asked about (a reply is scored on whether it
/// quotes it, as serpis needs to link the answer).
fn score(text: &str, question: Option<&str>) -> Value {
    let shaped = shaped_report(text);
    let sections = sections_of(text);
    let titles: Vec<String> = sections.iter().map(|(t, _)| t.clone()).collect();
    let known = |t: &str| SECTIONS.iter().position(|s| s.eq_ignore_ascii_case(t));
    let invented: Vec<&String> = titles.iter().filter(|t| known(t).is_none()).collect();
    let order: Vec<usize> = titles.iter().filter_map(|t| known(t)).collect();
    let in_order = order.windows(2).all(|w| w[0] < w[1]);
    let needs_at = titles.iter().position(|t| t.eq_ignore_ascii_case("needs you"));
    let body = |title: &str| sections.iter().find(|(t, _)| t.eq_ignore_ascii_case(title)).map(|(_, b)| b.clone());
    let needs = body("Needs you").unwrap_or_default();
    // Words per top-level Needs you item.
    let mut items: Vec<usize> = Vec::new();
    for l in needs.lines() {
        if !l.starts_with(' ') && is_item(l) {
            items.push(0);
        }
        if let Some(n) = items.last_mut() {
            *n += words(l);
        }
    }
    let checked = body("Checked").unwrap_or_default();
    let checks: Vec<&str> = checked.lines().filter(|l| !l.starts_with(' ') && is_item(l)).collect();
    let marked = checks.iter().filter(|l| check_mark(l.trim_start().trim_start_matches(['-', '*']).trim().replace("**", "").as_str()).is_some()).count();
    let not_checked = checks.iter().any(|l| normalize(l).trim_start_matches(['-', ' ']).starts_with("not checked"));
    let pseudo = pseudo_headings(text);
    let first_section = prose(text).find("\n## ").unwrap_or(text.len());
    let before = prose(text)[..first_section.min(prose(text).len())].to_string();
    let intro_words = words(&before).saturating_sub(words(before.lines().next().unwrap_or("")));
    let parsed = shaped.is_some();
    let clean = parsed && invented.is_empty() && pseudo.is_empty() && in_order && needs_at.is_none_or(|i| i == 0);
    let mut v = json!({
        "parsed": parsed,
        "outcome": shaped.as_ref().map(|s| s.outcome.badge().0),
        "first_line": text.trim_start().lines().next().unwrap_or(""),
        "clean": clean,
        "sections": titles,
        "invented_sections": invented,
        "pseudo_headings": pseudo,
        "in_order": in_order,
        "needs_you_first": needs_at.map(|i| i == 0),
        "needs_you_items": shaped.as_ref().map(|s| s.needs_you()).unwrap_or(items.len()),
        "needs_you_max_words": items.iter().max(),
        "checks": checks.len(),
        "checks_marked": marked,
        "not_checked": not_checked,
        "words": words(&prose(text)),
        "intro_words": intro_words,
        "top_items": top_items(&needs),
    });
    if let Some(q) = question {
        let first = normalize(q.lines().next().unwrap_or(""));
        let first: String = first.chars().take(60).collect();
        v["answered"] = json!(!first.is_empty() && normalize(text).contains(&first));
        // The places the review named ("Needs you 1", "item 1 under …"), copied into the reply.
        let body = normalize(&prose(text));
        v["label_reuse"] = json!(["needs you 1", "changed 1", "item 1 under", "needs you item"].iter().any(|l| body.contains(l)));
    }
    v
}

// ---- Runs.

struct Job {
    task: Task,
    variant: String,
    i: usize,
}

struct Ctx {
    out: PathBuf,
    commit: String,
    /// `provider/id`, as a chat names it.
    model: String,
    thinking: String,
    timeout: Duration,
    app: App,
    nodes: BTreeMap<String, Node>,
}

fn cost(ms: &[Message]) -> f64 {
    ms.iter().map(|m| if let Message::Assistant(a) = m { a.usage.cost.total } else { 0.0 }).sum()
}

fn upstreams(ms: &[Message]) -> Vec<String> {
    let mut u: Vec<String> = ms.iter().filter_map(|m| if let Message::Assistant(a) = m { a.upstream.clone() } else { None }).collect();
    u.sort();
    u.dedup();
    u
}

async fn run_job(cx: Arc<Ctx>, job: Job) -> Vec<Value> {
    let dir = cx.out.join("runs").join(&job.task.id).join(&job.variant).join(job.i.to_string());
    let repo = dir.join("repo");
    std::fs::create_dir_all(dir.join("home")).unwrap();
    {
        let (c, r) = (cx.commit.clone(), repo.clone());
        tokio::task::spawn_blocking(move || clone(&c, &r)).await.unwrap();
    }
    if let Some(setup) = &job.task.setup {
        let o = Command::new("sh").arg("-c").arg(setup).current_dir(&repo).output().expect("sh");
        assert!(o.status.success(), "setup: {}", String::from_utf8_lossy(&o.stderr));
    }
    let base = json!({"task": job.task.id, "variant": job.variant, "i": job.i, "dir": dir});
    let mut d = match Drive::open(&cx, &cx.nodes[&job.variant], &repo).await {
        Ok(d) => d,
        Err(e) => {
            let mut rec = base;
            rec["step"] = json!(job.task.id);
            rec["error"] = json!(format!("{e:#}"));
            rec["score"] = Value::Null;
            return vec![rec];
        }
    };
    let mut out = Vec::new();
    let first = step(&mut d, job.task.prompt.clone(), cx.timeout).await;
    if let Some(l) = d.loaded()
        && let Some((_, p)) = l.entries.iter().rev().find(|(t, _)| t == serpis_chat::SYSTEM_PROMPT_ENTRY)
    {
        std::fs::write(dir.join("system-prompt.md"), p["prompt"].as_str().unwrap_or_default()).unwrap();
        std::fs::write(dir.join("tools.json"), serde_json::to_string_pretty(&p["tools"]).unwrap()).unwrap();
    }
    let report = first.messages.iter().rev().find(|m| matches!(m, Message::Assistant(_))).map(text_of).unwrap_or_default();
    std::fs::write(dir.join("report.md"), &report).unwrap();
    std::fs::write(dir.join("transcript.json"), serde_json::to_string_pretty(&first.messages).unwrap()).unwrap();
    let ok = first.error.is_none() && !report.trim().is_empty();
    let mut rec = base.clone();
    rec["step"] = json!(job.task.id);
    rec["error"] = json!(first.error);
    rec["secs"] = json!(first.secs.round());
    rec["cost"] = json!(cost(&first.messages));
    rec["turns"] = json!(first.messages.iter().filter(|m| matches!(m, Message::Assistant(_))).count());
    rec["upstreams"] = json!(upstreams(&first.messages));
    rec["lent_calls"] = json!(d.lent_calls);
    rec["score"] = if ok { score(&report, None) } else { Value::Null };
    out.push(rec);
    if let (true, Some(r)) = (ok, &job.task.review) {
        let review = review_of(&report, r);
        std::fs::write(dir.join("review.md"), &review).unwrap();
        let question = chat_review::parse_sent(&review).and_then(|s| s.items.into_iter().find(|i| i.ask == Ask::Question)).map(|i| i.quote.join("\n"));
        let seen = first.messages.len();
        let mut second = step(&mut d, review, cx.timeout).await;
        second.messages.drain(..seen.min(second.messages.len()));
        let reply = second.messages.iter().rev().find(|m| matches!(m, Message::Assistant(_))).map(text_of).unwrap_or_default();
        std::fs::write(dir.join("reply.md"), &reply).unwrap();
        std::fs::write(dir.join("transcript-reply.json"), serde_json::to_string_pretty(&second.messages).unwrap()).unwrap();
        let ok = second.error.is_none() && !reply.trim().is_empty();
        let mut rec = base;
        rec["step"] = json!(format!("{}+review", job.task.id));
        rec["error"] = json!(second.error);
        rec["secs"] = json!(second.secs.round());
        rec["cost"] = json!(cost(&second.messages));
        rec["turns"] = json!(second.messages.iter().filter(|m| matches!(m, Message::Assistant(_))).count());
        rec["upstreams"] = json!(upstreams(&second.messages));
        rec["question"] = json!(question);
        rec["score"] = if ok { score(&reply, question.as_deref()) } else { Value::Null };
        out.push(rec);
    }
    let _ = d.client.call(serpis_proto::wire::Request::Detach { chat: d.chat.clone(), close: true }).await;
    for r in &out {
        std::fs::write(dir.join(format!("score-{}.json", r["step"].as_str().unwrap().replace('+', "-"))), serde_json::to_string_pretty(r).unwrap()).unwrap();
    }
    out
}

// ---- Summary.

/// Two-sided Fisher exact test on [[a, b], [c, d]].
fn fisher(a: usize, b: usize, c: usize, d: usize) -> f64 {
    let lf = |n: usize| (1..=n).map(|k| (k as f64).ln()).sum::<f64>();
    let (r1, r2, c1, n) = (a + b, c + d, a + c, a + b + c + d);
    let p = |x: usize| (lf(r1) + lf(r2) + lf(c1) + lf(n - c1) - lf(n) - lf(x) - lf(r1 - x) - lf(c1 - x) - lf(r2 + x - c1)).exp();
    let lo = c1.saturating_sub(r2);
    let hi = r1.min(c1);
    let observed = p(a);
    (lo..=hi).map(p).filter(|q| *q <= observed * (1.0 + 1e-9)).sum::<f64>().min(1.0)
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Some(if v.len() % 2 == 1 { v[v.len() / 2] } else { (v[v.len() / 2 - 1] + v[v.len() / 2]) / 2.0 })
}

fn summary(records: &[Value], variants: &[String]) -> String {
    let mut steps: Vec<String> = records.iter().map(|r| r["step"].as_str().unwrap().to_string()).collect();
    steps.sort();
    steps.dedup();
    let rate = |rs: &[&Value], f: &dyn Fn(&Value) -> bool| -> (usize, usize) { (rs.iter().filter(|r| f(&r["score"])).count(), rs.len()) };
    let pct = |(k, n): (usize, usize)| if n == 0 { "–".to_string() } else { format!("{k}/{n}") };
    let med = |rs: &[&Value], k: &str| median(rs.iter().filter_map(|r| r["score"][k].as_f64()).collect()).map_or("–".into(), |m| format!("{m:.0}"));
    let mut s = String::from("| step | variant | ran | parsed | clean | invented sections | bold/### headings | Needs you first | words | words before sections | biggest Needs you item | Not checked | answered | label reused | cost |\n|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n");
    let mut pooled: BTreeMap<String, (usize, usize, usize, usize)> = BTreeMap::new();
    let of = |step: Option<&str>, v: &str| -> Vec<&Value> { records.iter().filter(|r| r["variant"] == v && step.is_none_or(|s| r["step"] == s)).collect() };
    for step in steps.iter().map(Some).chain([None]) {
        for v in variants {
            let all = of(step.map(|s| s.as_str()), v);
            let rs: Vec<&Value> = all.iter().copied().filter(|r| !r["score"].is_null()).collect();
            let reply = step.is_some_and(|s| s.ends_with("+review"));
            let parsed = rate(&rs, &|s| s["parsed"] == true);
            let clean = rate(&rs, &|s| s["clean"] == true);
            if step.is_none() {
                pooled.insert(v.clone(), (parsed.0, parsed.1, clean.0, clean.1));
            }
            s.push_str(&format!(
                "| {} | {v} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | ${:.3} |\n",
                step.map_or("**all**", |s| s.as_str()),
                pct((rs.len(), all.len())),
                pct(parsed),
                pct(clean),
                pct(rate(&rs, &|s| s["invented_sections"].as_array().is_some_and(|a| !a.is_empty()))),
                pct(rate(&rs, &|s| s["pseudo_headings"].as_array().is_some_and(|a| !a.is_empty()))),
                pct(rate(&rs.iter().copied().filter(|r| !r["score"]["needs_you_first"].is_null()).collect::<Vec<_>>(), &|s| s["needs_you_first"] == true)),
                med(&rs, "words"),
                med(&rs, "intro_words"),
                med(&rs, "needs_you_max_words"),
                pct(rate(&rs, &|s| s["not_checked"] == true)),
                if reply { pct(rate(&rs, &|s| s["answered"] == true)) } else { "–".into() },
                if reply { pct(rate(&rs, &|s| s["label_reuse"] == true)) } else { "–".into() },
                all.iter().filter_map(|r| r["cost"].as_f64()).sum::<f64>(),
            ));
        }
    }
    if variants.len() >= 2 {
        let (a, b) = (&variants[0], &variants[1]);
        if let (Some(x), Some(y)) = (pooled.get(a), pooled.get(b)) {
            s.push_str(&format!(
                "\nAll steps, {a} vs {b} (two-sided Fisher exact): parsed {}/{} vs {}/{}, p = {:.3}; clean {}/{} vs {}/{}, p = {:.3}.\n",
                x.0, x.1, y.0, y.1, fisher(x.0, x.1 - x.0, y.0, y.1 - y.0), x.2, x.3, y.2, y.3, fisher(x.2, x.3 - x.2, y.2, y.3 - y.2)
            ));
        }
    }
    s.push_str("\nparsed: serpis reads it as a report (it opens with Done, Partly done or Blocked). clean: parsed, only the skill's sections in its order, Needs you first, no bold or ### headings of its own. Words: medians.\n");
    s
}

/// Score a finished run again from the reports it saved.
fn rescore(dir: &Path) -> (Vec<Value>, Vec<String>) {
    let meta: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("meta.json")).unwrap()).unwrap();
    let variants: Vec<String> = meta["variants"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
    let mut records = Vec::new();
    for l in std::fs::read_to_string(dir.join("results.jsonl")).unwrap().lines() {
        let mut r: Value = serde_json::from_str(l).unwrap();
        if r["score"].is_null() {
            records.push(r);
            continue;
        }
        let run = PathBuf::from(r["dir"].as_str().unwrap());
        let reply = r["step"].as_str().unwrap().ends_with("+review");
        let text = std::fs::read_to_string(run.join(if reply { "reply.md" } else { "report.md" })).unwrap();
        r["score"] = score(&text, r["question"].as_str());
        records.push(r);
    }
    (records, variants)
}

fn main() {
    let a = args();
    if let Some(dir) = &a.score {
        let (records, variants) = rescore(dir);
        let s = summary(&records, &variants);
        std::fs::write(dir.join("summary.md"), &s).unwrap();
        println!("{s}");
        return;
    }
    let root = root();
    let commit = git(&root, &["rev-parse", &a.commit]);
    let stamp = chrono_stamp();
    let out = a.out.clone().unwrap_or_else(|| root.join("target/report-eval").join(&stamp));
    std::fs::create_dir_all(out.join("variants")).unwrap();
    let out = out.canonicalize().unwrap();

    // The key, from serpis's own settings (OPENROUTER_API_KEY wins), before HOME changes below.
    let reg = Registry::new(Paths::default_paths());
    let key = std::env::var("OPENROUTER_API_KEY").ok().filter(|k| !k.is_empty() && a.provider == "openrouter").or_else(|| reg.api_key(&a.provider)).expect("no API key for the provider in serpis's settings");
    drop(reg);

    // What the agent's commands see: a HOME of their own, no keys, one shared cargo target (so
    // dependencies build once), your cargo and rustup.
    let real_home = dirs::home_dir().unwrap();
    let cargo_home = std::env::var("CARGO_HOME").unwrap_or_else(|_| real_home.join(".cargo").to_string_lossy().into());
    let rustup_home = std::env::var("RUSTUP_HOME").unwrap_or_else(|_| real_home.join(".rustup").to_string_lossy().into());
    std::fs::create_dir_all(out.join("home")).unwrap();
    // SAFETY: no other thread runs yet (the runtime starts below).
    unsafe {
        for (k, _) in std::env::vars() {
            if k.ends_with("_API_KEY") || k.ends_with("_TOKEN") || k.starts_with("SERPIS_") {
                std::env::remove_var(k);
            }
        }
        std::env::set_var("HOME", out.join("home"));
        std::env::set_var("CARGO_HOME", cargo_home);
        std::env::set_var("RUSTUP_HOME", rustup_home);
        std::env::set_var("CARGO_TARGET_DIR", root.join("target/report-eval/cargo-target"));
    }

    let mut skills = BTreeMap::new();
    for v in &a.variants {
        let t = variant_text(v);
        std::fs::write(out.join("variants").join(format!("{v}.md")), &t).unwrap();
        skills.insert(v.clone(), t);
    }
    let ts = tasks(&a.tasks);
    std::fs::write(
        out.join("meta.json"),
        serde_json::to_string_pretty(&json!({
            "commit": commit, "model": format!("{}/{}", a.provider, a.model), "thinking": a.thinking, "n": a.n,
            "variants": a.variants, "tasks": ts.iter().map(|t| &t.id).collect::<Vec<_>>(), "started": stamp,
        }))
        .unwrap(),
    )
    .unwrap();
    println!("report eval: {} task(s) × {} variant(s) × {} on {}/{} ({:?}), commit {}, in {}", ts.len(), a.variants.len(), a.n, a.provider, a.model, a.thinking, &commit[..10], out.display());

    if a.prewarm {
        println!("building dependencies once (shared target)…");
        let dir = out.join("prewarm");
        clone(&commit, &dir);
        let st = Command::new("cargo").args(["test", "--no-run", "-q", "-p", "serpis-agent", "-p", "serpis"]).current_dir(&dir).status().expect("cargo");
        if !st.success() {
            eprintln!("prewarm build failed; runs will build what they need");
        }
    }

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    // The app's settings and lent tools, from a headless editor on this run's HOME (which writes
    // the default config there), then a node per variant, as the app's own.
    let app = boot_app(&rt);
    std::fs::write(out.join("app.json"), serde_json::to_string_pretty(&json!({"vars": app.vars, "tools": app.tools})).unwrap()).unwrap();
    let key_cmd = prime_key(&key);
    let model = format!("{}/{}", a.provider, a.model);
    let nodes = skills.iter().map(|(v, t)| (v.clone(), start_node(&out, v, t, (&key_cmd, key.len()), &a.provider, &model, &rt))).collect();
    drop(key);
    let thinking = serde_json::to_value(a.thinking).unwrap().as_str().unwrap().to_string();
    let cx = Arc::new(Ctx { out: out.clone(), commit, model, thinking, timeout: a.timeout, app, nodes });
    let records = rt.block_on(async {
        let sem = Arc::new(tokio::sync::Semaphore::new(a.jobs));
        let mut set = tokio::task::JoinSet::new();
        // Samples outermost, so a run stopped early still has every variant.
        for i in 0..a.n {
            for t in &ts {
                for v in &a.variants {
                    let (cx, sem, job) = (cx.clone(), sem.clone(), Job { task: t.clone(), variant: v.clone(), i });
                    set.spawn(async move {
                        let _p = sem.acquire().await.unwrap();
                        run_job(cx, job).await
                    });
                }
            }
        }
        let mut records = Vec::new();
        let results = out.join("results.jsonl");
        while let Some(r) = set.join_next().await {
            for rec in r.unwrap_or_else(|e| vec![json!({"error": format!("{e}")})]) {
                let line = serde_json::to_string(&rec).unwrap();
                use std::io::Write;
                std::fs::OpenOptions::new().create(true).append(true).open(&results).unwrap().write_all(format!("{line}\n").as_bytes()).unwrap();
                let sc = &rec["score"];
                println!(
                    "{} {} #{}: {} ({}s, ${:.4})",
                    rec["step"].as_str().unwrap_or("?"),
                    rec["variant"].as_str().unwrap_or("?"),
                    rec["i"],
                    if sc.is_null() { format!("no report: {}", rec["error"]) } else { format!("parsed={} clean={} words={}", sc["parsed"], sc["clean"], sc["words"]) },
                    rec["secs"],
                    rec["cost"].as_f64().unwrap_or(0.0)
                );
                records.push(rec);
            }
        }
        records
    });
    let s = summary(&records, &a.variants);
    std::fs::write(out.join("summary.md"), &s).unwrap();
    println!("\n{s}\nRuns, reports and transcripts: {}", out.display());
}

fn chrono_stamp() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let o = Command::new("date").args(["-u", "-r", &secs.to_string(), "+%Y%m%d-%H%M%S"]).output().ok();
    o.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| secs.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "Done: the cache key ignored the locale, so French users got English pages.\n\n## Needs you\n1. **Backfill?** I recommend purging the cache once. Old entries never expire. Waiting a day also works but serves wrong pages meanwhile.\n\n## Changed\n- **Locale in the key** (`cache.rs:40`): pages differ by locale.\n\n## Checked\n- **`cargo test`:** passes (88 tests).\n- **Not checked:** the CDN's own cache.\n";
    const BAD: &str = "Here is what I found: the cache key ignores the locale.\n\n**Why it happens**\n\n- `cache.rs:40` builds the key from the path only.\n\nConfidence: high.\n\n### Details\n\nMore prose here.\n\n## What I found\n\n- a\n\n## Needs you\n\n1. Purge? I'd purge.\n";

    #[test]
    fn scores_as_serpis_reads_it() {
        let g = score(GOOD, None);
        assert_eq!((g["parsed"].clone(), g["clean"].clone(), g["needs_you_first"].clone(), g["not_checked"].clone()), (json!(true), json!(true), json!(true), json!(true)));
        assert_eq!((g["checks"].clone(), g["checks_marked"].clone(), g["needs_you_items"].clone()), (json!(2), json!(2), json!(1)));
        assert_eq!(g["intro_words"], json!(0));
        let b = score(BAD, None);
        assert_eq!((b["parsed"].clone(), b["clean"].clone(), b["needs_you_first"].clone()), (json!(false), json!(false), json!(false)));
        assert_eq!(b["invented_sections"], json!(["What I found"]));
        assert_eq!(b["pseudo_headings"], json!(["**Why it happens**", "### Details"]));
        assert!(b["intro_words"].as_u64().unwrap() > 10);
    }

    #[test]
    fn a_review_is_serpis_own_and_its_reply_is_scored() {
        let review = review_of(GOOD, &json!({"fix_needs_you": "Go with what you recommend.", "fix": "x", "question": "Why did you do it this way?"}));
        assert!(review.starts_with("Review comments on what you wrote.\n\nReply with a report"), "{review}");
        let sent = chat_review::parse_sent(&review).unwrap();
        assert_eq!(sent.items.iter().map(|i| (i.ask, i.place.as_str())).collect::<Vec<_>>(), vec![(Ask::Question, "Changed 1"), (Ask::Fix, "Needs you 1")]);
        assert_eq!(sent.items[0].quote, vec!["Locale in the key (cache.rs:40): pages differ by locale."]);
        let q = "Locale in the key (cache.rs:40): pages differ by locale.";
        let reply = "Done: purged the cache.\n\n> Locale in the key (cache.rs:40): pages differ by locale.\n\nBecause the pages differ.\n\n## Changed\n- **Purged** once.\n";
        assert_eq!((score(reply, Some(q))["answered"].clone(), score(reply, Some(q))["label_reuse"].clone()), (json!(true), json!(false)));
        let bad = "Done: did it.\n\n## What I did for each item\n1. **Needs you 1 — purge:** done.\n";
        assert_eq!((score(bad, Some(q))["answered"].clone(), score(bad, Some(q))["label_reuse"].clone()), (json!(false), json!(true)));
    }

    #[test]
    fn fisher_matches_known_values() {
        // The tea-tasting table: p = 0.486.
        assert!((fisher(3, 1, 1, 3) - 0.4857).abs() < 1e-3);
        assert!((fisher(10, 0, 0, 10) - 1.0825e-5).abs() < 1e-7);
        assert!((fisher(5, 5, 5, 5) - 1.0).abs() < 1e-9);
    }
}
