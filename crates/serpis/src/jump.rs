//! Jumping and copying by label, like avy and hop.nvim, plus ninja (avy-driven copying).
//!
//! A jump finds targets (regex matches on the visible lines of every visible window, or only
//! the focused one with `jump-all-windows` off), dims the windows and paints a label over each
//! target. Typing a label acts on its target: `goto` moves the cursor there (focusing its
//! window), `copy` copies the target's text and flashes it without moving anything. Labels are
//! a prefix-free code over `jump-keys`: the nearest targets get one key, the rest two or more,
//! narrowing as you type. `ESC` / `C-g` (or a click) cancels.
//!
//! Commands: `jump-word`, `jump-char`, `jump-char-2`, `jump-line`, `jump-pattern`, and ninja's
//! `ninja-copy-line`, `ninja-copy-word`, `ninja-copy-hash`, `ninja-copy-url`,
//! `ninja-copy-quoted`. Lua: `serpis.jump.run{regex, group, action, highlight, all_windows}`.
//!
//! Settings: `jump-keys`, `jump-all-windows`, `jump-dim`, `jump-label-colors`,
//! `jump-single-candidate`.

use std::rc::Rc;
use std::time::{Duration, Instant};

use egui::{Color32, FontId, Rect};
use regex::Regex;
use serde_json::{Value, json};

use crate::buffer::{BufferId, WindowId};
use crate::editor::{BUILTIN, Editor};
use crate::keys::{Key, KeyChord};
use crate::text::Pos;

/// Something a label points at.
#[derive(Debug, Clone)]
pub struct Target {
    pub window: WindowId,
    pub buffer: BufferId,
    /// The buffer part it is in (a chat's transcript or prompt).
    pub part: usize,
    pub start: Pos,
    /// Exclusive end of the matched region (single line).
    pub end: Pos,
    /// Where the label goes (the first character's cell).
    pub anchor: Rect,
    /// The region on screen (highlighted while copying, flashed after).
    pub region: Rect,
}

/// What choosing a target does.
#[derive(Clone)]
pub enum Action {
    Goto,
    Copy,
    Custom(Rc<dyn Fn(&mut Editor, &Target)>),
}

/// Which part of a match is the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Whole,
    Nth(usize),
    /// The first capture group that took part (alternatives like `"…"|'…'`).
    FirstMatched,
}

#[derive(Clone)]
pub struct Spec {
    pub regex: Regex,
    pub group: Group,
    /// Keep only matches whose text passes.
    pub filter: Option<Rc<dyn Fn(&str) -> bool>>,
    pub action: Action,
    /// Highlight every target's region while labels show (ninja).
    pub highlight: bool,
    /// Look in every visible window (else only the focused one); `None` follows `jump-all-windows`.
    pub all_windows: Option<bool>,
}

impl Spec {
    pub fn new(regex: Regex, action: Action) -> Self {
        Spec { regex, group: Group::Whole, filter: None, action, highlight: false, all_windows: None }
    }
}

/// An active jump: labels on screen, waiting for keys.
pub struct Jump {
    pub targets: Vec<Target>,
    pub labels: Vec<String>,
    pub typed: String,
    action: Action,
    highlight: bool,
    /// Window bodies that are dimmed.
    windows: Vec<Rect>,
}

/// Characters being read before a jump (`jump-char` reads one, `jump-char-2` two).
pub struct Reading {
    pub want: usize,
    pub got: String,
    pub action: Action,
}

#[derive(Default)]
pub struct JumpState {
    pub active: Option<Jump>,
    pub reading: Option<Reading>,
    /// Copied regions fading out (like Emacs `pulse`).
    pulses: Vec<(Rect, Instant)>,
}

impl JumpState {
    pub fn busy(&self) -> bool {
        self.active.is_some() || self.reading.is_some()
    }
}

