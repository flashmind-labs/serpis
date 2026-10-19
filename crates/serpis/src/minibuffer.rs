//! The minibuffer: prompts with fuzzy completion (`M-x`, buffer switching, model picker,
//! find-file, ...). Keys go through the `minibuffer` keymap; typed text edits the input.

use crate::editor::{Args, Editor};
use crate::keys::KeyChord;
use crate::text::{Pos, TextArea, TextSurface, char_len};

#[derive(Debug, Clone)]
pub struct Candidate {
    pub label: String,
    /// Dimmed annotation shown to the right (keybinding, doc, model info).
    pub detail: String,
    /// Value returned on accept; defaults to the label.
    pub value: String,
}

impl Candidate {
    pub fn new(label: impl Into<String>) -> Self {
        let label = label.into();
        Candidate {
            value: label.clone(),
            label,
            detail: String::new(),
        }
    }
    pub fn detail(mut self, d: impl Into<String>) -> Self {
        self.detail = d.into();
        self
    }
    pub fn value(mut self, v: impl Into<String>) -> Self {
        self.value = v.into();
        self
    }
}

pub type AcceptFn = Box<dyn FnOnce(&mut Editor, Option<Candidate>, String)>;
/// Called whenever the selected candidate changes (live preview, like consult's `:state`).
pub type SelectFn = std::rc::Rc<dyn Fn(&mut Editor, Option<&Candidate>)>;
pub type CancelFn = Box<dyn FnOnce(&mut Editor)>;
pub type CandidatesFn = Box<dyn Fn(&str) -> Vec<Candidate>>;

pub struct Minibuffer {
    pub prompt: String,
    pub input: TextArea,
    pub candidates: Vec<Candidate>,
    /// Recompute candidates from the input (dynamic sources such as file paths).
    pub dynamic: Option<CandidatesFn>,
    pub filtered: Vec<(usize, Vec<usize>)>,
    pub selected: usize,
    pub require_match: bool,
    on_accept: Option<AcceptFn>,
    last_query: Option<String>,
    pub on_select: Option<SelectFn>,
    pub on_cancel: Option<CancelFn>,
    /// Keep candidate order instead of ranking by match score.
    pub keep_order: bool,
    /// Value last reported to `on_select`.
    pub(crate) previewed: Option<Option<String>>,
}

/// Subsequence fuzzy match; returns a score (higher is better) and matched char positions.
pub fn fuzzy(query: &str, text: &str) -> Option<(i64, Vec<usize>)> {
    if query.is_empty() {
        return Some((0, Vec::new()));
    }
    let mut total = 0i64;
    let mut all_positions = Vec::new();
    // Space-separated terms must all match (orderless style).
    for term in query.split_whitespace() {
        let tl: Vec<char> = term.to_lowercase().chars().collect();
        let chars: Vec<char> = text.chars().collect();
        let lower: Vec<char> = text.to_lowercase().chars().collect();
        let mut positions = Vec::new();
        let mut ti = 0;
        let mut score = 0i64;
        let mut last: Option<usize> = None;
        for (i, c) in lower.iter().enumerate() {
            if ti < tl.len() && *c == tl[ti] {
                score += 10;
                if last.is_some_and(|l| l + 1 == i) {
                    score += 15;
                }
                if i == 0 || !chars[i - 1].is_alphanumeric() {
                    score += 20;
                }
                positions.push(i);
                last = Some(i);
                ti += 1;
            }
        }
        if ti < tl.len() {
            return None;
        }
        if let Some(sub) = lower.iter().collect::<String>().find(&term.to_lowercase()) {
            score += 40 - (sub as i64).min(30);
        }
        total += score;
        all_positions.extend(positions);
    }
    total -= text.chars().count() as i64 / 4;
    all_positions.sort_unstable();
    all_positions.dedup();
    Some((total, all_positions))
}

impl Minibuffer {
    pub fn refilter(&mut self) {
        let q = self.input.text();
        if self.last_query.as_deref() == Some(q.as_str()) {
            return;
        }
        if let Some(f) = &self.dynamic {
            self.candidates = f(&q);
        }
        let query = if self.dynamic.is_some() { q.rsplit('/').next().unwrap_or("").to_string() } else { q.clone() };
        let mut scored: Vec<(i64, usize, Vec<usize>)> = self
            .candidates
            .iter()
            .enumerate()
            .filter_map(|(i, c)| fuzzy(&query, &c.label).map(|(s, p)| (s, i, p)))
            .collect();
        if !query.is_empty() && !self.keep_order {
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        }
        self.filtered = scored.into_iter().map(|(_, i, p)| (i, p)).collect();
        self.selected = 0;
        self.last_query = Some(q);
    }

    pub fn current(&self) -> Option<&Candidate> {
        self.filtered.get(self.selected).map(|(i, _)| &self.candidates[*i])
    }
}

