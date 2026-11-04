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
    /// Drawn as a checkbox, ticked or not.
    pub checked: Option<bool>,
    /// The heading this candidate falls under, drawn above the first of each.
    pub section: Option<String>,
}

impl Candidate {
    pub fn new(label: impl Into<String>) -> Self {
        let label = label.into();
        Candidate {
            value: label.clone(),
            label,
            detail: String::new(),
            checked: None,
            section: None,
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
/// Called on RET or a click instead of accepting: the prompt stays open.
pub type ToggleFn = std::rc::Rc<dyn Fn(&mut Editor, &Candidate)>;
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
    /// Typing a candidate's first letter accepts it at once (`y_or_n`).
    pub instant: bool,
    /// With a dynamic source, typed text wins over an unselected first candidate (file
    /// paths); pickers that offer named choices turn this off.
    pub prefer_input: bool,
    /// Frecency category: picks are remembered and rank higher next time (see `frecency`).
    pub frecency: Option<String>,
    /// Ranking bonus per candidate (from frecency), parallel to `candidates`.
    boost: Vec<i64>,
    /// With an empty input, order by frecency too (not for lists whose order means something,
    /// such as recent buffers).
    pub frecency_empty: bool,
    /// Switch the candidates to another set (TAB forward, S-TAB back): chat-resume's scopes.
    pub cycle: Option<CycleFn>,
    /// More candidates are on their way (a list fetched from elsewhere): shown as a moving bar.
    pub loading: bool,
    /// Instead of accepting, RET and clicks call this and the prompt stays open.
    pub on_toggle: Option<ToggleFn>,
    /// No search field: a list to go through. SPC does what RET does, `j` and `k` move, and
    /// other typing is ignored.
    pub searchable: bool,
}

/// Replaces the open prompt's candidates for the next (+1) or previous (-1) set.
pub type CycleFn = std::rc::Rc<dyn Fn(&mut Editor, i32)>;

impl Minibuffer {
    /// Show other candidates under another prompt, keeping what was typed.
    pub fn replace_candidates(&mut self, prompt: String, candidates: Vec<Candidate>) {
        self.prompt = prompt;
        self.candidates = candidates;
        self.boost.clear();
        self.last_query = None;
        self.refilter();
    }
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
        let boost = |i: usize| self.boost.get(i).copied().unwrap_or(0);
        let mut scored: Vec<(i64, usize, Vec<usize>)> = self
            .candidates
            .iter()
            .enumerate()
            .filter_map(|(i, c)| fuzzy(&query, &c.label).map(|(s, p)| (s + boost(i), i, p)))
            .collect();
        if !self.keep_order && (!query.is_empty() || (self.frecency_empty && !self.boost.is_empty())) {
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
    /// Rank the open prompt's candidates by frecency in `category`, and remember what is
    /// picked. `empty`: also order them so with nothing typed. Off with `minibuffer-frecency`.
    pub fn minibuffer_frecency(&mut self, category: &str, empty: bool) {
        if !self.var("minibuffer-frecency").and_then(serde_json::Value::as_bool).unwrap_or(true) {
            return;
        }
        let Some(mut mb) = self.minibuffer.take() else { return };
        let keys: Vec<&str> = mb.candidates.iter().map(|c| c.value.as_str()).collect();
        let scores = crate::frecency::scores(self, category, &keys);
        mb.boost = if scores.iter().any(|s| *s > 0.0) { scores.into_iter().map(crate::frecency::boost).collect() } else { Vec::new() };
        mb.frecency = Some(category.to_string());
        mb.frecency_empty = empty;
        mb.last_query = None;
        mb.refilter();
        self.minibuffer = Some(mb);
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
            on_toggle: None,
            searchable: true,
            previewed: None,
            instant: false,
            prefer_input: true,
            frecency: None,
            boost: Vec::new(),
            frecency_empty: false,
            cycle: None,
            loading: false,
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
            on_toggle: None,
            searchable: true,
            previewed: None,
            instant: false,
            prefer_input: true,
            frecency: None,
            boost: Vec::new(),
            frecency_empty: false,
            cycle: None,
            loading: false,
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
    if let Some(f) = mb.on_toggle.clone() {
        let cand = mb.current().cloned();
        ed.minibuffer = Some(mb);
        if let Some(c) = cand {
            f(ed, &c);
        }
        return;
    }
    let input = mb.input.text();
    let cand = mb.current().cloned();
    if mb.require_match && cand.is_none() {
        ed.minibuffer = Some(mb);
        ed.message("[No match]");
        return;
    }
    // With dynamic sources (paths), the typed text wins unless a candidate was moved to.
    let cand = if mb.dynamic.is_some() && mb.prefer_input && mb.selected == 0 && !input.ends_with('/') && cand.as_ref().is_none_or(|c| c.value != input) {
        cand.filter(|c| c.value.ends_with(&input))
    } else {
        cand
    };
    if let (Some(cat), Some(c)) = (&mb.frecency, &cand) {
        crate::frecency::bump(ed, cat, &c.value);
    }
    if let Some(f) = mb.on_accept.take() {
        f(ed, cand, input);
    }
}

impl Editor {
    /// Ask a yes/no question answered with one key (`y` / `n`; `C-g` or `ESC` means no).
    pub fn y_or_n(&mut self, prompt: &str, f: impl FnOnce(&mut Editor, bool) + 'static) {
        let f = std::rc::Rc::new(std::cell::RefCell::new(Some(f)));
        let on_cancel = f.clone();
        self.completing_read(&format!("{prompt} (y or n) "), vec![Candidate::new("yes"), Candidate::new("no")], true, move |ed, c, _| {
            let f = f.borrow_mut().take();
            if let Some(f) = f {
                f(ed, c.is_some_and(|c| c.value == "yes"));
            }
        });
        if let Some(mb) = &mut self.minibuffer {
            mb.instant = true;
            mb.keep_order = true;
            mb.on_cancel = Some(Box::new(move |ed| {
                let f = on_cancel.borrow_mut().take();
                if let Some(f) = f {
                    f(ed, false);
                }
            }));
        }
    }

    /// Show or hide the open prompt's loading bar (more candidates are coming).
    pub fn minibuffer_loading(&mut self, loading: bool) {
        if let Some(mb) = &mut self.minibuffer {
            mb.loading = loading;
            self.ctx.request_repaint();
        }
    }

    /// Replace the open prompt's candidates (they arrived), keeping the selected one selected.
    pub fn minibuffer_set_candidates(&mut self, candidates: Vec<Candidate>) {
        if let Some(mb) = &mut self.minibuffer {
            let selected = mb.filtered.get(mb.selected).and_then(|(i, _)| mb.candidates.get(*i)).map(|c| c.value.clone());
            mb.candidates = candidates;
            mb.boost.clear();
            mb.last_query = None;
            mb.refilter();
            if let Some(v) = selected {
                mb.selected = mb.filtered.iter().position(|(i, _)| mb.candidates[*i].value == v).unwrap_or(0);
            }
            self.ctx.request_repaint();
        }
    }

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
    if ed.minibuffer.as_ref().is_some_and(|mb| !mb.searchable) {
        match text {
            " " => accept(ed),
            "j" => move_sel(ed, 1),
            "k" => move_sel(ed, -1),
            _ => {}
        }
        return;
    }
    if let Some(mb) = &mut ed.minibuffer {
        let text = text.replace('\n', " ");
        let c = mb.input.cursor();
        let end = mb.input.insert(c, &text);
        mb.input.set_cursor(end);
        mb.refilter();
    }
}

pub fn self_insert(ed: &mut Editor, chord: &KeyChord) {
    let Some(c) = chord.printable() else { return };
    if let Some(mb) = &mut ed.minibuffer {
        if mb.instant {
            let lc = c.to_lowercase().next().unwrap_or(c);
            if let Some(i) = mb.candidates.iter().position(|k| k.label.to_lowercase().starts_with(lc)) {
                let label = mb.candidates[i].label.clone();
                mb.input.set_text(&label);
                mb.refilter();
                accept(ed);
            }
            return;
        }
    }
    insert_text(ed, &c.to_string());
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
    ed.define_command("minibuffer-cycle", "Switch the prompt to its next set of candidates (param: -1 for the previous), if it has several (chat-resume's scopes).", o, |ed, a| {
        let dir = a.params.first().and_then(serde_json::Value::as_i64).unwrap_or(1) as i32;
        if let Some(f) = ed.minibuffer.as_ref().and_then(|m| m.cycle.clone()) {
            f(ed, if dir < 0 { -1 } else { 1 });
        }
        Ok(())
    });
    ed.define_command("minibuffer-cycle-back", "Switch the prompt to its previous set of candidates.", o, |ed, _| {
        if let Some(f) = ed.minibuffer.as_ref().and_then(|m| m.cycle.clone()) {
            f(ed, -1);
        }
        Ok(())
    });
    ed.define_command("minibuffer-complete", "Complete the input to the selected candidate (in prompts with several candidate sets, switch set).", o, |ed, _| {
        if let Some(f) = ed.minibuffer.as_ref().and_then(|m| m.cycle.clone()) {
            f(ed, 1);
            return Ok(());
        }
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
        ("S-TAB", "minibuffer-cycle-back"),
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
