//! Review comments: what they anchor to, how they follow the code as it changes, what is sent to
//! an agent, and where they are kept between runs. No UI in here.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Which {
    Old,
    #[default]
    New,
}

impl Which {
    pub fn name(self) -> &'static str {
        match self {
            Which::Old => "old",
            Which::New => "new",
        }
    }
}

/// What the author wants from the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Ask {
    /// Change the code.
    #[default]
    Fix,
    /// Answer, and leave the code alone.
    Question,
}

impl Ask {
    pub fn name(self) -> &'static str {
        match self {
            Ask::Fix => "fix",
            Ask::Question => "question",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "fix" => Some(Ask::Fix),
            "question" => Some(Ask::Question),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Waiting to be sent.
    #[default]
    Draft,
    Sent,
    Resolved,
}

impl State {
    pub fn name(self) -> &'static str {
        match self {
            State::Draft => "draft",
            State::Sent => "sent",
            State::Resolved => "resolved",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Comment {
    pub id: u64,
    pub path: String,
    pub side: Which,
    /// The lines it is about, 1-based and inclusive.
    pub start: u32,
    pub end: u32,
    pub text: String,
    pub ask: Ask,
    pub state: State,
    /// Those lines when it was written: how it finds them again, and what the agent is shown.
    pub code: Vec<String>,
    /// The lines are gone: it shows above the file, and the agent gets them as they were.
    #[serde(default)]
    pub outdated: bool,
}

impl Comment {
    /// `src/a.rs:12` or `src/a.rs:12-14`.
    pub fn place(&self) -> String {
        if self.end > self.start { format!("{}:{}-{}", self.path, self.start, self.end) } else { format!("{}:{}", self.path, self.start) }
    }

    /// Find its lines in `lines`, the file now: where they were, else the nearest place they
    /// occur, else it is outdated and stays where it was.
    pub fn reanchor(&mut self, lines: &[String]) {
        let n = self.code.len();
        // Added by a script for a file not showing then: only its place to go by.
        if n == 0 {
            self.outdated = self.end as usize > lines.len();
            return;
        }
        let matches_at = |at: usize| at + n <= lines.len() && lines[at..at + n] == self.code[..];
        let was = (self.start as usize).saturating_sub(1);
        if matches_at(was) {
            self.outdated = false;
            return;
        }
        let found = (0..lines.len().saturating_sub(n - 1)).filter(|&at| matches_at(at)).min_by_key(|&at| at.abs_diff(was));
        match found {
            Some(at) => {
                self.start = at as u32 + 1;
                self.end = (at + n) as u32;
                self.outdated = false;
            }
            None => self.outdated = true,
        }
    }
}

/// What the prompt says besides the comments.
#[derive(Debug, Clone, Default)]
pub struct Framing {
    /// The worktree, for the agent to find the files.
    pub root: String,
    /// What was compared: `main → the worktree`.
    pub compared: String,
    /// What to do about fixes.
    pub fix: String,
    /// What to do about questions.
    pub question: String,
    /// Lines of code quoted per comment at most (0: none).
    pub quote_lines: usize,
}

fn fence(code: &[String]) -> String {
    let longest = code.iter().map(|l| l.chars().take_while(|c| *c == '`').count()).max().unwrap_or(0);
    "`".repeat(longest.max(2) + 1)
}

fn language(path: &str) -> &str {
    path.rsplit('/').next().and_then(|n| n.rsplit_once('.')).map_or("", |(_, e)| e)
}

/// The message an agent gets for these comments.
pub fn prompt(comments: &[&Comment], f: &Framing) -> String {
    let mut out = format!("Review comments on {} in {}.", f.compared, f.root);
    out.push_str(" Line numbers are the current file's, except where a comment says it is on the old version.\n");
    let section = |out: &mut String, ask: Ask, title: &str, how: &str| {
        let of: Vec<&&Comment> = comments.iter().filter(|c| c.ask == ask).collect();
        if of.is_empty() {
            return;
        }
        out.push_str(&format!("\n## {title}\n"));
        if !how.is_empty() {
            out.push_str(how);
            out.push('\n');
        }
        for c in of {
            let mut notes = Vec::new();
            if c.side == Which::Old {
                notes.push("on the old version");
            }
            if c.outdated {
                notes.push("this code has changed since");
            }
            let notes = if notes.is_empty() { String::new() } else { format!(" ({})", notes.join(", ")) };
            out.push_str(&format!("\n`{}`{notes}\n", c.place()));
            if f.quote_lines > 0 && !c.code.is_empty() {
                let shown = &c.code[..c.code.len().min(f.quote_lines)];
                let marks = fence(shown);
                out.push_str(&format!("{marks}{}\n{}\n", language(&c.path), shown.join("\n")));
                if shown.len() < c.code.len() {
                    out.push_str(&format!("… {} more lines\n", c.code.len() - shown.len()));
                }
                out.push_str(&format!("{marks}\n"));
            }
            out.push_str(c.text.trim());
            out.push('\n');
        }
    };
    section(&mut out, Ask::Fix, "Fix", &f.fix);
    section(&mut out, Ask::Question, "Questions", &f.question);
    out
}

/// A comment still being written, kept so quitting doesn't lose it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Writing {
    /// The comment it changes, when it isn't a new one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    pub path: String,
    pub side: Which,
    pub start: u32,
    pub end: u32,
    pub ask: Ask,
    pub text: String,
}

/// A worktree's review: on disk, only what hasn't been sent yet, so it can be picked up where it
/// was left. What was sent is the agent's now, and shows until the page closes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Saved {
    #[serde(default)]
    pub comments: Vec<Comment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub writing: Option<Writing>,
    /// What was being compared and which file was showing, for coming back to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<serpis_proto::wire::DiffSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default)]
    pub next_id: u64,
    /// Files marked as viewed, with what they looked like then: changing clears it.
    #[serde(default)]
    pub viewed: std::collections::HashMap<String, String>,
}

/// Where a worktree's review is kept: one file per machine and worktree, under the config folder.
fn path_of(machine: Option<&str>, root: &str) -> PathBuf {
    let safe = |s: &str| s.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '.' { c } else { '_' }).collect::<String>();
    crate::config_dir().join("reviews").join(format!("{}{}.json", machine.map(|m| format!("{}@", safe(m))).unwrap_or_default(), safe(root)))
}