/// Labels for `n` targets: a prefix-free code over `keys`, shortest labels first (avy/hop):
/// as many single keys as possible, the remaining targets under prefix keys taken from the end.
pub fn labels(n: usize, keys: &[char]) -> Vec<String> {
    let k = keys.len();
    if n == 0 || k == 0 {
        return Vec::new();
    }
    if n <= k {
        return keys[..n].iter().map(|c| c.to_string()).collect();
    }
    if k == 1 {
        return (1..=n).map(|i| keys[0].to_string().repeat(i)).collect();
    }
    // Fewest prefixes whose subtrees (two keys each, deeper if needed) hold the rest.
    let mut p = 1;
    while p < k && p * k < n - (k - p) {
        p += 1;
    }
    let leaves = k - p;
    let rest = n - leaves;
    let mut out: Vec<String> = keys[..leaves].iter().map(|c| c.to_string()).collect();
    for (i, prefix) in keys[leaves..].iter().enumerate() {
        let count = rest / p + usize::from(i < rest % p);
        out.extend(labels(count, keys).into_iter().map(|s| format!("{prefix}{s}")));
    }
    out
}

fn keys(ed: &Editor) -> Vec<char> {
    let s = ed.var_str("jump-keys").unwrap_or_default();
    let mut out: Vec<char> = Vec::new();
    for c in s.chars() {
        if !out.contains(&c) && !c.is_whitespace() {
            out.push(c);
        }
    }
    if out.len() < 2 { "asdghklqwertyuiopzxcvbnmfj".chars().collect() } else { out }
}

fn char_col(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].chars().count()
}

/// Every target of `spec` in the visible windows, nearest to the cursor first.
pub fn collect(ed: &mut Editor, spec: &Spec) -> (Vec<Target>, Vec<Rect>) {
    let all = spec.all_windows.unwrap_or_else(|| ed.var("jump-all-windows").and_then(Value::as_bool).unwrap_or(true));
    let mut windows: Vec<WindowId> = if all { ed.layout.visible_leaves() } else { vec![ed.focused] };
    // The focused window first: its targets win ties.
    windows.sort_by_key(|w| *w != ed.focused);
    let mut targets = Vec::new();
    let mut bodies = Vec::new();
    let mut seen: Vec<BufferId> = Vec::new();
    for w in windows {
        let Some(win) = ed.windows.get(w) else { continue };
        let (id, wrect) = (win.buffer, win.rect);
        // A buffer shown twice has one view state; label it once.
        if seen.contains(&id) || wrect == Rect::NOTHING {
            continue;
        }
        seen.push(id);
        bodies.push(wrect);
        let found = ed.with_buffer(id, |b, _| {
            let mut out = Vec::new();
            let original = b.part();
            for part in 0..b.parts() {
                b.set_part(part);
                let Some(s) = b.surface() else { continue };
                let n = s.line_count();
                for line in s.visible_lines() {
                    if line >= n {
                        break;
                    }
                    let text = s.line(line);
                    for caps in spec.regex.captures_iter(&text) {
                        let m = match spec.group {
                            Group::Whole => caps.get(0),
                            Group::Nth(g) => caps.get(g),
                            Group::FirstMatched => (1..caps.len()).find_map(|g| caps.get(g)).or_else(|| caps.get(0)),
                        };
                        let Some(m) = m else { continue };
                        if m.start() == m.end() && spec.group != Group::Whole {
                            continue;
                        }
                        if spec.filter.as_ref().is_some_and(|f| !f(m.as_str())) {
                            continue;
                        }
                        let start = Pos::new(line, char_col(&text, m.start()));
                        let end = Pos::new(line, char_col(&text, m.end()));
                        let Some(anchor) = s.screen_rect(start) else { continue };
                        // Only what is actually on screen in this window.
                        if !wrect.contains(anchor.center()) {
                            continue;
                        }
                        let last = if end.col > start.col { s.screen_rect(Pos::new(line, end.col - 1)) } else { None };
                        let region = last.map(|l| anchor.union(l)).unwrap_or(anchor);
                        out.push(Target { window: w, buffer: id, part, start, end, anchor, region });
                    }
                }
            }
            b.set_part(original);
            out
        });
        targets.extend(found.unwrap_or_default());
    }
    // Nearest first, measured from the cursor (rows count more than columns, as in hop).
    let origin = ed
        .with_surface(|s, _| s.screen_rect(s.cursor()))
        .flatten()
        .map(|r| r.center())
        .unwrap_or_else(|| ed.windows.get(ed.focused).map(|w| w.rect.center()).unwrap_or_default());
    targets.sort_by(|a, b| {
        let d = |t: &Target| {
            let c = t.anchor.center();
            (c.y - origin.y).abs() * 2.0 + (c.x - origin.x).abs()
        };
        d(a).total_cmp(&d(b))
    });
    (targets, bodies)
}

