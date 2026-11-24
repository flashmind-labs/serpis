//! Reviewing what an agent wrote in a chat (its report, or any message): comments on quoted
//! parts of it, asking a question or commenting, saved as you read and sent as one message with
//! whatever you type. Kept between runs like a diff page's review (`diff::review`): drafts and
//! the comment being written, one file per chat session. No UI in here (the chat draws it).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use crate::diff::review::{Ask, State};

/// A comment on part of an agent's message.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Comment {
    pub id: u64,
    /// The transcript entry it is about (found again by `quote` if entries moved).
    pub entry: usize,
    /// The text it is about, as selected.
    pub quote: String,
    /// Where that is: `Changed 1`, `Needs you 3`, `Summary`; empty outside a section.
    #[serde(default)]
    pub label: String,
    /// How the message it is about starts, so the agent knows which one.
    #[serde(default)]
    pub opening: String,
    /// What you wrote: the question or comment, or your note on a challenge's finding.
    pub text: String,
    pub ask: Ask,
    pub state: State,
    /// A finding of an agent that challenged the report ([`crate::chat_challenge`]), placed
    /// here as a draft: `text` is then your note on it (may be empty).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub challenge: Option<Finding>,
}

/// What a challenger found wrong with a claim.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    /// It ran something that shows it (else it only read the code).
    pub confirmed: bool,
    /// What is wrong.
    pub text: String,
    /// What it ran and what that showed, or what it read.
    #[serde(default)]
    pub evidence: String,
    /// Its challenger was asked to prove it (Prove it), and hasn't answered yet.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub proving: bool,
    /// Asked to prove it, the challenger took it back: why. It isn't sent (Keep anyway undoes it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub withdrawn: Option<String>,
}

impl Finding {
    pub fn verdict(&self) -> &'static str {
        if self.confirmed { "confirmed" } else { "plausible" }
    }
}

/// A comment being written in the composer (`id`: the saved one being changed).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Writing {
    pub id: Option<u64>,
    pub entry: usize,
    pub quote: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub opening: String,
    pub ask: Ask,
    pub text: String,
}

/// What is kept of a chat's review.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    #[serde(default)]
    pub comments: Vec<Comment>,
    #[serde(default)]
    pub writing: Option<Writing>,
    /// What was typed in the composer before the comment being written, to give back after.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aside: Option<String>,
    #[serde(default)]
    pub next_id: u64,
}

impl Saved {
    /// What goes with your next message: comments not sent yet (a finding its challenger
    /// withdrew left out).
    pub fn drafts(&self) -> Vec<&Comment> {
        self.comments.iter().filter(|c| c.state == State::Draft && !c.challenge.as_ref().is_some_and(|f| f.withdrawn.is_some())).collect()
    }

    /// Keep a comment being written: a new one, or the saved one it changes. Empty text
    /// deletes it. Its id.
    pub fn keep(&mut self, w: &Writing) -> Option<u64> {
        let finding = w.id.and_then(|id| self.comments.iter().find(|c| c.id == id)).and_then(|c| c.challenge.clone());
        if w.text.trim().is_empty() && finding.is_none() {
            if let Some(id) = w.id {
                self.comments.retain(|c| c.id != id);
            }
            return None;
        }
        let id = match w.id.filter(|id| self.comments.iter().any(|c| c.id == *id)) {
            Some(id) => id,
            None => {
                self.next_id = self.next_id.max(self.comments.iter().map(|c| c.id).max().unwrap_or(0)) + 1;
                self.comments.push(Comment { id: self.next_id, ..Default::default() });
                self.next_id
            }
        };
        let c = self.comments.iter_mut().find(|c| c.id == id)?;
        *c = Comment { id, entry: w.entry, quote: w.quote.clone(), label: w.label.clone(), opening: w.opening.clone(), text: w.text.trim().to_string(), ask: w.ask, state: State::Draft, challenge: finding };
        Some(id)
    }