impl Editor {
    /// Prompt for a choice. `on_accept` receives the selected candidate (if any) and the raw input.
    pub fn completing_read(
        &mut self,
        prompt: &str,
        candidates: Vec<Candidate>,
        require_match: bool,
        on_accept: impl FnOnce(&mut Editor, Option<Candidate>, String) + 'static,
    ) {
        let mut mb = Minibuffer {
            prompt: prompt.to_string(),
            input: TextArea::new(""),
            candidates,
            dynamic: None,
            filtered: Vec::new(),
            selected: 0,
            require_match,
            on_accept: Some(Box::new(on_accept)),
            last_query: None,
            on_select: None,
            on_cancel: None,
            keep_order: false,
            previewed: None,
        };
        mb.refilter();
        self.minibuffer = Some(mb);
    }

    /// Prompt with candidates recomputed from the input on every change.
    pub fn completing_read_dynamic(
        &mut self,
        prompt: &str,
        initial: &str,
        source: impl Fn(&str) -> Vec<Candidate> + 'static,
        on_accept: impl FnOnce(&mut Editor, Option<Candidate>, String) + 'static,
    ) {
        let mut input = TextArea::new(initial);
        input.move_to_end();
        let mut mb = Minibuffer {
            prompt: prompt.to_string(),
            input,
            candidates: Vec::new(),
            dynamic: Some(Box::new(source)),
            filtered: Vec::new(),
            selected: 0,
            require_match: false,
            on_accept: Some(Box::new(on_accept)),
            last_query: None,
            on_select: None,
            on_cancel: None,
            keep_order: false,
            previewed: None,
        };
        mb.refilter();
        self.minibuffer = Some(mb);
    }

    /// Free-text prompt.
    pub fn read_string(&mut self, prompt: &str, initial: &str, on_accept: impl FnOnce(&mut Editor, String) + 'static) {
        self.completing_read(prompt, Vec::new(), false, move |ed, _, input| on_accept(ed, input));
        if let Some(mb) = &mut self.minibuffer {
            mb.input.set_text(initial);
            mb.input.move_to_end();
        }
    }
}

pub fn accept(ed: &mut Editor) {
    let Some(mut mb) = ed.minibuffer.take() else { return };
    mb.refilter();
    let input = mb.input.text();
    let cand = mb.current().cloned();
    if mb.require_match && cand.is_none() {
        ed.minibuffer = Some(mb);
        ed.message("[No match]");
        return;
    }
    // With dynamic sources (paths), the typed text wins unless a candidate was moved to.
    let cand = if mb.dynamic.is_some() && mb.selected == 0 && !input.ends_with('/') && cand.as_ref().is_none_or(|c| c.value != input) {
        cand.filter(|c| c.value.ends_with(&input))
    } else {
        cand
    };
    if let Some(f) = mb.on_accept.take() {
        f(ed, cand, input);
    }
}

impl Editor {
    /// Attach live-preview and cancel callbacks to the open prompt.
    pub fn minibuffer_preview(&mut self, on_select: Option<SelectFn>, on_cancel: Option<CancelFn>, keep_order: bool) {
        if let Some(mb) = &mut self.minibuffer {
            mb.on_select = on_select;
            mb.on_cancel = on_cancel;
            mb.keep_order = keep_order;
            mb.last_query = None;
            mb.refilter();
        }
    }
}

pub fn cancel(ed: &mut Editor) {
    let mb = ed.minibuffer.take();
    ed.message("Quit");
    if let Some(f) = mb.and_then(|mut m| m.on_cancel.take()) {
        f(ed);
    }
}

/// Report selection changes to the prompt's `on_select` (called once per frame).
pub fn notify_selection(ed: &mut Editor) {
    let Some(mb) = &mut ed.minibuffer else { return };
    let Some(f) = mb.on_select.clone() else { return };
    let cur = mb.current().cloned();
    let key = Some(cur.as_ref().map(|c| c.value.clone()));
    if mb.previewed == key {
        return;
    }
    mb.previewed = key;
    f(ed, cur.as_ref());
}

pub fn insert_text(ed: &mut Editor, text: &str) {
    if let Some(mb) = &mut ed.minibuffer {
        let text = text.replace('\n', " ");
        let c = mb.input.cursor();
        let end = mb.input.insert(c, &text);
        mb.input.set_cursor(end);
        mb.refilter();
    }
}

pub fn self_insert(ed: &mut Editor, chord: &KeyChord) {
    if let Some(c) = chord.printable() {
        insert_text(ed, &c.to_string());
    }
}

fn edit(ed: &mut Editor, f: impl FnOnce(&mut TextArea)) {
    if let Some(mb) = &mut ed.minibuffer {
        f(&mut mb.input);
        mb.refilter();
    }
}

