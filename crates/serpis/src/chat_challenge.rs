//! Challenging an agent's report: a fresh agent, in a worktree of its own (the chat's commits
//! with its uncommitted changes copied in), tries to prove the report's claims wrong. It says
//! what it found with its own tools (serpis_agent::challenge: its chat's session holds the
//! report they check calls against), each finding going to the challenged chat's review as a
//! draft under the claim it disputes, to send with the rest, as soon as it is made. A challenger
//! without them (a harness they couldn't reach) lists its findings in its report instead, in
//! the shape skills/challenge gives. The worktree is removed a while after the challenge ends
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
    /// Findings already placed on the report, of those its challenger recorded with its tool.
    #[serde(default)]
    pub placed: usize,
    pub error: Option<String>,
    /// Kept: its worktree isn't removed by itself.
    pub keep: bool,
    /// A finding its challenger was asked to prove: (its comment's id in the challenged chat's
    /// review, the challenger's entries when it was asked).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proving: Option<(u64, usize)>,
}

/// The challenges this window started (kept in `<config>/challenges.json`, so a worktree is
/// still removed after a restart).
#[derive(Default)]
pub struct Challenges {
    pub runs: Vec<Run>,
    loaded: bool,
    checked: Option<Instant>,
    /// Challenges that stopped whose challenger has been seen idle since: working again, it was
    /// retried or continued in its chat.
    idle: std::collections::HashSet<String>,
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
        "proving": r.proving.map(|(id, _)| id),
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
                launch(ed, run, path, model, thinking, &report, prompt);
            });
        });
    });
    Ok(())
}

