//! Reviewing what an agent wrote in a chat (its report, or any message): comments on quoted
//! parts of it, asking a question or a change, saved as you read and sent as one message with
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
    pub text: String,
    pub ask: Ask,
    pub state: State,
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
    pub fn drafts(&self) -> Vec<&Comment> {
        self.comments.iter().filter(|c| c.state == State::Draft).collect()
    }

    /// Keep a comment being written: a new one, or the saved one it changes. Empty text
    /// deletes it. Its id.
    pub fn keep(&mut self, w: &Writing) -> Option<u64> {
        if w.text.trim().is_empty() {
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
        *c = Comment { id, entry: w.entry, quote: w.quote.clone(), label: w.label.clone(), opening: w.opening.clone(), text: w.text.trim().to_string(), ask: w.ask, state: State::Draft };
        Some(id)
    }
}

/// The opening of a message, to say which one a comment is about: its first line, cut short.
pub fn opening_of(text: &str) -> String {
    let first = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let first = first.trim_start_matches('#').replace("**", "");
    let first = first.trim();
    if first.chars().count() > 60 { format!("{}…", first.chars().take(60).collect::<String>().trim_end()) } else { first.to_string() }
}

/// The message an agent gets for these comments, and what was typed with them (`also`).
pub fn prompt(comments: &[&Comment], also: &str) -> String {
    let mut out = String::from("Review comments on what you wrote.");
    let section = |out: &mut String, ask: Ask, title: &str, how: &str| {
        let of: Vec<&&Comment> = comments.iter().filter(|c| c.ask == ask).collect();
        if of.is_empty() {
            return;
        }
        out.push_str(&format!("\n\n## {title}\n{how}\n"));
        for c in of {
            let place = match (c.opening.is_empty(), c.label.is_empty()) {
                (false, false) => format!("In your message that starts \"{}\", {}:", c.opening, c.label),
                (false, true) => format!("In your message that starts \"{}\":", c.opening),
                (true, false) => format!("{}:", c.label),
                (true, true) => "On this:".to_string(),
            };
            out.push_str(&format!("\n{place}\n"));
            for l in c.quote.lines() {
                out.push_str(&format!("> {}\n", l.trim_end()));
            }
            out.push_str(c.text.trim());
            out.push('\n');
        }
    };
    section(&mut out, Ask::Question, "Questions", "Answer each, quoting it. Change nothing for these.");
    section(&mut out, Ask::Fix, "Changes asked for", "Make these changes, then say what you did for each.");
    let also = also.trim();
    if !also.is_empty() {
        out.push_str(&format!("\n\n## Also\n\n{also}\n"));
    }
    out
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
    let o = crate::editor::BUILTIN;
    ed.define_command("chat-ask", "Ask a question about the selection in what the agent wrote (or the line at the cursor): written in the composer, saved to the chat's review.", o, |ed, _| with_chat(ed, |c, ed| c.start_comment(ed, Ask::Question)));
    ed.define_command("chat-comment", "Ask for a change about the selection in what the agent wrote (or the line at the cursor): written in the composer, saved to the chat's review.", o, |ed, _| with_chat(ed, |c, ed| c.start_comment(ed, Ask::Fix)));
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
    ed.define_command("chat-comment-cycle-kind", "Whether the comment being written asks a question or for a change (param: \"question\" or \"fix\").", o, |ed, a| {
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let p = prompt(&[&q, &f], "Then run the tests again.");
        assert_eq!(
            p,
            "Review comments on what you wrote.\n\n## Questions\nAnswer each, quoting it. Change nothing for these.\n\nIn your message that starts \"Done: the retry change is sound\", Changed 1:\n> This is the likely cause\n> of the flake.\nDid you reproduce it?\n\n\n## Changes asked for\nMake these changes, then say what you did for each.\n\nNeeds you 2:\n> Its Display output changed.\nUpdate the dashboards' parser too.\n\n\n## Also\n\nThen run the tests again.\n"
        );
        // Nothing typed: no "Also".
        assert!(!prompt(&[&q], "  ").contains("## Also"));
    }

    #[test]
    fn openings_are_short() {
        assert_eq!(opening_of("\n## Done: the retry change is sound, but `retry_total` is now counted twice and needs a fix."), "Done: the retry change is sound, but `retry_total` is now co…");
        assert_eq!(opening_of("**Blocked:** no access."), "Blocked: no access.");
    }
}
