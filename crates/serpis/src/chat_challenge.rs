//! Challenging an agent's report: a fresh agent, in a worktree of its own (the chat's commits
//! with its uncommitted changes copied in), tries to prove the report's claims wrong. Its own
//! report lists what it found in the `challenge` skill's shape (skills/challenge); each finding
//! goes to the challenged chat's review as a draft under the claim it disputes, to send with
//! the rest. The worktree is removed a while after the challenge ends
//! (chat-report-challenge-worktree-ttl), unless kept or its chat is still open.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::buffers::chat::ChatBuffer;
use crate::buffer::BufferId;
use crate::editor::Editor;

/// A challenge, from its start until its worktree is removed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Run {
    /// Short and unique: names the worktree's branch (`challenge-<id>`).
    pub id: String,
    /// The challenged chat (its id on this machine) and its report's entry.
    pub chat: String,
    pub entry: usize,
    /// How the report starts, to say which one it was.
    pub opening: String,
    /// The challenger's chat.
    pub challenger: String,
    pub model: Option<String>,
    pub repo: PathBuf,
    pub worktree: PathBuf,
    pub branch: String,
    /// Unix seconds.
    pub started: u64,
    pub ended: Option<u64>,
    /// What came of it: findings placed on the report, claims that held, or why it failed.
    pub findings: usize,
    pub held: Vec<String>,
    pub error: Option<String>,
    /// Kept: its worktree isn't removed by itself.
    pub keep: bool,
}

/// The challenges this window started (kept in `<config>/challenges.json`, so a worktree is
/// still removed after a restart).
#[derive(Default)]
pub struct Challenges {
    pub runs: Vec<Run>,
    loaded: bool,
    checked: Option<Instant>,
}

/// What the person chose for a challenge: the challenger's model and thinking, and what else
/// it should know or do.
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub model: Option<String>,
    pub thinking: Option<String>,
    pub note: String,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn store_path() -> PathBuf {
    crate::config_dir().join("challenges.json")
}