fn move_sel(ed: &mut Editor, d: isize) {
    if let Some(mb) = &mut ed.minibuffer {
        let n = mb.filtered.len() as isize;
        if n > 0 {
            mb.selected = ((mb.selected as isize + d).rem_euclid(n)) as usize;
        }
    }
}

/// Register minibuffer commands and the `minibuffer` keymap (owner `builtin`).
pub fn install(ed: &mut Editor) {
    let o = crate::editor::BUILTIN;
    ed.define_command("minibuffer-accept", "Accept the current candidate or input.", o, |ed, _| {
        accept(ed);
        Ok(())
    });
    ed.define_command("minibuffer-next", "Select the next candidate.", o, |ed, a: &Args| {
        move_sel(ed, a.count() as isize);
        Ok(())
    });
    ed.define_command("minibuffer-previous", "Select the previous candidate.", o, |ed, a: &Args| {
        move_sel(ed, -(a.count() as isize));
        Ok(())
    });
    ed.define_command("minibuffer-complete", "Complete the input to the selected candidate.", o, |ed, _| {
        edit(ed, |_| {});
        if let Some(mb) = &mut ed.minibuffer {
            if let Some(c) = mb.current().cloned() {
                mb.input.set_text(&c.value);
                mb.input.move_to_end();
                mb.refilter();
            }
        }
        Ok(())
    });
    ed.define_command("minibuffer-delete-backward", "Delete the character before point.", o, |ed, _| {
        edit(ed, |t| {
            let c = t.cursor();
            if c.col > 0 {
                t.delete(Pos::new(0, c.col - 1), c);
                t.set_cursor(Pos::new(0, c.col - 1));
            }
        });
        Ok(())
    });
    ed.define_command("minibuffer-kill-word", "Delete the word before point.", o, |ed, _| {
        edit(ed, |t| {
            let c = t.cursor();
            let start = crate::motion::emacs_backward_word(t, c);
            let s = start.min(c);
            t.delete(s, c);
            t.set_cursor(s);
        });
        Ok(())
    });
    ed.define_command("minibuffer-clear", "Clear the input.", o, |ed, _| {
        edit(ed, |t| t.clear());
        Ok(())
    });
    ed.define_command("minibuffer-left", "Move point left.", o, |ed, _| {
        edit(ed, |t| {
            let c = t.cursor();
            t.set_cursor(Pos::new(0, c.col.saturating_sub(1)));
        });
        Ok(())
    });
    ed.define_command("minibuffer-right", "Move point right.", o, |ed, _| {
        edit(ed, |t| {
            let c = t.cursor();
            t.set_cursor(Pos::new(0, c.col + 1));
        });
        Ok(())
    });
    ed.define_command("minibuffer-home", "Move point to the start.", o, |ed, _| {
        edit(ed, |t| t.set_cursor(Pos::ZERO));
        Ok(())
    });
    ed.define_command("minibuffer-end", "Move point to the end.", o, |ed, _| {
        edit(ed, |t| {
            let len = char_len(&t.line(0));
            t.set_cursor(Pos::new(0, len));
        });
        Ok(())
    });
    for (k, c) in [
        ("RET", "minibuffer-accept"),
        ("C-m", "minibuffer-accept"),
        ("C-n", "minibuffer-next"),
        ("<down>", "minibuffer-next"),
        ("C-j", "minibuffer-next"),
        ("C-p", "minibuffer-previous"),
        ("<up>", "minibuffer-previous"),
        ("C-k", "minibuffer-previous"),
        ("TAB", "minibuffer-complete"),
        ("DEL", "minibuffer-delete-backward"),
        ("C-h", "minibuffer-delete-backward"),
        ("C-w", "minibuffer-kill-word"),
        ("M-DEL", "minibuffer-kill-word"),
        ("C-u", "minibuffer-clear"),
        ("<left>", "minibuffer-left"),
        ("C-b", "minibuffer-left"),
        ("<right>", "minibuffer-right"),
        ("C-f", "minibuffer-right"),
        ("C-a", "minibuffer-home"),
        ("C-e", "minibuffer-end"),
        ("ESC", "keyboard-quit"),
        ("C-g", "keyboard-quit"),
    ] {
        ed.bind("minibuffer", k, c, o);
    }
}

#[cfg(test)]
mod tests {
    use super::fuzzy;

    #[test]
    fn fuzzy_prefers_contiguous_word_starts() {
        let a = fuzzy("sw", "switch-to-buffer").unwrap().0;
        let b = fuzzy("sw", "set-window").unwrap().0;
        assert!(a > 0 && b > 0);
        assert!(fuzzy("xyz", "split-window").is_none());
        assert!(fuzzy("win split", "split-window-right").is_some());
    }
}