/// Start a jump over `spec`'s targets.
pub fn run(ed: &mut Editor, spec: Spec) {
    cancel(ed);
    let (targets, windows) = collect(ed, &spec);
    if targets.is_empty() {
        ed.message("Nothing to jump to");
        return;
    }
    if targets.len() == 1 && ed.var("jump-single-candidate").and_then(Value::as_bool).unwrap_or(true) {
        act(ed, &targets[0], &spec.action);
        return;
    }
    let labels = labels(targets.len(), &keys(ed));
    ed.jump.active = Some(Jump { targets, labels, typed: String::new(), action: spec.action, highlight: spec.highlight, windows });
    ed.ctx.request_repaint();
}

/// Read `n` characters, then jump to where they occur.
pub fn read_chars(ed: &mut Editor, n: usize, action: Action) {
    cancel(ed);
    ed.jump.reading = Some(Reading { want: n, got: String::new(), action });
    ed.message(if n == 1 { "Jump to char:" } else { "Jump to chars:" });
}

pub fn cancel(ed: &mut Editor) {
    ed.jump.active = None;
    ed.jump.reading = None;
}

/// Do `action` on `t`.
pub fn act(ed: &mut Editor, t: &Target, action: &Action) {
    match action {
        Action::Goto => {
            ed.focus_window(t.window);
            let start = t.start;
            let part = t.part;
            let editable = ed
                .with_buffer(t.buffer, |b, _| {
                    b.set_part(part);
                    b.surface().map(|s| {
                        s.set_cursor(start);
                        s.reveal_cursor();
                        s.editable_at(start)
                    })
                })
                .flatten()
                .unwrap_or(false);
            // Landing on read-only text (a transcript, terminal output) leaves vim insert state.
            if !editable && crate::packages::vim::state_of(ed, t.buffer) == Some(crate::packages::vim::State::Insert) {
                crate::packages::vim::set_state(ed, t.buffer, crate::packages::vim::State::Normal);
            }
        }
        Action::Copy => {
            let (part, start, end) = (t.part, t.start, t.end);
            let text = ed
                .with_buffer(t.buffer, |b, _| {
                    let original = b.part();
                    b.set_part(part);
                    let text = b.surface().map(|s| s.text_range(start, end));
                    b.set_part(original);
                    text
                })
                .flatten()
                .unwrap_or_default();
            ed.copy_to_clipboard(&text);
            ed.jump.pulses.push((t.region, Instant::now()));
            let shown: String = text.chars().take(80).collect();
            ed.message(format!("Copied: {shown}{}", if text.chars().count() > 80 { "…" } else { "" }));
            ed.run_hook("jump-copied", &json!({"text": text}));
        }
        Action::Custom(f) => f(ed, t),
    }
    ed.ctx.request_repaint();
}