pub fn load(machine: Option<&str>, root: &str) -> Saved {
    let mut saved: Saved = std::fs::read_to_string(path_of(machine, root)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    saved.comments.retain(|c| c.state == State::Draft);
    saved
}

pub fn save(machine: Option<&str>, root: &str, saved: &Saved) -> anyhow::Result<()> {
    let path = path_of(machine, root);
    let mut saved = saved.clone();
    saved.comments.retain(|c| c.state == State::Draft);
    // A review with nothing to keep leaves no file behind.
    if saved.comments.is_empty() && saved.viewed.is_empty() && saved.writing.is_none() {
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

/// A comment as scripts see it.
pub fn to_json(c: &Comment) -> Value {
    serde_json::json!({
        "id": c.id,
        "path": c.path,
        "side": c.side.name(),
        "line": c.start,
        "end_line": c.end,
        "text": c.text,
        "kind": c.ask.name(),
        "state": c.state.name(),
        "code": c.code,
        "outdated": c.outdated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &[&str]) -> Vec<String> {
        s.iter().map(|l| l.to_string()).collect()
    }

    fn comment(start: u32, code: &[&str]) -> Comment {
        Comment { id: 1, path: "src/a.rs".into(), start, end: start + code.len() as u32 - 1, code: lines(code), text: "why?".into(), ..Default::default() }
    }

    #[test]
    fn comments_follow_their_code() {
        let mut c = comment(2, &["two", "three"]);
        c.reanchor(&lines(&["one", "two", "three"]));
        assert_eq!((c.start, c.end, c.outdated), (2, 3, false));
        c.reanchor(&lines(&["new", "new", "one", "two", "three"]));
        assert_eq!((c.start, c.end, c.outdated), (4, 5, false));
        // Of two copies, the nearer.
        c.reanchor(&lines(&["two", "three", "x", "x", "x", "two", "three"]));
        assert_eq!(c.start, 6);
        // Gone: it stays where it was, marked, and comes back with the code.
        c.reanchor(&lines(&["other"]));
        assert_eq!((c.start, c.outdated), (6, true));
        c.reanchor(&lines(&["two", "three"]));
        assert_eq!((c.start, c.outdated), (1, false));
    }

    #[test]
    fn the_prompt_separates_fixes_from_questions_and_quotes_the_code() {
        let mut fix = comment(12, &["let x = 1;", "let y = 2;"]);
        fix.text = "  Use a constant.  ".into();
        let mut ask = comment(30, &["run();"]);
        ask.ask = Ask::Question;
        ask.text = "Why run it here?".into();
        ask.side = Which::Old;
        let f = Framing { root: "/w".into(), compared: "main → the worktree".into(), fix: "Fix these.".into(), question: "Answer these.".into(), quote_lines: 20 };
        let p = prompt(&[&fix, &ask], &f);
        assert!(p.starts_with("Review comments on main → the worktree in /w."), "{p}");
        let (a, b) = (p.find("## Fix").unwrap(), p.find("## Questions").unwrap());
        assert!(a < b, "{p}");
        assert!(p.contains("`src/a.rs:12-13`\n```rs\nlet x = 1;\nlet y = 2;\n```\nUse a constant.\n"), "{p}");
        assert!(p.contains("`src/a.rs:30` (on the old version)"), "{p}");
        assert!(p.contains("Answer these."), "{p}");
        // Only what there is.
        let only = prompt(&[&ask], &f);
        assert!(!only.contains("## Fix"), "{only}");
    }

    #[test]
    fn quotes_are_bounded_and_fenced_around_backticks() {
        let c = comment(1, &["```", "a", "b", "c"]);
        let f = Framing { quote_lines: 2, ..Default::default() };
        let p = prompt(&[&c], &f);
        assert!(p.contains("````rs\n```\na\n… 2 more lines\n````\n"), "{p}");
        let none = prompt(&[&c], &Framing::default());
        assert!(!none.contains("```"), "{none}");
    }
}