    /// Add a challenger's findings as drafts. Their ids.
    pub fn add_findings(&mut self, found: Vec<Comment>) -> Vec<u64> {
        let mut ids = Vec::new();
        for mut c in found {
            self.next_id = self.next_id.max(self.comments.iter().map(|c| c.id).max().unwrap_or(0)) + 1;
            c.id = self.next_id;
            c.state = State::Draft;
            ids.push(c.id);
            self.comments.push(c);
        }
        ids
    }
}

/// The opening of a message, to say which one a comment is about: its first line, cut short.
pub fn opening_of(text: &str) -> String {
    let first = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let first = first.trim_start_matches('#').replace("**", "");
    let first = first.trim();
    if first.chars().count() > 60 { format!("{}…", first.chars().take(60).collect::<String>().trim_end()) } else { first.to_string() }
}

/// What a review message says besides the comments (the chat-review-*-instructions settings).
#[derive(Debug, Clone, Default)]
pub struct Framing {
    /// How to reply, said first (empty: nothing).
    pub reply: String,
    /// What to do about questions.
    pub question: String,
    /// What to do about comments.
    pub fix: String,
}

/// chat-review-reply-instructions' default.
pub const REPLY_INSTRUCTIONS: &str = "Reply with a report, as after any task. Each comment acted on is a bullet under `## Changed`. Each answer comes after the report's first line, quoting what it answers. A place such as item 2 under \"Needs you\" is in your last report: don't use it as a heading.";
/// chat-review-question-instructions' default.
pub const QUESTION_INSTRUCTIONS: &str = "Answer each, quoting it. Change nothing for these.";
/// chat-review-fix-instructions' default.
pub const FIX_INSTRUCTIONS: &str = "Address these comments. When one is unclear or wrong, say so instead of guessing.";

/// What a review message says, from the settings: how to reply only when the agent writes
/// reports (chat-report-skill).
pub fn framing(ed: &crate::editor::Editor) -> Framing {
    let var = |name: &str| ed.var_str(name).unwrap_or_default();
    Framing {
        reply: if ed.var_bool("chat-report-skill") { var("chat-review-reply-instructions") } else { String::new() },
        question: var("chat-review-question-instructions"),
        fix: var("chat-review-fix-instructions"),
    }
}

/// A comment's place in words, so it reads as a place in the message and not as a name to
/// reuse: `Needs you 2` is "item 2 under "Needs you"", a paragraph's `Summary` "under
/// "Summary"", `Headline` "its first line".
fn place_words(label: &str) -> String {
    let label = label.trim();
    if label.is_empty() {
        String::new()
    } else if label.eq_ignore_ascii_case("headline") {
        "its first line".into()
    } else if let Some((title, n)) = label.rsplit_once(' ').filter(|(_, n)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())) {
        format!("item {n} under \"{title}\"")
    } else {
        format!("under \"{label}\"")
    }
}

/// The label `place_words` wrote (as it is, when it is one sent before it did).
fn label_of(words: &str) -> String {
    let w = words.trim();
    let lower = w.to_lowercase();
    let quoted = |s: &str| s.strip_prefix('"').and_then(|s| s.strip_suffix('"')).map(str::to_string);
    if lower == "its first line" {
        return "Headline".into();
    }
    if lower.starts_with("item ")
        && let Some((n, title)) = w[5..].split_once(" under ")
        && let Some(title) = quoted(title)
    {
        return format!("{title} {n}");
    }
    if lower.starts_with("under ")
        && let Some(title) = quoted(&w[6..])
    {
        return title;
    }
    w.to_string()
}