/// A key while a jump is active or characters are being read; returns whether it was taken.
pub fn key(ed: &mut Editor, chord: &KeyChord) -> bool {
    if !ed.jump.busy() {
        return false;
    }
    let cancel_key = matches!(&chord.key, Key::Named(n) if n == "escape") || (chord.mods.ctrl && chord.key == Key::Char('g'));
    if cancel_key {
        cancel(ed);
        ed.message("Quit");
        return true;
    }
    let Some(c) = chord.printable() else {
        // Any other command key ends the jump and runs as usual.
        cancel(ed);
        return false;
    };
    if let Some(mut r) = ed.jump.reading.take() {
        r.got.push(c);
        if r.got.chars().count() < r.want {
            ed.jump.reading = Some(r);
            return true;
        }
        // Case-insensitive unless the typed text has capitals (smartcase).
        let pat = regex::escape(&r.got);
        let pat = if r.got.chars().any(char::is_uppercase) { pat } else { format!("(?i){pat}") };
        if let Ok(re) = Regex::new(&pat) {
            run(ed, Spec::new(re, r.action));
        }
        return true;
    }
    let Some(mut j) = ed.jump.active.take() else { return true };
    j.typed.push(c);
    let matching: Vec<usize> = (0..j.labels.len()).filter(|i| j.labels[*i].starts_with(&j.typed)).collect();
    match matching.as_slice() {
        [] => {
            ed.message(format!("No label {}", j.typed));
        }
        [i] if j.labels[*i] == j.typed => {
            let t = j.targets[*i].clone();
            let action = j.action.clone();
            act(ed, &t, &action);
        }
        _ => ed.jump.active = Some(j),
    }
    ed.ctx.request_repaint();
    true
}

fn color(ed: &Editor, i: usize, fallback: Color32) -> Color32 {
    ed.var("jump-label-colors")
        .and_then(Value::as_array)
        .and_then(|a| a.get(i))
        .and_then(Value::as_str)
        .and_then(crate::themes::parse_hex)
        .unwrap_or(fallback)
}

/// Paint the active jump (dimmed windows, highlighted targets, labels) and fading copies.
pub fn draw(ui: &mut egui::Ui, ed: &mut Editor) {
    let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("serpis-jump")));
    let t = ed.theme.clone();
    // Copied regions fade out.
    let fade = Duration::from_millis(450);
    ed.jump.pulses.retain(|(_, born)| born.elapsed() < fade);
    for (r, born) in &ed.jump.pulses {
        let a = 1.0 - born.elapsed().as_secs_f32() / fade.as_secs_f32();
        painter.rect_filled(r.expand(1.0), 3.0, crate::chrome::tint(t.yellow, 0.45 * a));
    }
    if !ed.jump.pulses.is_empty() {
        ui.ctx().request_repaint();
    }
    // A click anywhere cancels.
    if ed.jump.busy() && ui.input(|i| i.pointer.any_pressed()) {
        cancel(ed);
        return;
    }
    let Some(j) = &ed.jump.active else { return };
    if ed.var("jump-dim").and_then(Value::as_bool).unwrap_or(true) {
        for r in &j.windows {
            painter.rect_filled(r.shrink(1.0), t.radius + 4, crate::chrome::tint(t.bg_alt, 0.74));
        }
    }
    let next = color(ed, 0, Color32::from_rgb(0xff, 0x00, 0x7c));
    let first = color(ed, 1, Color32::from_rgb(0x00, 0xdf, 0xff));
    let rest = color(ed, 2, Color32::from_rgb(0x2b, 0x8d, 0xb3));
    let font = FontId::monospace(t.mono_size);
    for (target, label) in j.targets.iter().zip(&j.labels) {
        let Some(remaining) = label.strip_prefix(j.typed.as_str()) else { continue };
        if j.highlight {
            painter.rect_filled(target.region.expand(1.0), 3.0, crate::chrome::tint(t.accent, 0.22));
        }
        // The label replaces the characters it covers, like avy's at-full style.
        let mut job = egui::text::LayoutJob::default();
        for (i, c) in remaining.chars().enumerate() {
            let fg = if remaining.chars().count() == 1 { next } else if i == 0 { first } else { rest };
            let mut f = egui::TextFormat { font_id: font.clone(), color: fg, ..Default::default() };
            f.coords.push(b"wght", t.bold_weight);
            job.append(&c.to_string(), 0.0, f);
        }
        let g = painter.layout_job(job);
        let pos = egui::pos2(target.anchor.left(), target.anchor.center().y - g.size().y / 2.0);
        let bg = Rect::from_min_size(pos, g.size()).expand2(egui::vec2(1.0, 0.0));
        painter.rect_filled(bg, 2.0, t.bg_alt);
        painter.galley(pos, g, next);
    }
}