fn load(ed: &mut Editor) {
    if ed.challenges.loaded {
        return;
    }
    ed.challenges.loaded = true;
    ed.challenges.runs = std::fs::read_to_string(store_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
}

fn save(ed: &Editor) {
    let path = store_path();
    if ed.challenges.runs.is_empty() {
        let _ = std::fs::remove_file(&path);
        return;
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_vec_pretty(&ed.challenges.runs) {
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

/// The challenges of chat `chat` (its id), oldest first.
pub fn runs_of(ed: &mut Editor, chat: &str) -> Vec<Run> {
    load(ed);
    ed.challenges.runs.iter().filter(|r| r.chat == chat).cloned().collect()
}

/// A challenge as scripts see it.
pub fn to_json(ed: &Editor, r: &Run) -> Value {
    let ttl = ttl(ed);
    json!({
        "id": r.id,
        "entry": r.entry + 1,
        "challenger": r.challenger,
        "challenger_buffer": crate::remote::chat_buffer(ed, crate::remote::LOCAL, &r.challenger).map(crate::editor::buffer_id_to_u64),
        "model": r.model,
        "worktree": r.worktree.to_string_lossy(),
        "branch": r.branch,
        "running": r.ended.is_none(),
        "findings": r.findings,
        "held": r.held,
        "error": r.error,
        "keep": r.keep,
        "removed_in": r.ended.filter(|_| !r.keep && ttl > 0).map(|e| (e + ttl).saturating_sub(now())),
    })
}

fn ttl(ed: &Editor) -> u64 {
    ed.var("chat-report-challenge-worktree-ttl").and_then(Value::as_u64).unwrap_or(1800)
}

// ---------------------------------------------------------------------------------------------
// Starting one
// ---------------------------------------------------------------------------------------------

/// Challenge the report at `entry` of chat buffer `buf` (None: its latest report).
pub fn start(ed: &mut Editor, buf: BufferId, entry: Option<usize>, opts: Options) -> anyhow::Result<()> {
    load(ed);
    if !ed.var_bool("chat-report-challenge") {
        anyhow::bail!("Challenges are off (chat-report-challenge)");
    }
    let (report, chat, cwd, local, model) = ed
        .with_buffer_as::<ChatBuffer, _>(buf, |c, ed| {
            let report = match entry {
                Some(e) => c.report_text(e).map(|t| (e, t)),
                None => c.reports(ed).pop(),
            };
            (report, c.chat_id(), c.state_now().cwd, c.node.local(), c.state_now().model)
        })
        .ok_or_else(|| anyhow::anyhow!("Not a chat buffer"))?;
    let (entry, report) = report.ok_or_else(|| anyhow::anyhow!("No report to challenge here"))?;
    if !local {
        anyhow::bail!("Challenging a chat on another machine isn't supported yet");
    }
    if let Some(r) = ed.challenges.runs.iter().find(|r| r.chat == chat && r.entry == entry && r.ended.is_none()) {
        anyhow::bail!("This report is being challenged already (challenge {})", r.id);
    }
    let cwd = PathBuf::from(cwd);
    let repo = ed.repo_of(&cwd).ok_or_else(|| anyhow::anyhow!("{} is not in a git repository: a challenge needs a worktree", cwd.display()))?;
    let id = uuid::Uuid::new_v4().simple().to_string()[..6].to_string();
    let branch = format!("challenge-{id}");
    let model = opts.model.clone().or_else(|| ed.var_str("chat-report-challenge-model").filter(|m| !m.is_empty())).or(model);
    let thinking = opts.thinking.clone().or_else(|| ed.var_str("chat-report-challenge-thinking").filter(|t| !t.is_empty()));
    let run = Run {
        id: id.clone(),
        chat,
        entry,
        opening: crate::chat_review::opening_of(&report),
        model: model.clone(),
        repo: repo.main.clone(),
        branch: branch.clone(),
        started: now(),
        ..Default::default()
    };
    let note = opts.note.trim().to_string();
    let source = cwd.clone();
    crate::sidebar::create_worktree(ed, cwd, branch, None, move |ed, path| {
        let main = ed.main_sender();
        let dst = path.clone();
        ed.rt.spawn_blocking(move || {
            let copied = copy_changes(&source, &dst);
            let head = git_out(&source, &["log", "--oneline", "-1"]).unwrap_or_default();
            main.send(move |ed| {
                if let Err(e) = &copied {
                    ed.warn(format!("Challenge: couldn't copy the chat's uncommitted changes: {e:#}"));
                }
                let prompt = challenge_prompt(ed, &report, &source, &path, head.trim(), &note);
                launch(ed, run, path, model, thinking, prompt);
            });
        });
    });
    Ok(())
}

/// Start the challenger's chat in the worktree and send it the challenge.
fn launch(ed: &mut Editor, mut run: Run, worktree: PathBuf, model: Option<String>, thinking: Option<String>, prompt: String) {
    let mut params = json!({"cwd": worktree.to_string_lossy()});
    if let Some(m) = &model {
        params["model"] = json!(m);
    }
    let buf = match ed.create_buffer("chat", Some("*challenge*"), &params) {
        Ok(b) => b,
        Err(e) => {
            ed.error(format!("Challenge: {e:#}"));
            return;
        }
    };
    let title = format!("Challenge · {}", run.opening);
    let challenger = ed.with_buffer_as::<ChatBuffer, _>(buf, |c, ed| {
        if let Some(level) = thinking.as_deref().and_then(serpis_ai::ThinkingLevel::parse) {
            c.set_thinking(level);
        }
        c.submit(ed, prompt, false);
        c.set_session_name(&title);
        c.chat_id()
    });
    if let Some(m) = ed.meta_mut(buf) {
        m.name = title;
    }
    run.challenger = challenger.unwrap_or_default();
    run.worktree = worktree;
    ed.message(format!("Challenging the report with {}…", run.model.as_deref().unwrap_or("the default model")));
    ed.run_hook("chat-challenge-started", &json!({"id": run.id, "chat": run.chat, "worktree": run.worktree.to_string_lossy()}));
    ed.challenges.runs.push(run);
    save(ed);
}

/// What the challenger is sent: the skill by name (`/skill:challenge`), where it is, the
/// report, and what the person added.
fn challenge_prompt(ed: &Editor, report: &str, source: &Path, worktree: &Path, head: &str, note: &str) -> String {
    let skill = ed.var_str("chat-report-challenge-skill").filter(|s| !s.is_empty()).unwrap_or_else(|| "challenge".into());
    let run = if ed.var("chat-report-challenge-run").and_then(Value::as_bool).unwrap_or(true) {
        "You may run commands, the report's checks included."
    } else {
        "Don't run commands: read the code only, and tag every finding [plausible]."
    };
    let mut out = format!(
        "/skill:{skill} Challenge the report below, which another agent wrote after working in {}.\n\nYou are in a copy of that worktree, at {}: the same commit ({head}), with its uncommitted changes copied in (`git diff HEAD` shows them). {run}\n\n<report>\n{}\n</report>\n",
        source.display(),
        worktree.display(),
        report.trim()
    );
    if !note.is_empty() {
        out.push_str(&format!("\nFrom the person who asked for this challenge:\n\n{note}\n"));
    }
    out
}

fn git_out(dir: &Path, args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output()?;
    if !out.status.success() {
        anyhow::bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Copy the uncommitted changes of the checkout at `src` (staged or not, and untracked files)
/// into the worktree at `dst`, which is at the same commit.
pub fn copy_changes(src: &Path, dst: &Path) -> anyhow::Result<()> {
    let diff = Command::new("git").arg("-C").arg(src).args(["diff", "HEAD", "--binary"]).output()?;
    if !diff.status.success() {
        anyhow::bail!("git diff: {}", String::from_utf8_lossy(&diff.stderr).trim());
    }
    if !diff.stdout.is_empty() {
        let mut apply = Command::new("git").arg("-C").arg(dst).args(["apply", "--whitespace=nowarn", "-"]).stdin(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        std::io::Write::write_all(apply.stdin.as_mut().ok_or_else(|| anyhow::anyhow!("git apply: no stdin"))?, &diff.stdout)?;
        let out = apply.wait_with_output()?;
        if !out.status.success() {
            anyhow::bail!("git apply: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
    }
    let untracked = git_out(src, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    for rel in untracked.split('\0').filter(|p| !p.is_empty()) {
        let to = dst.join(rel);
        if let Some(dir) = to.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::copy(src.join(rel), &to)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Its findings
// ---------------------------------------------------------------------------------------------

/// A finding as the challenger wrote it (skills/challenge).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Found {
    pub confirmed: bool,
    /// The claim: `Changed 1`, `Checked 2`, `Headline`.
    pub label: String,
    /// The words of the report it disputes.
    pub quote: String,
    pub text: String,
    pub evidence: String,
}

/// The findings under the challenger's `## Needs you`, and the claims it says held (under
/// `## Checked`: `- **Changed 2:** held.`).
pub fn parse_findings(report: &str) -> (Vec<Found>, Vec<String>) {
    let section = |name: &str| -> Option<&str> {
        let at = report.lines().scan(0, |pos, l| {
            let here = *pos;
            *pos += l.len() + 1;
            Some((here, l))
        });
        let mut start = None;
        let mut end = report.len();
        for (pos, l) in at {
            if let Some(h) = l.strip_prefix("## ") {
                if start.is_some() {
                    end = pos;
                    break;
                }
                if h.trim().eq_ignore_ascii_case(name) {
                    start = Some(pos + l.len() + 1);
                }
            }
        }
        start.map(|s| &report[s.min(report.len())..end])
    };
    let mut found: Vec<Found> = Vec::new();
    // Which part of a finding lines go to: 0 its text, 1 its evidence.
    let mut into = 0;
    for line in section("Needs you").unwrap_or("").lines() {
        let t = line.trim();
        let top = !line.starts_with(' ') && !line.starts_with('\t');
        let item = top.then(|| {
            let digits = t.chars().take_while(char::is_ascii_digit).count();
            let rest = t[digits..].strip_prefix(". ").or_else(|| t[digits..].strip_prefix(") ")).filter(|_| digits > 0);
            rest.or_else(|| t.strip_prefix("- "))
        });
        if let Some(Some(rest)) = item {
            let plain = rest.replace("**", "");
            let lower = plain.to_lowercase();
            let tag = [("[confirmed]", true), ("[plausible]", false)].into_iter().find_map(|(tag, c)| lower.find(tag).map(|at| (at + tag.len(), c)));
            let Some((after, confirmed)) = tag else {
                into = 2;
                continue;
            };
            let after = plain[after..].trim();
            let (label, text) = match after.split_once(':') {
                Some((l, t)) => (l.trim(), t.trim()),
                None => (after, ""),
            };
            found.push(Found { confirmed, label: label.to_string(), text: text.to_string(), ..Default::default() });
            into = 0;
            continue;
        }
        let Some(f) = found.last_mut().filter(|_| into < 2) else { continue };
        if t.is_empty() {
            continue;
        }
        if let Some(q) = t.strip_prefix('>').map(str::trim).filter(|_| f.quote.is_empty() || f.text.is_empty()) {
            f.quote = format!("{} {q}", f.quote).trim().to_string();
        } else if let Some(e) = t.strip_prefix("Evidence:").or_else(|| t.strip_prefix("**Evidence:**")) {
            f.evidence = e.trim().to_string();
            into = 1;
        } else if into == 1 {
            f.evidence = format!("{} {t}", f.evidence).trim().to_string();
        } else {
            f.text = format!("{} {t}", f.text).trim().to_string();
        }
    }
    let held = section("Checked")
        .unwrap_or("")
        .lines()
        .filter_map(|l| {
            let l = l.trim().strip_prefix("- ")?.replace("**", "");
            let (label, rest) = l.split_once(':')?;
            rest.trim().to_lowercase().starts_with("held").then(|| label.trim().to_string())
        })
        .collect();
    (found, held)
}

/// Markdown's inline marks out of a quote, so it reads as the report is drawn.
pub fn plain(s: &str) -> String {
    s.replace("**", "").replace('`', "").trim().to_string()
}

// ---------------------------------------------------------------------------------------------
// Following them
// ---------------------------------------------------------------------------------------------

/// Once a second: a challenger that finished has its findings placed; a worktree whose time is
/// up is removed.
pub fn tick(ed: &mut Editor) {
    if ed.challenges.checked.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
        return;
    }
    ed.challenges.checked = Some(Instant::now());
    load(ed);
    if ed.challenges.runs.is_empty() {
        return;
    }
    let mut changed = false;
    for i in 0..ed.challenges.runs.len() {
        let run = ed.challenges.runs[i].clone();
        if run.ended.is_some() || run.challenger.is_empty() {
            continue;
        }
        let Some(buf) = crate::remote::chat_buffer(ed, crate::remote::LOCAL, &run.challenger) else { continue };
        let Some(Some(done)) = ed.with_buffer_as::<ChatBuffer, _>(buf, |c, _| c.finished_reply()) else { continue };
        let r = &mut ed.challenges.runs[i];
        r.ended = Some(now());
        changed = true;
        match done {
            Ok(text) => {
                let (found, held) = parse_findings(&text);
                r.findings = found.len();
                r.held = held;
                let run = r.clone();
                deliver(ed, &run, found);
            }
            Err(why) => {
                r.error = Some(why.clone());
                ed.warn(format!("The challenge stopped: {why}"));
            }
        }
        let run = ed.challenges.runs[i].clone();
        ed.run_hook("chat-challenge-finished", &to_json(ed, &run));
    }
    // Worktrees whose time is up, their chats closed.
    let ttl = ttl(ed);
    let due: Vec<Run> = ed
        .challenges
        .runs
        .iter()
        .filter(|r| ttl > 0 && !r.keep && r.ended.is_some_and(|e| now() >= e + ttl) && crate::remote::chat_buffer(ed, crate::remote::LOCAL, &r.challenger).is_none())
        .cloned()
        .collect();
    for r in due {
        remove(ed, &r.id);
        changed = true;
    }
    if changed {
        save(ed);
    }
}

/// Place the findings on the challenged chat's report, as drafts of its review.
fn deliver(ed: &mut Editor, run: &Run, found: Vec<Found>) {
    let n = found.len();
    match crate::remote::chat_buffer(ed, crate::remote::LOCAL, &run.chat) {
        Some(buf) => {
            ed.with_buffer_as::<ChatBuffer, _>(buf, |c, ed| c.add_findings(ed, run.entry, &run.opening, found));
        }
        None => {
            // Closed: into its saved review, found where they are about when it is opened.
            let mut saved = crate::chat_review::load(None, &run.chat);
            saved.add_findings(found.into_iter().map(|f| comment_of(run.entry, &run.opening, f, None)).collect());
            if let Err(e) = crate::chat_review::save(None, &run.chat, &saved) {
                ed.warn(format!("Challenge: couldn't save its findings: {e:#}"));
            }
        }
    }
    ed.message(match n {
        0 => "The challenge found nothing wrong with the report".to_string(),
        1 => "The challenge found 1 thing wrong: it is in your review".to_string(),
        n => format!("The challenge found {n} things wrong: they are in your review"),
    });
}

/// A finding as a draft comment of the review (`quote`: where it was found on the report).
pub fn comment_of(entry: usize, opening: &str, f: Found, quote: Option<String>) -> crate::chat_review::Comment {
    crate::chat_review::Comment {
        entry,
        quote: quote.unwrap_or_else(|| plain(&f.quote)),
        label: f.label,
        opening: opening.to_string(),
        ask: crate::chat_review::Ask::Fix,
        challenge: Some(crate::chat_review::Finding { confirmed: f.confirmed, text: f.text, evidence: f.evidence }),
        ..Default::default()
    }
}

/// Remove challenge `id`'s worktree and branch now (its chat, if open, is closed first).
pub fn remove(ed: &mut Editor, id: &str) {
    load(ed);
    let Some(i) = ed.challenges.runs.iter().position(|r| r.id == id) else { return };
    let r = ed.challenges.runs.remove(i);
    save(ed);
    if let Some(buf) = crate::remote::chat_buffer(ed, crate::remote::LOCAL, &r.challenger) {
        ed.kill_buffer(buf);
    }
    if r.worktree.as_os_str().is_empty() {
        return;
    }
    let main = ed.main_sender();
    ed.rt.spawn_blocking(move || {
        let removed = crate::git::remove_worktree(&r.repo, &r.worktree, true).and_then(|_| crate::git::delete_branch(&r.repo, &r.branch, true));
        main.send(move |ed| {
            crate::sidebar::worktree_removed(ed, &r.repo, &r.worktree);
            match removed {
                Ok(()) => {
                    ed.run_hook("worktree-removed", &json!({"project": r.repo.to_string_lossy(), "path": r.worktree.to_string_lossy(), "branch": r.branch}));
                }
                // Gone already (removed by hand): nothing to do.
                Err(e) if !r.worktree.exists() => tracing::debug!("challenge {}: {e:#}", r.id),
                Err(e) => ed.warn(format!("Challenge {}: couldn't remove its worktree: {e:#}", r.id)),
            }
        });
    });
}

/// Keep challenge `id`'s worktree (or let it go again).
pub fn keep(ed: &mut Editor, id: &str, on: bool) {
    load(ed);
    if let Some(r) = ed.challenges.runs.iter_mut().find(|r| r.id == id) {
        r.keep = on;
    }
    save(ed);
}

// ---------------------------------------------------------------------------------------------
// Settings and commands
// ---------------------------------------------------------------------------------------------

pub fn install(ed: &mut Editor) {
    ed.defvar("chat-report-challenge", json!(true), "Challenge an agent's report: a fresh agent, in a worktree of its own, tries to prove the report's claims wrong, and what it finds goes to your review as drafts under the claims (chat-report-challenge, the command). Off: no Challenge on reports, and agents don't get the challenge skill.");
    ed.defvar("chat-report-challenge-model", Value::Null, "The challenger's model (\"provider/id\"); nil: the challenged chat's own. A model from another vendor catches more of what the author's missed.");
    ed.defvar("chat-report-challenge-thinking", Value::Null, "The challenger's thinking level (off, minimal, low, medium, high, xhigh, max); nil: new chats' default.");
    ed.defvar("chat-report-challenge-skill", json!("challenge"), "The skill the challenger is sent by name: serpis's own (skills/challenge), or one of yours by that name or another.");
    ed.defvar("chat-report-challenge-run", json!(true), "Whether the challenger may run commands, such as the report's checks; off, it only reads.");
    ed.defvar("chat-report-challenge-worktree-ttl", json!(1800), "Seconds after a challenge ends before its worktree and branch are removed, once its chat is closed; 0 keeps them.");
    let o = crate::editor::BUILTIN;
    ed.define_command(
        "chat-report-challenge",
        "Challenge the chat's latest report (or the report at entry `entry`): a fresh agent in a new worktree tries to prove its claims wrong, and its findings go to your review. Param: {model, thinking, note, entry}, each optional; chat-report-challenge-model and -thinking by default.",
        o,
        |ed, a| {
            let buf = crate::commands::current_chat(ed).ok_or_else(|| anyhow::anyhow!("Not in a chat buffer"))?;
            let p = a.params.first().cloned().unwrap_or(Value::Null);
            let s = |k: &str| p.get(k).and_then(Value::as_str).map(str::to_string).filter(|s| !s.is_empty());
            let entry = p.get("entry").and_then(Value::as_u64).map(|e| e.saturating_sub(1) as usize);
            start(ed, buf, entry, Options { model: s("model"), thinking: s("thinking"), note: s("note").unwrap_or_default() })
        },
    );
    ed.define_command("chat-challenge-keep", "Keep a challenge's worktree instead of removing it a while after it ends (param: its id; default the current chat's latest). With C-u, let it go again.", o, |ed, a| {
        let id = challenge_id(ed, a.str(0))?;
        let on = a.prefix.is_none();
        keep(ed, &id, on);
        ed.message(if on { format!("Challenge {id}'s worktree is kept") } else { format!("Challenge {id}'s worktree will be removed") });
        Ok(())
    });
    ed.define_command("chat-challenge-remove", "Remove a challenge's worktree and branch now, closing its chat (param: its id; default the current chat's latest).", o, |ed, a| {
        let id = challenge_id(ed, a.str(0))?;
        remove(ed, &id);
        Ok(())
    });
    ed.define_command("chat-challenge-open", "Show a challenge's chat (param: its id; default the current chat's latest).", o, |ed, a| {
        let id = challenge_id(ed, a.str(0))?;
        let challenger = ed.challenges.runs.iter().find(|r| r.id == id).map(|r| r.challenger.clone()).unwrap_or_default();
        match crate::remote::chat_buffer(ed, crate::remote::LOCAL, &challenger) {
            Some(b) => ed.show_buffer(b),
            None => anyhow::bail!("Challenge {id}'s chat is closed"),
        }
        Ok(())
    });
}

/// `given`, or the current chat's latest challenge.
fn challenge_id(ed: &mut Editor, given: Option<&str>) -> anyhow::Result<String> {
    load(ed);
    if let Some(id) = given {
        return Ok(id.to_string());
    }
    let chat = crate::commands::with_chat(ed, |c, _| c.chat_id())?;
    // The challenger's own chat counts as its challenge's.
    ed.challenges.runs.iter().rev().find(|r| r.chat == chat || r.challenger == chat).map(|r| r.id.clone()).ok_or_else(|| anyhow::anyhow!("No challenge of this chat"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn findings_are_read_from_the_challengers_report() {
        let report = "Done: two claims don't hold.\n\n## Needs you\n\n1. **[confirmed] Changed 1**\n   > This is the likely cause of the flaky `retries_respect_limit` test in CI.\n   The race is still there: `push_front` runs first.\n   It reads the old count.\n   Evidence: 8 workers failed 3 of 500 runs.\n2. **[Plausible] Checked 2:** The loop ran on one thread.\n   > passed every time.\n3. Something else, not a finding.\n   > not a quote\n\n## Checked\n\n- **Changed 2:** held. Read the backoff.\n- **Checked 1:** held, reran it.\n- **Changed 3:** not checked.\n";
        let (found, held) = parse_findings(report);
        assert_eq!(found.len(), 2, "{found:#?}");
        assert_eq!(
            found[0],
            Found {
                confirmed: true,
                label: "Changed 1".into(),
                quote: "This is the likely cause of the flaky `retries_respect_limit` test in CI.".into(),
                text: "The race is still there: `push_front` runs first. It reads the old count.".into(),
                evidence: "8 workers failed 3 of 500 runs.".into(),
            }
        );
        // Its finding on the item's line, the quote after it.
        assert_eq!((found[1].confirmed, found[1].label.as_str(), found[1].text.as_str(), found[1].quote.as_str()), (false, "Checked 2", "The loop ran on one thread.", "passed every time."));
        assert_eq!(held, ["Changed 2", "Checked 1"]);
        assert_eq!(plain(&found[0].quote), "This is the likely cause of the flaky retries_respect_limit test in CI.");
        // Nothing found.
        assert_eq!(parse_findings("Done: every claim held.\n\n## Checked\n\n- **Headline:** held.\n"), (vec![], vec!["Headline".to_string()]));
    }

    #[test]
    fn uncommitted_changes_are_copied_into_the_worktree() {
        let root = std::env::temp_dir().join(format!("serpis-challenge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (src, dst) = (root.join("src"), root.join("wt"));
        std::fs::create_dir_all(&src).unwrap();
        let git = |dir: &Path, args: &[&str]| assert!(Command::new("git").arg("-C").arg(dir).args(args).output().unwrap().status.success(), "git {args:?}");
        git(&src, &["init", "-q"]);
        git(&src, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "start"]);
        std::fs::write(src.join("a.txt"), "one\n").unwrap();
        git(&src, &["add", "a.txt"]);
        git(&src, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "a"]);
        git(&src, &["worktree", "add", "-q", "-b", "challenge-x", dst.to_str().unwrap()]);
        // Changed, staged and new (in a new folder too).
        std::fs::write(src.join("a.txt"), "two\n").unwrap();
        std::fs::write(src.join("b.txt"), "staged\n").unwrap();
        git(&src, &["add", "b.txt"]);
        std::fs::create_dir_all(src.join("new")).unwrap();
        std::fs::write(src.join("new/c.txt"), "untracked\n").unwrap();
        copy_changes(&src, &dst).unwrap();
        assert_eq!(std::fs::read_to_string(dst.join("a.txt")).unwrap(), "two\n");
        assert_eq!(std::fs::read_to_string(dst.join("b.txt")).unwrap(), "staged\n");
        assert_eq!(std::fs::read_to_string(dst.join("new/c.txt")).unwrap(), "untracked\n");
        // The chat's own checkout is untouched.
        assert_eq!(std::fs::read_to_string(src.join("a.txt")).unwrap(), "two\n");
        let _ = std::fs::remove_dir_all(&root);
    }
}