/// The message an agent gets for these comments, and what was typed with them (`also`).
pub fn prompt(comments: &[&Comment], also: &str, f: &Framing) -> String {
    let mut out = String::from("Review comments on what you wrote.");
    if !f.reply.trim().is_empty() {
        out.push_str(&format!("\n\n{}", f.reply.trim()));
    }
    let place_of = |c: &Comment| {
        let at = place_words(&c.label);
        match (c.opening.is_empty(), at.is_empty()) {
            (false, false) => format!("In your message that starts \"{}\", {at}:", c.opening),
            (false, true) => format!("In your message that starts \"{}\":", c.opening),
            (true, false) => format!("{}{}:", at[..1].to_uppercase(), &at[1..]),
            (true, true) => "On this:".to_string(),
        }
    };
    let challenged: Vec<&&Comment> = comments.iter().filter(|c| c.challenge.is_some()).collect();
    if !challenged.is_empty() {
        out.push_str(&format!("\n\n## {CHALLENGED}\nAnother agent checked your report and found these. Answer each, quoting it: fix it, or show why it is wrong.\n"));
        for c in challenged {
            let Some(f) = &c.challenge else { continue };
            out.push_str(&format!("\n{}\n", place_of(c)));
            for l in c.quote.lines() {
                out.push_str(&format!("> {}\n", l.trim_end()));
            }
            out.push_str(&format!("Challenge ({}): {}\n", f.verdict(), f.text.trim()));
            if !f.evidence.trim().is_empty() {
                out.push_str(&format!("Evidence: {}\n", f.evidence.trim()));
            }
            if !c.text.trim().is_empty() {
                out.push_str(&format!("My note: {}\n", c.text.trim()));
            }
        }
    }
    let section = |out: &mut String, ask: Ask, title: &str, how: &str| {
        let of: Vec<&&Comment> = comments.iter().filter(|c| c.ask == ask && c.challenge.is_none()).collect();
        if of.is_empty() {
            return;
        }
        out.push_str(&format!("\n\n## {title}\n"));
        if !how.trim().is_empty() {
            out.push_str(&format!("{}\n", how.trim()));
        }
        for c in of {
            out.push_str(&format!("\n{}\n", place_of(c)));
            for l in c.quote.lines() {
                out.push_str(&format!("> {}\n", l.trim_end()));
            }
            out.push_str(c.text.trim());
            out.push('\n');
        }
    };
    section(&mut out, Ask::Question, "Questions", &f.question);
    section(&mut out, Ask::Fix, "Comments", &f.fix);
    let also = also.trim();
    if !also.is_empty() {
        out.push_str(&format!("\n\n## Also\n\n{also}\n"));
    }
    out
}

/// The heading challenges go under in a review message.
pub const CHALLENGED: &str = "Challenged by another agent";

/// A review message as sent, read back from the transcript: what each comment asked, where and
/// about what. A chat's review (of what the agent wrote) or a diff page's (of code).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sent {
    /// A diff page's review: what it compared (`main → the worktree`). None: a chat's.
    pub compared: Option<String>,
    pub items: Vec<SentItem>,
    /// What was typed with it.
    pub also: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SentItem {
    pub ask: Ask,
    /// Where: `Changed 1`, `Needs you 3` (a chat's), or `src/a.rs:12` with its notes (a diff's).
    pub place: String,
    /// What it is about: the quoted text, or the code.
    pub quote: Vec<String>,
    /// The quote is code (a diff's).
    pub code: bool,
    pub text: String,
    /// A challenge's finding (`text` is then the finding, and `note` what you added).
    pub challenge: Option<Finding>,
    pub note: String,
}