/// Start the challenger's chat in the worktree and send it the challenge.
fn launch(ed: &mut Editor, mut run: Run, worktree: PathBuf, model: Option<String>, thinking: Option<String>, report: &str, prompt: String) {
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
        // What gives it its tools, before its first turn asks for them.
        c.append_entry(serpis_agent::challenge::ENTRY, json!({"report": report.trim()}));
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

/// A finding as the challenger recorded it with `challenge_finding`.
pub fn found_of(args: &Value) -> Found {
    let s = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
    Found { confirmed: s("status") == "confirmed", label: s("claim"), quote: s("quote").trim_start_matches('>').trim().to_string(), text: s("finding"), evidence: s("evidence") }
}

/// The findings and the claims that held, as challenger buffer `buf` recorded them with its
/// tools; None when it used neither (its report has them, then).
fn recorded(ed: &mut Editor, challenger: &str) -> Option<(Vec<Found>, Vec<String>)> {
    use serpis_agent::challenge::{FINDING, HELD};
    let buf = crate::remote::chat_buffer(ed, crate::remote::LOCAL, challenger)?;
    let calls = ed.with_buffer_as::<ChatBuffer, _>(buf, |c, _| c.challenge_calls(0))?;
    // The same finding twice (recorded again after it was retried or continued): once.
    let mut found: Vec<Found> = Vec::new();
    for f in calls.iter().filter(|(t, _)| *t == FINDING).map(|(_, a)| found_of(a)) {
        if !found.iter().any(|g| g.label.eq_ignore_ascii_case(&f.label) && plain(&g.quote) == plain(&f.quote)) {
            found.push(f);
        }
    }
    let mut held: Vec<String> = Vec::new();
    for c in calls.iter().filter(|(t, _)| *t == HELD).filter_map(|(_, a)| a["claim"].as_str().map(|c| c.trim().to_string())) {
        if !held.contains(&c) {
            held.push(c);
        }
    }
    (!found.is_empty() || !held.is_empty()).then_some((found, held))
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
    // A challenge that stopped (a rate limit, say) goes on when its chat does, retried or
    // continued there: it ends again when that chat finishes.
    for i in 0..ed.challenges.runs.len() {
        let run = ed.challenges.runs[i].clone();
        if run.error.is_none() {
            continue;
        }
        let Some(buf) = crate::remote::chat_buffer(ed, crate::remote::LOCAL, &run.challenger) else { continue };
        let Some((running, finished)) = ed.with_buffer_as::<ChatBuffer, _>(buf, |c, _| (c.running(), matches!(c.finished_reply(), Some(Ok(_))))) else { continue };
        if !ed.challenges.idle.contains(&run.id) {
            // Still winding down from what stopped it (Stop aborts it).
            if !running {
                ed.challenges.idle.insert(run.id);
            }
            continue;
        }
        if running || finished {
            ed.challenges.idle.remove(&run.id);
            let r = &mut ed.challenges.runs[i];
            r.ended = None;
            r.error = None;
            changed = true;
            ed.message("The challenge goes on");
        }
    }
    // Findings recorded while it works go on the report at once.
    for i in 0..ed.challenges.runs.len() {
        let run = ed.challenges.runs[i].clone();
        if run.ended.is_some() || run.challenger.is_empty() {
            continue;
        }
        let Some((found, _)) = recorded(ed, &run.challenger) else { continue };
        if found.len() > run.placed {
            let new: Vec<Found> = found[run.placed..].to_vec();
            ed.challenges.runs[i].placed = found.len();
            changed = true;
            ed.message(match &new[..] {
                [f] => format!("The challenge found something wrong with {}: it is in your review", if f.label.is_empty() { "the report" } else { &f.label }),
                _ => format!("The challenge found {} more things wrong: they are in your review", new.len()),
            });
            place(ed, &run, new);
        }
    }
    for i in 0..ed.challenges.runs.len() {
        let run = ed.challenges.runs[i].clone();
        if run.ended.is_some() || run.challenger.is_empty() {
            continue;
        }
        // Its chat closed (its worktree removed, say): no report coming.
        let done = match crate::remote::chat_buffer(ed, crate::remote::LOCAL, &run.challenger) {
            None => Err("its chat was closed before it finished".to_string()),
            Some(buf) => {
                let Some(Some(done)) = ed.with_buffer_as::<ChatBuffer, _>(buf, |c, _| c.finished_reply()) else { continue };
                done
            }
        };
        end(ed, i, done);
        changed = true;
    }
    // A finding a challenger was asked to prove: its answer.
    for i in 0..ed.challenges.runs.len() {
        let run = ed.challenges.runs[i].clone();
        let Some((id, before)) = run.proving else { continue };
        let Some(buf) = crate::remote::chat_buffer(ed, crate::remote::LOCAL, &run.challenger) else {
            // Its chat was closed: no answer coming.
            ed.challenges.runs[i].proving = None;
            set_finding(ed, &run, id, |f| f.proving = false);
            changed = true;
            continue;
        };
        let Some(Some((done, proof))) = ed.with_buffer_as::<ChatBuffer, _>(buf, |c, _| {
            let proof = c.challenge_calls(before).into_iter().rev().find(|(t, _)| *t == serpis_agent::challenge::PROOF).map(|(_, a)| a);
            (c.entry_count() > before + 1).then(|| c.finished_reply().map(|d| (d, proof))).flatten()
        }) else {
            continue;
        };
        ed.challenges.runs[i].proving = None;
        changed = true;
        // Its challenge_proof call, else what its reply says.
        let answer = match proof {
            Some(a) => {
                let evidence = a["evidence"].as_str().unwrap_or("").trim().to_string();
                Ok(if a["outcome"] == "confirmed" { Proof::Confirmed(evidence) } else { Proof::Withdrawn(evidence) })
            }
            None => done.map(|t| parse_proof(&t)),
        };
        set_finding(ed, &run, id, |f| {
            f.proving = false;
            match answer {
                Ok(Proof::Confirmed(evidence)) => {
                    f.confirmed = true;
                    if !evidence.is_empty() {
                        f.evidence = evidence;
                    }
                }
                Ok(Proof::Withdrawn(why)) => f.withdrawn = Some(why),
                Ok(Proof::Unclear) | Err(_) => {}
            }
        });
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

/// Run `i` is over: its findings placed (from the challenger's report), or why it has none.
fn end(ed: &mut Editor, i: usize, done: Result<String, String>) {
    let r = &mut ed.challenges.runs[i];
    r.ended = Some(now());
    match done {
        Ok(text) => {
            let challenger = r.challenger.clone();
            // What it recorded with its tools (those not placed yet), else its report's list.
            let (found, held, before) = match recorded(ed, &challenger) {
                Some((found, held)) => (found, held, ed.challenges.runs[i].placed),
                None => {
                    let (found, held) = parse_findings(&text);
                    (found, held, 0)
                }
            };
            let r = &mut ed.challenges.runs[i];
            r.findings = found.len();
            r.placed = found.len();
            r.held = held;
            let run = r.clone();
            place(ed, &run, found.into_iter().skip(before).collect());
            ed.message(match run.findings {
                0 => "The challenge found nothing wrong with the report".to_string(),
                1 => "The challenge found 1 thing wrong: it is in your review".to_string(),
                n => format!("The challenge found {n} things wrong: they are in your review"),
            });
        }
        Err(why) => {
            r.error = Some(why.clone());
            ed.warn(format!("The challenge stopped: {why}"));
        }
    }
    let run = ed.challenges.runs[i].clone();
    ed.run_hook("chat-challenge-finished", &to_json(ed, &run));
}

/// Place the findings on the challenged chat's report, as drafts of its review.
fn place(ed: &mut Editor, run: &Run, found: Vec<Found>) {
    if found.is_empty() {
        return;
    }
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
}

/// A finding as a draft comment of the review (`quote`: where it was found on the report).
pub fn comment_of(entry: usize, opening: &str, f: Found, quote: Option<String>) -> crate::chat_review::Comment {
    crate::chat_review::Comment {
        entry,
        quote: quote.unwrap_or_else(|| plain(&f.quote)),
        label: f.label,
        opening: opening.to_string(),
        ask: crate::chat_review::Ask::Fix,
        challenge: Some(crate::chat_review::Finding { confirmed: f.confirmed, text: f.text, evidence: f.evidence, ..Default::default() }),
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

/// What a challenger answered when asked to prove a finding.
#[derive(Debug, Clone, PartialEq)]
pub enum Proof {
    /// It ran something that shows it: what (its evidence).
    Confirmed(String),
    /// It took it back: why.
    Withdrawn(String),
    Unclear,
}

/// A challenger's answer to Prove it: `[confirmed]` with an `Evidence:` line, or `[withdrawn]`
/// and why.
pub fn parse_proof(text: &str) -> Proof {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let after_tag = |tag: &str| -> Option<String> {
        let i = lines.iter().position(|l| l.to_lowercase().contains(tag))?;
        let l = lines[i].replace("**", "");
        let at = l.to_lowercase().find(tag)? + tag.len();
        let rest = l[at..].trim().trim_start_matches(':').trim().to_string();
        Some(if rest.is_empty() { lines.get(i + 1).map(|s| s.to_string()).unwrap_or_default() } else { rest })
    };
    let evidence = lines.iter().find_map(|l| l.replace("**", "").strip_prefix("Evidence:").map(|e| e.trim().to_string()));
    if let Some(why) = after_tag("[withdrawn]") {
        return Proof::Withdrawn(why);
    }
    match after_tag("[confirmed]") {
        Some(rest) => Proof::Confirmed(evidence.unwrap_or(rest)),
        None => Proof::Unclear,
    }
}

/// Change finding `id` of `run`'s challenged chat: in its buffer when open, else its saved review.
fn set_finding(ed: &mut Editor, run: &Run, id: u64, f: impl FnOnce(&mut crate::chat_review::Finding)) {
    match crate::remote::chat_buffer(ed, crate::remote::LOCAL, &run.chat) {
        Some(buf) => {
            ed.with_buffer_as::<ChatBuffer, _>(buf, |c, ed| c.update_finding(ed, id, f));
        }
        None => {
            let mut saved = crate::chat_review::load(None, &run.chat);
            if let Some(found) = saved.comments.iter_mut().find(|c| c.id == id).and_then(|c| c.challenge.as_mut()) {
                f(found);
                let _ = crate::chat_review::save(None, &run.chat, &saved);
            }
        }
    }
}

/// Prove it: ask the challenger of chat buffer `buf`'s finding `id` to show it by running
/// something, or take it back. Its answer changes the finding (tick).
pub fn prove(ed: &mut Editor, buf: BufferId, id: u64) -> anyhow::Result<()> {
    load(ed);
    let (chat, comment) = ed.with_buffer_as::<ChatBuffer, _>(buf, |c, _| (c.chat_id(), c.comment(id))).ok_or_else(|| anyhow::anyhow!("Not a chat buffer"))?;
    let comment = comment.ok_or_else(|| anyhow::anyhow!("No comment {id} in the review"))?;
    let finding = comment.challenge.clone().ok_or_else(|| anyhow::anyhow!("That comment isn't a challenge's"))?;
    let i = ed.challenges.runs.iter().rposition(|r| r.chat == chat && r.entry == comment.entry).ok_or_else(|| anyhow::anyhow!("The challenge it came from is gone"))?;
    let run = ed.challenges.runs[i].clone();
    if run.ended.is_none() || run.proving.is_some() {
        anyhow::bail!("Its challenger is busy: try again when it is done");
    }
    let challenger = crate::remote::chat_buffer(ed, crate::remote::LOCAL, &run.challenger).ok_or_else(|| anyhow::anyhow!("Its challenger's chat is closed"))?;
    let place = if comment.label.is_empty() { "In the report".to_string() } else { comment.label.clone() };
    let quote: String = comment.quote.lines().map(|l| format!("> {l}\n")).collect();
    let prompt = format!(
        "Prove this finding of yours, or take it back.\n\n{place}:\n{quote}Your finding: {}\n\nRun something that shows it. Then answer with `challenge_proof`: confirmed, with what you ran and what it showed, or withdrawn, with why it doesn't hold. Without that tool, answer with a line starting `[confirmed]` and a line `Evidence: what you ran and what it showed`, or a line starting `[withdrawn]` and why it doesn't hold.",
        finding.text
    );
    let before = ed.with_buffer_as::<ChatBuffer, _>(challenger, |c, ed| {
        let n = c.entry_count();
        c.submit(ed, prompt, false);
        n
    });
    ed.challenges.runs[i].proving = Some((id, before.unwrap_or(0)));
    save(ed);
    ed.with_buffer_as::<ChatBuffer, _>(buf, |c, ed| c.update_finding(ed, id, |f| f.proving = true));
    ed.message("Asked the challenger to prove it…");
    Ok(())
}

/// Show challenge `id`'s chat.
pub fn open(ed: &mut Editor, id: &str) -> anyhow::Result<()> {
    load(ed);
    let challenger = ed.challenges.runs.iter().find(|r| r.id == id).map(|r| r.challenger.clone()).unwrap_or_default();
    match crate::remote::chat_buffer(ed, crate::remote::LOCAL, &challenger) {
        Some(b) => ed.show_buffer(b),
        None => anyhow::bail!("Challenge {id}'s chat is closed"),
    }
    Ok(())
}

/// Stop challenge `id`: its challenger, if its chat is open and working, and the challenge
/// itself, which then has no findings.
pub fn stop(ed: &mut Editor, id: &str) {
    load(ed);
    let Some(i) = ed.challenges.runs.iter().position(|r| r.id == id) else { return };
    let challenger = ed.challenges.runs[i].challenger.clone();
    if let Some(b) = crate::remote::chat_buffer(ed, crate::remote::LOCAL, &challenger) {
        ed.with_buffer_as::<ChatBuffer, _>(b, |c, _| c.abort());
    }
    if ed.challenges.runs[i].ended.is_none() {
        end(ed, i, Err("stopped".into()));
        save(ed);
    }
}

// ---------------------------------------------------------------------------------------------
// Settings and commands
// ---------------------------------------------------------------------------------------------

pub fn install(ed: &mut Editor) {
    ed.defvar("chat-report-challenge", json!(true), "Challenge an agent's report: a fresh agent, in a worktree of its own, tries to prove the report's claims wrong, and what it finds goes to your review as drafts under the claims (chat-report-challenge, the command). Off: no Challenge on reports, and agents don't get the challenge skill.");
    // What the challenge form's "default from now on" chose, kept between runs.
    ed.defvar("chat-report-challenge-model", Editor::persisted_state("chat-report-challenge-model").unwrap_or(Value::Null), "The challenger's model (\"provider/id\"); nil: the challenged chat's own. A model from another vendor catches more of what the author's missed. The challenge form's \"default from now on\" sets it, kept between runs.");
    ed.defvar("chat-report-challenge-thinking", Editor::persisted_state("chat-report-challenge-thinking").unwrap_or(Value::Null), "The challenger's thinking level (off, minimal, low, medium, high, xhigh, max); nil: new chats' default. The challenge form's \"default from now on\" sets it, kept between runs.");
    ed.defvar("chat-report-challenge-skill", json!("challenge"), "The skill the challenger is sent by name: serpis's own (skills/challenge), or one of yours by that name or another.");
    ed.defvar("chat-report-challenge-run", json!(true), "Whether the challenger may run commands, such as the report's checks; off, it only reads.");
    ed.defvar("chat-report-challenge-worktree-ttl", json!(1800), "Seconds after a challenge ends before its worktree and branch are removed, once its chat is closed; 0 keeps them.");
    let o = crate::editor::BUILTIN;
    ed.define_command(
        "chat-report-challenge",
        "Challenge the chat's latest report: a fresh agent in a new worktree tries to prove its claims wrong, and its findings go to your review. With no param, set it up in the composer first (its model, thinking, and what to tell it); with {model, thinking, note, entry}, each optional, start it now (chat-report-challenge-model and -thinking by default; entry: the report's, 1-based).",
        o,
        |ed, a| {
            let buf = crate::commands::current_chat(ed).ok_or_else(|| anyhow::anyhow!("Not in a chat buffer"))?;
            if a.params.is_empty() {
                return crate::commands::with_chat(ed, |c, ed| c.open_challenge_form(ed, None));
            }
            let p = a.params.first().cloned().unwrap_or(Value::Null);
            let s = |k: &str| p.get(k).and_then(Value::as_str).map(str::to_string).filter(|s| !s.is_empty());
            let entry = p.get("entry").and_then(Value::as_u64).map(|e| e.saturating_sub(1) as usize);
            start(ed, buf, entry, Options { model: s("model"), thinking: s("thinking"), note: s("note").unwrap_or_default() })
        },
    );
    let form = |ed: &mut Editor, f: &mut dyn FnMut(&mut ChatBuffer, &mut Editor)| crate::commands::with_chat(ed, |c, ed| f(c, ed));
    ed.define_command("chat-challenge-form-submit", "Start the challenge set up in the composer, with what you typed as its note.", o, move |ed, _| form(ed, &mut |c, ed| c.submit_challenge_form(ed)));
    ed.define_command("chat-challenge-form-cancel", "Drop the challenge being set up in the composer.", o, move |ed, _| form(ed, &mut |c, _| {
        c.cancel_challenge_form();
    }));
    ed.define_command("chat-challenge-form-escape", "In vim's insert state, back to normal; otherwise drop the challenge being set up.", o, |ed, _| {
        let id = ed.current_buffer();
        if crate::packages::vim::state_of(ed, id).is_some_and(|s| s == crate::packages::vim::State::Insert) {
            crate::packages::vim::set_state(ed, id, crate::packages::vim::State::Normal);
            return Ok(());
        }
        crate::commands::with_chat(ed, |c, _| {
            c.cancel_challenge_form();
        })
    });
    ed.define_command("chat-challenge-form-model", "The model of the challenge being set up (param: provider/model-id; none: choose it).", o, |ed, a| {
        if let Some(spec) = a.str(0).map(str::to_string) {
            return crate::commands::with_chat(ed, |c, _| {
                if let Some(f) = c.challenge_form_mut() {
                    f.model = Some(spec);
                }
            });
        }
        let id = crate::commands::current_chat(ed).ok_or_else(|| anyhow::anyhow!("Not in a chat buffer"))?;
        let ours = ed.with_buffer_as::<ChatBuffer, _>(id, |c, _| c.state_now().model).flatten();
        let vendor = |spec: &str| vendor_of(spec);
        let ours_vendor = ours.as_deref().map(vendor).unwrap_or_default();
        let configured = ed.registry.configured_providers();
        let mut specs: Vec<(String, String)> = ed
            .registry
            .models()
            .into_iter()
            .filter(|m| configured.contains(&m.provider) && serpis_ai::api::chat_api(&m.api))
            .map(|m| (format!("{}/{}", m.provider, m.id), m.display_name().to_string()))
            .collect();
        // Another vendor's models first: they catch more of what the author's missed.
        specs.sort_by_key(|(spec, _)| (vendor(spec) == ours_vendor, spec.clone()));
        let cands = specs.into_iter().map(|(spec, name)| {
            let other = vendor(&spec) != ours_vendor;
            crate::minibuffer::Candidate::new(spec).detail(if other { format!("{name}  · another vendor") } else { name })
        }).collect();
        ed.completing_read("Challenger's model: ", cands, true, move |ed, c, _| {
            let Some(c) = c else { return };
            ed.with_buffer_as::<ChatBuffer, _>(id, |chat, _| {
                if let Some(f) = chat.challenge_form_mut() {
                    f.model = Some(c.value);
                }
            });
        });
        Ok(())
    });
    ed.define_command("chat-challenge-form-thinking", "The thinking level of the challenge being set up: the next one, or the param (off, minimal, low, medium, high, xhigh, max; \"default\": new chats').", o, |ed, a| {
        let set = a.str(0).map(str::to_string);
        crate::commands::with_chat(ed, |c, _| {
            let Some(f) = c.challenge_form_mut() else { return };
            let levels: Vec<Option<String>> = std::iter::once(None).chain(serpis_ai::ThinkingLevel::ALL.iter().map(|l| Some(l.as_str().to_string()))).collect();
            f.thinking = match set.as_deref() {
                Some("default") => None,
                Some(s) => Some(s.to_string()),
                None => levels[(levels.iter().position(|l| *l == f.thinking).map_or(0, |p| p + 1)) % levels.len()].clone(),
            };
        })
    });
    ed.define_command("chat-challenge-form-remember", "Whether the model and thinking of the challenge being set up are the default (chat-report-challenge-model, -thinking): ticked, they become it; unticked, there is none (challenges start on the chat's own model). It opens ticked when there is one.", o, |ed, _| {
        crate::commands::with_chat(ed, |c, _| {
            if let Some(f) = c.challenge_form_mut() {
                f.remember = !f.remember;
            }
        })
    });
    ed.define_command("chat-challenge-prove", "Prove it: ask a finding's challenger to show it by running something, or take it back (param: the finding's comment id).", o, |ed, a| {
        let id = a.params.first().and_then(Value::as_u64).ok_or_else(|| anyhow::anyhow!("chat-challenge-prove: which finding (its comment id)?"))?;
        let buf = crate::commands::current_chat(ed).ok_or_else(|| anyhow::anyhow!("Not in a chat buffer"))?;
        prove(ed, buf, id)
    });
    ed.define_command("chat-challenge-keep-finding", "Keep a finding its challenger withdrew: it goes with your next message again (param: its comment id).", o, |ed, a| {
        let id = a.params.first().and_then(Value::as_u64).ok_or_else(|| anyhow::anyhow!("chat-challenge-keep-finding: which finding (its comment id)?"))?;
        crate::commands::with_chat(ed, |c, ed| {
            c.update_finding(ed, id, |f| f.withdrawn = None);
        })
    });
    ed.define_command("chat-challenge-stop", "Stop a challenge's challenger (param: its id; default the current chat's latest).", o, |ed, a| {
        let id = challenge_id(ed, a.str(0))?;
        stop(ed, &id);
        Ok(())
    });
    for (k, c) in [("RET", "chat-challenge-form-submit"), ("C-RET", "chat-challenge-form-submit"), ("C-c C-c", "chat-challenge-form-submit"), ("C-c C-k", "chat-challenge-form-cancel"), ("ESC", "chat-challenge-form-escape"), ("M-m", "chat-challenge-form-model"), ("M-t", "chat-challenge-form-thinking")] {
        ed.bind("chat-challenging", k, c, o);
    }
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
        open(ed, &id)
    });
}

/// Who makes model `spec` (`provider/id`, or `openrouter/org/id`): the org OpenRouter names,
/// else told by the model's name (`claude-…` is Anthropic's whichever provider serves it).
pub fn vendor_of(spec: &str) -> String {
    let parts: Vec<&str> = spec.split('/').collect();
    let id = parts.last().copied().unwrap_or("").to_lowercase();
    let by_name = [("claude", "anthropic"), ("gpt", "openai"), ("o1", "openai"), ("o3", "openai"), ("o4", "openai"), ("codex", "openai"), ("gemini", "google"), ("gemma", "google"), ("grok", "x-ai"), ("llama", "meta-llama"), ("mistral", "mistralai"), ("deepseek", "deepseek"), ("qwen", "qwen"), ("kimi", "moonshotai"), ("glm", "z-ai")];
    if let Some((_, v)) = by_name.iter().find(|(p, _)| id.starts_with(p)) {
        return v.to_string();
    }
    if parts.len() >= 3 {
        return parts[parts.len() - 2].to_lowercase();
    }
    parts.first().map(|p| p.to_lowercase()).unwrap_or_default()
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
    fn vendors_are_told_apart() {
        assert_eq!(vendor_of("anthropic/claude-opus-5-5"), vendor_of("openrouter/anthropic/claude-opus-5-5"));
        assert_eq!(vendor_of("openrouter/google/gemini-3-pro"), "google");
        assert_eq!(vendor_of("openai/gpt-5.1"), "openai");
        assert_eq!(vendor_of("mock/m"), "mock");
    }

    #[test]
    fn proofs_are_read() {
        assert_eq!(parse_proof("[confirmed] It fails.\nEvidence: 3 of 500 runs failed with 8 threads."), Proof::Confirmed("3 of 500 runs failed with 8 threads.".into()));
        assert_eq!(parse_proof("**[confirmed]** It fails with 8 threads."), Proof::Confirmed("It fails with 8 threads.".into()));
        assert_eq!(parse_proof("Looked again.\n\n**[withdrawn]**\nThe test spawns its own workers."), Proof::Withdrawn("The test spawns its own workers.".into()));
        assert_eq!(parse_proof("Hm, not sure."), Proof::Unclear);
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