// ------------------------------------------------------------------ ninja and commands

pub const LINE_REGEX: &str = r"^[ \t]*([^ \t].*?)[ \t]*$";
pub const WORD_REGEX: &str = r"[\p{L}\p{N}_]+(?:[-.][\p{L}\p{N}_]+)*";
pub const HASH_REGEX: &str = r"\b[0-9a-fA-F]{7,128}\b";
pub const URL_REGEX: &str = r#"(?:https?|ftp|file)://[^\]\[ \t\n"'<>(){}`]*[^\]\[ \t\n"'<>(){}`.,;:!?]"#;
pub const QUOTE_REGEX: &str = r#""((?:[^"\\\n]|\\.)+)"|'((?:[^'\\\n]|\\.)+)'"#;

/// A hash has at least one hex letter and one digit (not a plain number or word).
fn is_hash(s: &str) -> bool {
    s.chars().any(|c| c.is_ascii_hexdigit() && c.is_ascii_alphabetic()) && s.chars().any(|c| c.is_ascii_digit())
}

fn re(s: &str) -> Regex {
    Regex::new(s).expect("built-in jump regex")
}

pub fn install(ed: &mut Editor) {
    let o = BUILTIN;
    ed.defvar("jump-keys", json!("asdghklqwertyuiopzxcvbnmfj"), "Keys for jump labels, best first (hop's default; avy users often use \"abcdefghijklmnopqrstuvwxyz\").");
    ed.defvar("jump-all-windows", json!(true), "Jumps label targets in every visible window (else only the focused one).");
    ed.defvar("jump-dim", json!(true), "Dim the windows while jump labels show.");
    ed.defvar("jump-label-colors", json!(["#ff007c", "#00dfff", "#2b8db3"]), "Label colors: a single remaining key, the first of several, the others (hop's colors).");
    ed.defvar("jump-single-candidate", json!(true), "With a single target, act at once instead of showing a label.");

    let goto = |ed: &mut Editor, regex: &str, group: Group| {
        let mut spec = Spec::new(re(regex), Action::Goto);
        spec.group = group;
        run(ed, spec);
    };
    ed.define_command("jump-word", "Jump to the start of a visible word (avy-goto-word-0, HopWord).", o, move |ed, _| {
        goto(ed, r"[\p{L}\p{N}_]+", Group::Whole);
        Ok(())
    });
    ed.define_command("jump-line", "Jump to a visible line (its first non-blank character).", o, move |ed, _| {
        goto(ed, r"^[ \t]*([^ \t])", Group::Nth(1));
        Ok(())
    });
    ed.define_command("jump-char", "Type a character, then jump to one of its visible occurrences (avy-goto-char).", o, |ed, _| {
        read_chars(ed, 1, Action::Goto);
        Ok(())
    });
    ed.define_command("jump-char-2", "Type two characters, then jump to one of their visible occurrences (avy-goto-char-2, HopChar2).", o, |ed, _| {
        read_chars(ed, 2, Action::Goto);
        Ok(())
    });
    ed.define_command("jump-pattern", "Jump to a visible match of a regex (HopPattern; param: regex).", o, |ed, a| {
        let go = |ed: &mut Editor, pat: String| match Regex::new(&pat) {
            Ok(re) if !pat.is_empty() => run(ed, Spec::new(re, Action::Goto)),
            Ok(_) => {}
            Err(e) => ed.error(format!("{e}")),
        };
        match a.str(0) {
            Some(p) => go(ed, p.to_string()),
            None => ed.read_string("Jump to pattern: ", "", go),
        }
        Ok(())
    });

    // ninja: copy a visible target.
    let copy = |ed: &mut Editor, regex: &str, group: Group, filter: Option<Rc<dyn Fn(&str) -> bool>>| {
        let mut spec = Spec::new(re(regex), Action::Copy);
        spec.group = group;
        spec.filter = filter;
        spec.highlight = true;
        run(ed, spec);
    };
    ed.define_command("ninja-copy-line", "Copy a visible line (without surrounding whitespace).", o, move |ed, _| {
        copy(ed, LINE_REGEX, Group::Nth(1), None);
        Ok(())
    });
    ed.define_command("ninja-copy-word", "Copy a visible word or symbol.", o, move |ed, _| {
        copy(ed, WORD_REGEX, Group::Whole, None);
        Ok(())
    });
    ed.define_command("ninja-copy-hash", "Copy a visible hex hash (git sha, digest, ...).", o, move |ed, _| {
        copy(ed, HASH_REGEX, Group::Whole, Some(Rc::new(is_hash)));
        Ok(())
    });
    ed.define_command("ninja-copy-url", "Copy a visible URL.", o, move |ed, _| {
        copy(ed, URL_REGEX, Group::Whole, None);
        Ok(())
    });
    ed.define_command("ninja-copy-quoted", "Copy the contents of a visible double- or single-quoted string.", o, move |ed, _| {
        copy(ed, QUOTE_REGEX, Group::FirstMatched, None);
        Ok(())
    });
    ed.define_command("jump-cancel", "Remove jump labels.", o, |ed, _| {
        cancel(ed);
        Ok(())
    });

    for (k, c) in [
        ("M-g w", "jump-word"),
        ("M-g l", "jump-line"),
        ("M-g c", "jump-char"),
        ("M-g C", "jump-char-2"),
        ("M-g /", "jump-pattern"),
        ("s-g l", "ninja-copy-line"),
        ("s-g w", "ninja-copy-word"),
        ("s-g h", "ninja-copy-hash"),
        ("s-g u", "ninja-copy-url"),
        ("s-g q", "ninja-copy-quoted"),
    ] {
        ed.bind("global", k, c, o);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_prefix_free_and_short_first() {
        let keys: Vec<char> = "abc".chars().collect();
        assert_eq!(labels(3, &keys), ["a", "b", "c"]);
        let l = labels(5, &keys);
        assert_eq!(l.len(), 5);
        assert_eq!(l[0], "a", "nearest gets a single key");
        for (i, a) in l.iter().enumerate() {
            for (j, b) in l.iter().enumerate() {
                assert!(i == j || !b.starts_with(a.as_str()), "{a} is a prefix of {b}");
            }
        }
        let many = labels(300, &"asdghklqwertyuiopzxcvbnmfj".chars().collect::<Vec<_>>());
        assert_eq!(many.len(), 300);
        assert!(many.iter().all(|l| l.len() <= 2));
        let mut sorted = many.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 300, "unique");
        let deep = labels(2000, &keys);
        assert_eq!(deep.len(), 2000);
    }

    #[test]
    fn ninja_regexes_match_like_emacs() {
        let caps = |r: &str, s: &str, g: Group| -> Vec<String> {
            Regex::new(r)
                .unwrap()
                .captures_iter(s)
                .filter_map(|c| match g {
                    Group::Whole => c.get(0),
                    Group::Nth(n) => c.get(n),
                    Group::FirstMatched => (1..c.len()).find_map(|i| c.get(i)),
                })
                .map(|m| m.as_str().to_string())
                .collect()
        };
        assert_eq!(caps(LINE_REGEX, "   let x = 1;  ", Group::Nth(1)), ["let x = 1;"]);
        assert_eq!(caps(URL_REGEX, "see https://example.com/a?b=c, then (http://x.io/y).", Group::Whole), ["https://example.com/a?b=c", "http://x.io/y"]);
        let hashes: Vec<String> = caps(HASH_REGEX, "commit 4c898c3 and 1234567 and deadbeef00", Group::Whole).into_iter().filter(|s| is_hash(s)).collect();
        assert_eq!(hashes, ["4c898c3", "deadbeef00"]);
        assert_eq!(caps(QUOTE_REGEX, r#"a "one \"two\"" b 'three'"#, Group::FirstMatched), [r#"one \"two\""#, "three"]);
        assert_eq!(caps(WORD_REGEX, "foo-bar baz_qux v1.2.3 x", Group::Whole), ["foo-bar", "baz_qux", "v1.2.3", "x"]);
    }
}