/// `text` read as a review message serpis sent (`prompt` here, or a diff page's): None for
/// anything else.
pub fn parse_sent(text: &str) -> Option<Sent> {
    let text = text.trim_start();
    let compared = if text.starts_with("Review comments on what you wrote.") {
        None
    } else {
        let rest = text.strip_prefix("Review comments on ")?;
        let first = rest.lines().next()?;
        if !first.contains("Line numbers are") {
            return None;
        }
        Some(first.split_once(" in ")?.0.to_string())
    };
    let diff = compared.is_some();
    let mut out = Sent { compared, ..Default::default() };
    let mut ask: Option<Ask> = None;
    let mut challenges = false;
    let mut also: Option<Vec<&str>> = None;
    let mut item: Option<SentItem> = None;
    let mut in_code = false;
    let mut text_lines: Vec<&str> = Vec::new();
    let finish = |out: &mut Sent, item: &mut Option<SentItem>, text_lines: &mut Vec<&str>| {
        if let Some(mut it) = item.take() {
            it.text = text_lines.join("\n").trim().to_string();
            if let Some(f) = it.challenge.as_mut() {
                // "Challenge (confirmed): …", "Evidence: …", "My note: …".
                let (mut finding, mut evidence, mut note) = (Vec::new(), Vec::new(), Vec::new());
                let mut into = 0;
                for l in it.text.lines() {
                    if let Some(rest) = l.strip_prefix("Challenge (") {
                        let (verdict, rest) = rest.split_once("): ").unwrap_or(("", rest));
                        f.confirmed = verdict == "confirmed";
                        finding.push(rest);
                        into = 0;
                    } else if let Some(rest) = l.strip_prefix("Evidence: ") {
                        evidence.push(rest);
                        into = 1;
                    } else if let Some(rest) = l.strip_prefix("My note: ") {
                        note.push(rest);
                        into = 2;
                    } else {
                        [&mut finding, &mut evidence, &mut note][into].push(l);
                    }
                }
                f.text = finding.join("\n").trim().to_string();
                f.evidence = evidence.join("\n").trim().to_string();
                it.note = note.join("\n").trim().to_string();
                it.text = f.text.clone();
            }
            out.items.push(it);
        }
        text_lines.clear();
    };
    for line in text.lines().skip(1) {
        if let Some(a) = also.as_mut() {
            a.push(line);
            continue;
        }
        if in_code {
            if line.starts_with("```") {
                in_code = false;
            } else if let Some(it) = item.as_mut() {
                it.quote.push(line.to_string());
            }
            continue;
        }
        if let Some(h) = line.strip_prefix("## ") {
            finish(&mut out, &mut item, &mut text_lines);
            challenges = h.trim() == CHALLENGED;
            match h.trim() {
                CHALLENGED => ask = Some(Ask::Fix),
                "Questions" => ask = Some(Ask::Question),
                "Comments" | "Changes asked for" | "Fix" => ask = Some(Ask::Fix),
                "Also" => also = Some(Vec::new()),
                _ => ask = None,
            }
            continue;
        }
        let Some(kind) = ask else { continue };
        // Where a comment starts: a chat's place line ends with a colon; a diff's is `path:line`.
        let place = if diff {
            (line.starts_with('`') && !line.starts_with("``")).then(|| line.to_string()).filter(|l| l[1..].contains('`'))
        } else {
            // Its place in words ("item 2 under "Needs you"") or, sent before, as a label ("Needs you 2").
            let words = line.strip_suffix(':').is_some_and(|l| {
                let l = l.to_lowercase();
                l == "its first line" || (l.starts_with("item ") && l.contains(" under \"")) || l.starts_with("under \"")
            });
            (line.ends_with(':') && (words || line.starts_with("In your message that starts") || line == "On this:" || !line.contains(' ') || line.split(' ').next_back().is_some_and(|w| w.trim_end_matches(':').chars().all(|c| c.is_ascii_digit())))).then(|| {
                let l = line.trim_end_matches(':');
                match l.strip_prefix("In your message that starts \"").and_then(|r| r.rsplit_once("\", ")) {
                    Some((_, at)) => label_of(at),
                    None if l.starts_with("In your message that starts") || l == "On this" => String::new(),
                    None => label_of(l),
                }
            })
        };
        if let Some(place) = place {
            finish(&mut out, &mut item, &mut text_lines);
            item = Some(SentItem { ask: kind, place: place.replace('`', ""), code: diff, challenge: challenges.then(Finding::default), ..Default::default() });
            continue;
        }
        let Some(it) = item.as_mut() else { continue };
        if diff && line.starts_with("```") && it.quote.is_empty() && text_lines.is_empty() {
            in_code = true;
        } else if let Some(q) = line.strip_prefix("> ").filter(|_| !diff && text_lines.is_empty()) {
            it.quote.push(q.to_string());
        } else if line.starts_with("… ") && diff && text_lines.is_empty() {
            it.quote.push(line.to_string());
        } else {
            text_lines.push(line);
        }
    }
    finish(&mut out, &mut item, &mut text_lines);
    out.also = also.map(|a| a.join("\n").trim().to_string()).unwrap_or_default();
    (!out.items.is_empty() || !out.also.is_empty()).then_some(out)
}

/// Where a chat's review is kept: one file per session (and machine), under the config folder.
fn path_of(machine: Option<&str>, session: &str) -> PathBuf {
    let safe = |s: &str| s.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '.' { c } else { '_' }).collect::<String>();
    crate::config_dir().join("reviews").join("chats").join(format!("{}{}.json", machine.map(|m| format!("{}@", safe(m))).unwrap_or_default(), safe(session)))
}

pub fn load(machine: Option<&str>, session: &str) -> Saved {
    let mut saved: Saved = std::fs::read_to_string(path_of(machine, session)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    saved.comments.retain(|c| c.state == State::Draft);
    saved
}

pub fn save(machine: Option<&str>, session: &str, saved: &Saved) -> anyhow::Result<()> {
    let path = path_of(machine, session);
    let mut saved = saved.clone();
    saved.comments.retain(|c| c.state == State::Draft);
    // A review with nothing to keep leaves no file behind.
    if saved.comments.is_empty() && saved.writing.is_none() {
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&saved)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Commands and keys: a selection in what the agent wrote takes `a` (ask) and `c` (comment); a
/// comment being written in the composer saves with RET, as the diff page's comments with C-RET.
pub fn install(ed: &mut crate::editor::Editor) {
    use crate::commands::with_chat;
    use serde_json::json;
    let o = crate::editor::BUILTIN;
    ed.defvar("chat-review-reply-instructions", json!(REPLY_INSTRUCTIONS), "What a review sent to a chat's agent says first, about how to reply (only while chat-report-skill is on). Empty: nothing.");
    ed.defvar("chat-review-question-instructions", json!(QUESTION_INSTRUCTIONS), "What a review sent to a chat's agent says about questions.");
    ed.defvar("chat-review-fix-instructions", json!(FIX_INSTRUCTIONS), "What a review sent to a chat's agent says about comments.");
    ed.define_command("chat-ask", "Ask a question about the selection in what the agent wrote (or the line at the cursor): written in the composer, saved to the chat's review.", o, |ed, _| with_chat(ed, |c, ed| c.start_comment(ed, Ask::Question)));
    ed.define_command("chat-comment", "Comment on the selection in what the agent wrote (or the line at the cursor): written in the composer, saved to the chat's review.", o, |ed, _| with_chat(ed, |c, ed| c.start_comment(ed, Ask::Fix)));
    ed.define_command("chat-comment-save", "Save the comment being written to the chat's review, sent with your next message (emptied, a saved one is deleted).", o, |ed, _| with_chat(ed, |c, ed| c.save_comment(ed)));
    ed.define_command("chat-comment-send", "Send the comment being written now, on its own.", o, |ed, _| with_chat(ed, |c, ed| c.send_comment_now(ed)));
    ed.define_command("chat-comment-cancel", "Drop the comment being written (a saved one stays as it was).", o, |ed, _| with_chat(ed, |c, ed| c.cancel_comment(ed)));
    ed.define_command("chat-comment-escape", "In vim's insert state, back to normal; otherwise drop the comment being written.", o, |ed, _| {
        let id = ed.current_buffer();
        if crate::packages::vim::state_of(ed, id).is_some_and(|s| s == crate::packages::vim::State::Insert) {
            crate::packages::vim::set_state(ed, id, crate::packages::vim::State::Normal);
            return Ok(());
        }
        with_chat(ed, |c, ed| c.cancel_comment(ed))
    });
    ed.define_command("chat-comment-cycle-kind", "Whether the comment being written is a question or a comment (param: \"question\" or \"fix\").", o, |ed, a| {
        let set = a.str(0).and_then(Ask::parse);
        with_chat(ed, |c, _| c.cycle_comment_kind(set))
    });
    ed.define_command("chat-comment-edit", "Change a saved comment of the chat's review (param: its id).", o, |ed, a| {
        let id = a.params.first().and_then(Value::as_u64).ok_or_else(|| anyhow::anyhow!("chat-comment-edit: which comment (its id)?"))?;
        with_chat(ed, |c, ed| c.edit_comment(ed, id))
    });
    ed.define_command("chat-comment-delete", "Delete a saved comment of the chat's review (param: its id).", o, |ed, a| {
        let id = a.params.first().and_then(Value::as_u64).ok_or_else(|| anyhow::anyhow!("chat-comment-delete: which comment (its id)?"))?;
        with_chat(ed, |c, ed| c.delete_comment(ed, id))
    });
    ed.define_command("chat-review-discard", "Drop every comment of the chat's review not sent yet, after asking (param: true doesn't ask).", o, |ed, a| {
        if a.params.first().and_then(Value::as_bool) == Some(true) {
            return with_chat(ed, |c, ed| {
                c.discard_review(ed);
            });
        }
        let n = with_chat(ed, |c, _| c.review_json()["drafts"].as_u64().unwrap_or(0))?;
        if n == 0 {
            return Ok(());
        }
        ed.y_or_n(&format!("Discard {n} comment{} not sent? ", if n == 1 { "" } else { "s" }), |ed, yes| {
            if yes {
                let _ = with_chat(ed, |c, ed| c.discard_review(ed));
            }
        });
        Ok(())
    });
    for (k, c) in [("a", "chat-ask"), ("c", "chat-comment")] {
        ed.bind("chat-selection", k, c, o);
    }
    for (k, c) in [
        ("RET", "chat-comment-save"),
        ("C-RET", "chat-comment-save"),
        ("s-RET", "chat-comment-save"),
        ("C-c C-c", "chat-comment-save"),
        ("C-c C-s", "chat-comment-send"),
        ("C-c C-k", "chat-comment-cancel"),
        ("ESC", "chat-comment-escape"),
        ("M-k", "chat-comment-cycle-kind"),
    ] {
        ed.bind("chat-commenting", k, c, o);
    }
}

/// A comment as scripts see it.
pub fn to_json(c: &Comment) -> Value {
    serde_json::json!({
        "id": c.id,
        "entry": c.entry + 1,
        "quote": c.quote,
        "label": c.label,
        "text": c.text,
        "kind": c.ask.name(),
        "state": c.state.name(),
        "challenge": c.challenge.as_ref().map(|f| serde_json::json!({"verdict": f.verdict(), "finding": f.text, "evidence": f.evidence, "proving": f.proving, "withdrawn": f.withdrawn})),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn framing() -> Framing {
        Framing { reply: String::new(), question: "Answer each, quoting it.".into(), fix: "Address these comments.".into() }
    }

    fn writing(quote: &str, text: &str, ask: Ask) -> Writing {
        Writing { quote: quote.into(), label: "Changed 1".into(), opening: "Done: the retry change is sound".into(), text: text.into(), ask, ..Default::default() }
    }

    #[test]
    fn comments_are_kept_changed_and_deleted() {
        let mut s = Saved::default();
        let a = s.keep(&writing("This is the likely cause.", "Likely?", Ask::Question)).unwrap();
        let b = s.keep(&writing("Retries were synchronizing.", "Logs?", Ask::Question)).unwrap();
        assert_ne!(a, b);
        // Changing one keeps its id.
        assert_eq!(s.keep(&Writing { id: Some(a), ..writing("This is the likely cause.", "Did you reproduce it?", Ask::Question) }), Some(a));
        assert_eq!(s.comments.iter().find(|c| c.id == a).unwrap().text, "Did you reproduce it?");
        // Emptied, it goes.
        assert_eq!(s.keep(&Writing { id: Some(b), ..writing("", " ", Ask::Question) }), None);
        assert_eq!(s.drafts().len(), 1);
        // Ids aren't reused.
        let c = s.keep(&writing("x", "y", Ask::Fix)).unwrap();
        assert!(c > b);
    }

    #[test]
    fn the_message_quotes_each_comment_under_what_it_asks() {
        let q = Comment { quote: "This is the likely cause\nof the flake.".into(), label: "Changed 1".into(), opening: "Done: the retry change is sound".into(), text: "Did you reproduce it?".into(), ask: Ask::Question, ..Default::default() };
        let f = Comment { quote: "Its Display output changed.".into(), label: "Needs you 2".into(), text: "Update the dashboards' parser too.".into(), ask: Ask::Fix, ..Default::default() };
        let p = prompt(&[&q, &f], "Then run the tests again.", &framing());
        assert_eq!(
            p,
            "Review comments on what you wrote.\n\n## Questions\nAnswer each, quoting it.\n\nIn your message that starts \"Done: the retry change is sound\", item 1 under \"Changed\":\n> This is the likely cause\n> of the flake.\nDid you reproduce it?\n\n\n## Comments\nAddress these comments.\n\nItem 2 under \"Needs you\":\n> Its Display output changed.\nUpdate the dashboards' parser too.\n\n\n## Also\n\nThen run the tests again.\n"
        );
        // Nothing typed: no "Also".
        assert!(!prompt(&[&q], "  ", &framing()).contains("## Also"));
        // How to reply comes first; no instructions, no line for them.
        let f = Framing { reply: "Reply with a report.".into(), ..Default::default() };
        assert!(prompt(&[&q], "", &f).starts_with("Review comments on what you wrote.\n\nReply with a report.\n\n## Questions\n\nIn your message"), "{}", prompt(&[&q], "", &f));
    }

    #[test]
    fn places_read_as_places() {
        for (label, words) in [("Needs you 2", "item 2 under \"Needs you\""), ("Summary", "under \"Summary\""), ("Headline", "its first line"), ("", "")] {
            assert_eq!(place_words(label), words);
            assert_eq!(label_of(&words), label);
        }
        // Sent before they were words.
        assert_eq!(label_of("Needs you 2"), "Needs you 2");
    }

    #[test]
    fn sent_reviews_are_read_back() {
        let q = Comment { quote: "This is the likely cause\nof the flake.".into(), label: "Changed 1".into(), opening: "Done: the retry change".into(), text: "Did you reproduce it?\nAnd how?".into(), ask: Ask::Question, ..Default::default() };
        let f = Comment { quote: "Its output changed.".into(), label: "Needs you 2".into(), text: "Update the parser.".into(), ask: Ask::Fix, ..Default::default() };
        let o = Comment { quote: "Something.".into(), text: "Hm?".into(), ask: Ask::Question, ..Default::default() };
        let p = prompt(&[&q, &f, &o], "Then run the tests.", &Framing { reply: "Reply with a report.".into(), ..framing() });
        let sent = parse_sent(&p).unwrap();
        assert_eq!(sent.compared, None);
        assert_eq!(sent.items.len(), 3);
        assert_eq!((sent.items[0].ask, sent.items[0].place.as_str(), sent.items[0].text.as_str()), (Ask::Question, "Changed 1", "Did you reproduce it?\nAnd how?"));
        assert_eq!(sent.items[0].quote, vec!["This is the likely cause", "of the flake."]);
        assert_eq!((sent.items[1].place.as_str(), sent.items[2].place.as_str()), ("", "Needs you 2"));
        assert_eq!(sent.items[2].ask, Ask::Fix);
        assert_eq!(sent.also, "Then run the tests.");
        // One sent before places were words.
        let old = "Review comments on what you wrote.\n\n## Changes asked for\nMake these changes, then say what you did for each.\n\nIn your message that starts \"Done: x\", Needs you 1:\n> Six clients.\nNo compression.\n\nChanged 2:\n> Jitter.\nWhy?\n";
        let sent = parse_sent(old).unwrap();
        assert_eq!(sent.items.iter().map(|i| (i.place.as_str(), i.text.as_str())).collect::<Vec<_>>(), vec![("Needs you 1", "No compression."), ("Changed 2", "Why?")]);
        // A diff page's.
        let diff = "Review comments on main → the worktree in /w. Line numbers are the current file's, except where a comment says it is on the old version.\n\n## Fix\nFix these.\n\n`src/a.rs:12-13`\n```rs\nlet x = 1;\nlet y = 2;\n```\nUse a constant.\n\n## Questions\nAnswer these.\n\n`src/a.rs:30` (on the old version)\nWhy run it here?\n";
        let sent = parse_sent(diff).unwrap();
        assert_eq!(sent.compared.as_deref(), Some("main → the worktree"));
        assert_eq!(sent.items.len(), 2);
        assert_eq!((sent.items[0].place.as_str(), sent.items[0].quote.clone(), sent.items[0].text.as_str(), sent.items[0].code), ("src/a.rs:12-13", vec!["let x = 1;".to_string(), "let y = 2;".to_string()], "Use a constant.", true));
        assert_eq!((sent.items[1].ask, sent.items[1].place.as_str(), sent.items[1].text.as_str()), (Ask::Question, "src/a.rs:30 (on the old version)", "Why run it here?"));
        assert!(parse_sent("Review comments on the plan, please").is_none());
        assert!(parse_sent("Hello").is_none());
    }

    #[test]
    fn challenges_go_first_and_are_read_back() {
        let finding = Finding { confirmed: true, text: "The race is still there.".into(), evidence: "8 workers: 3 of 500 failed.".into(), ..Default::default() };
        let ch = Comment { quote: "This is the likely cause.".into(), label: "Changed 1".into(), opening: "Done: retries".into(), text: "Agreed. Add the run to the test.".into(), ask: Ask::Fix, challenge: Some(finding.clone()), ..Default::default() };
        let bare = Comment { quote: "passed every time.".into(), label: "Checked 2".into(), ask: Ask::Fix, challenge: Some(Finding { confirmed: false, text: "One thread can't race.".into(), ..Default::default() }), ..Default::default() };
        let q = Comment { quote: "Added jitter.".into(), label: "Changed 2".into(), text: "Same as the old client?".into(), ask: Ask::Question, ..Default::default() };
        let p = prompt(&[&q, &ch, &bare], "", &framing());
        assert!(p.starts_with(&format!("Review comments on what you wrote.\n\n## {CHALLENGED}\n")), "{p}");
        assert!(p.contains("In your message that starts \"Done: retries\", item 1 under \"Changed\":\n> This is the likely cause.\nChallenge (confirmed): The race is still there.\nEvidence: 8 workers: 3 of 500 failed.\nMy note: Agreed. Add the run to the test.\n"), "{p}");
        assert!(p.contains("Item 2 under \"Checked\":\n> passed every time.\nChallenge (plausible): One thread can't race.\n\n"), "{p}");
        // Not repeated under Comments.
        assert!(!p.contains("## Comments"), "{p}");
        let sent = parse_sent(&p).unwrap();
        assert_eq!(sent.items.len(), 3);
        let (a, b, c) = (&sent.items[0], &sent.items[1], &sent.items[2]);
        assert_eq!((a.place.as_str(), a.challenge.as_ref(), a.note.as_str(), a.text.as_str()), ("Changed 1", Some(&finding), "Agreed. Add the run to the test.", "The race is still there."));
        assert_eq!((b.challenge.as_ref().map(|f| f.confirmed), b.note.as_str()), (Some(false), ""));
        assert_eq!((c.ask, c.challenge.is_none()), (Ask::Question, true));
        // A note emptied keeps the finding; a plain comment emptied goes.
        let mut s = Saved::default();
        let ids = s.add_findings(vec![bare.clone()]);
        assert_eq!(s.keep(&Writing { id: Some(ids[0]), quote: bare.quote.clone(), ask: Ask::Fix, ..Default::default() }), Some(ids[0]));
        assert!(s.comments[0].challenge.is_some());
    }

    #[test]
    fn openings_are_short() {
        assert_eq!(opening_of("\n## Done: the retry change is sound, but `retry_total` is now counted twice and needs a fix."), "Done: the retry change is sound, but `retry_total` is now co…");
        assert_eq!(opening_of("**Blocked:** no access."), "Blocked: no access.");
    }
}
