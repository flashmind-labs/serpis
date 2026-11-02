//! The diff page: the files that differ in a tree, the current one side by side (or unified) with
//! matched lines across from each other, unchanged stretches folded, a strip showing where the
//! changes are, and review comments between the lines.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::text::LayoutJob;
use egui::{Color32, FontId, Pos2, Rect, Sense, Stroke, TextFormat};
use serpis_proto::wire::{DiffEntry, DiffRefs, DiffSide, DiffSpec, Request, Response};

use super::engine::{self, FileDiff, Kind, Options, Row, Whitespace};
use super::review::{self, Ask, Comment, Saved, State, Which};
use crate::buffer::{Buffer, BufferId, Modeline};
use crate::chrome;
use crate::editor::{Args, Editor};
use crate::highlight::Spans;
use crate::impl_any;
use crate::text::{CursorShape, PlainLines, TextArea, TextSurface, ViewOptions};
use crate::theme::{Face, Theme};

pub const KIND: &str = "diff";
/// The major mode while a comment is being written, so typing reaches it (the page's own keymap
/// is all single letters).
pub const EDITING: &str = "diff-comment";

/// A comparison's commonest forms.
pub fn uncommitted() -> DiffSpec {
    DiffSpec { base: DiffSide::Rev("HEAD".into()), target: DiffSide::Worktree, merge_base: false }
}
pub fn staged() -> DiffSpec {
    DiffSpec { base: DiffSide::Rev("HEAD".into()), target: DiffSide::Index, merge_base: false }
}
pub fn unstaged() -> DiffSpec {
    DiffSpec { base: DiffSide::Index, target: DiffSide::Worktree, merge_base: false }
}
/// What a branch would contain if merged into `base`, uncommitted work included.
pub fn against(base: &str) -> DiffSpec {
    DiffSpec { base: DiffSide::Rev(base.to_string()), target: DiffSide::Worktree, merge_base: true }
}

/// What a typed source means: `uncommitted`, `staged`, `unstaged`, `a..b`, `a...b` (from where they
/// forked), `commit:<rev>`, or a branch or commit to compare the worktree with.
pub fn parse_source(s: &str) -> Option<DiffSpec> {
    let s = s.trim();
    let rev = |r: &str| DiffSide::Rev(r.to_string());
    Some(match s {
        "" => return None,
        "uncommitted" => uncommitted(),
        "staged" => staged(),
        "unstaged" => unstaged(),
        _ => {
            if let Some(c) = s.strip_prefix("commit:") {
                DiffSpec { base: rev(&format!("{c}^")), target: rev(c), merge_base: false }
            } else if let Some((a, b)) = s.split_once("...") {
                DiffSpec { base: rev(a), target: rev(b), merge_base: true }
            } else if let Some((a, b)) = s.split_once("..") {
                DiffSpec { base: rev(a), target: rev(b), merge_base: false }
            } else {
                against(s)
            }
        }
    })
}

/// A comparison, as a name.
pub fn describe(spec: &DiffSpec) -> String {
    match (&spec.base, &spec.target) {
        (DiffSide::Rev(a), DiffSide::Worktree) if a == "HEAD" => "Uncommitted changes".into(),
        (DiffSide::Rev(a), DiffSide::Index) if a == "HEAD" => "Staged changes".into(),
        (DiffSide::Index, DiffSide::Worktree) => "Unstaged changes".into(),
        (DiffSide::Rev(a), DiffSide::Rev(b)) if a.strip_suffix('^') == Some(b) => format!("Commit {b}"),
        (DiffSide::Rev(a), DiffSide::Rev(b)) => format!("{a} → {b}"),
        (DiffSide::Rev(a), DiffSide::Worktree) => format!("{a} → worktree"),
        (DiffSide::Rev(a), DiffSide::Index) => format!("{a} → staged"),
        (DiffSide::Index, _) => "staged → ?".into(),
        (DiffSide::Worktree, _) => "worktree → ?".into(),
    }
}

/// Ask for something about a repository, where it is: this machine (on a background thread) or
/// a machine's node.
fn ask(ed: &mut Editor, machine: Option<&str>, request: Request, done: impl FnOnce(&mut Editor, Result<Response, String>) + 'static) {
    match machine {
        Some(m) => crate::remote::request(ed, m, request, done),
        None => crate::remote::answer_with(
            ed,
            async move {
                match tokio::task::spawn_blocking(move || serpis_node::gitdiff::answer(&request)).await {
                    Ok(r) => r.map_err(|e| format!("{e:#}")),
                    Err(e) => Err(e.to_string()),
                }
            },
            done,
        ),
    }
}

/// Columns a wrapped line's continuation is indented: its own indentation and a little, so the
/// code's shape stays readable, within reason.
pub fn hang_of(text: &str, cols: usize) -> usize {
    let indent: usize = text.chars().take_while(|c| c.is_whitespace()).map(char_cols).sum();
    (indent + 2).min(cols / 3)
}

/// Where each screen line of a wrapped line after the first starts, in `cols` columns.
pub fn wrap_points(text: &str, cols: usize) -> Vec<usize> {
    let cols = cols.max(8);
    let hang = hang_of(text, cols);
    let mut out = Vec::new();
    let (mut col, mut soft, mut since_soft) = (0usize, None::<usize>, 0usize);
    for (i, c) in text.char_indices() {
        let w = char_cols(c);
        if col + w > cols {
            // At the last space or punctuation mark, when it isn't too far back.
            match soft.filter(|_| since_soft * 2 < cols) {
                Some(at) => {
                    out.push(at);
                    col = hang + since_soft;
                }
                None => {
                    out.push(i);
                    col = hang;
                }
            }
            soft = None;
            since_soft = 0;
        }
        col += w;
        since_soft += w;
        if c.is_whitespace() || matches!(c, ',' | ';' | ')' | ']' | '}') {
            soft = Some(i + c.len_utf8());
            since_soft = 0;
        }
    }
    // A break can't land past the text or start a screen line with nothing after it.
    out.retain(|&at| at < text.len());
    out
}

/// Columns a character takes in a monospace font: a tab is drawn as four, wide scripts as two.
fn char_cols(c: char) -> usize {
    match c as u32 {
        9 => 4,
        0x1100..=0x115f | 0x2e80..=0xa4cf | 0xac00..=0xd7a3 | 0xf900..=0xfaff | 0xfe30..=0xfe6f | 0xff00..=0xff60 | 0xffe0..=0xffe6 | 0x1f300..=0x1faff | 0x20000..=0x3fffd => 2,
        _ => 1,
    }
}

/// What the page shows, top to bottom.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Row(usize),
    /// Unchanged rows folded away.
    Fold { from: usize, to: usize },
    Comment(u64),
    Composer,
}

/// A comment being written.
pub struct CommentEditor {
    /// The comment it edits, when it isn't a new one.
    pub id: Option<u64>,
    pub path: String,
    pub side: Which,
    pub start: u32,
    pub end: u32,
    pub ask: Ask,
    pub area: TextArea,
}

/// Where to land when a file finishes loading.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Land {
    FirstChange,
    LastChange,
}

pub struct DiffBuffer {
    pub id: Option<BufferId>,
    pub machine: Option<String>,
    /// The folder it was opened for.
    pub dir: String,
    /// The worktree around it, once known.
    pub root: String,
    pub spec: Option<DiffSpec>,
    pub refs: Option<DiffRefs>,
    pub merge_base: Option<String>,
    pub files: Vec<DiffEntry>,
    pub error: Option<String>,
    listing: bool,
    listed: Option<Instant>,
    /// Answers to an older comparison are dropped.
    list_token: u64,
    /// A list of files has come since the comparison last changed.
    pub loaded: bool,

    pub current: Option<String>,
    pub diff: Option<Arc<FileDiff>>,
    shown: Option<(String, String)>,
    /// Why the file has no rows (binary, too large).
    pub note: Option<String>,
    pub loading: bool,
    file_token: u64,
    cache: HashMap<(String, String, Options), Arc<FileDiff>>,

    /// The side by side view; off, or too narrow for it, unified.
    pub split: bool,
    pub split_now: bool,
    pub whitespace: Whitespace,
    pub wrap: bool,
    pub show_tree: bool,
    pub expand_all: bool,
    expanded: HashSet<(Option<u32>, Option<u32>)>,
    closed_dirs: HashSet<String>,

    pub rows: Vec<Row>,
    pub hunks: Vec<engine::Hunk>,
    pub items: Vec<Item>,
    row_item: Vec<u32>,
    /// Every item's top, plus the bottom of the last.
    ys: Vec<f32>,
    /// What `ys` was laid out for.
    laid_out: Option<(u32, u32, u32, bool, bool)>,
    items_dirty: bool,

    pub cursor: usize,
    pub side: Which,
    pub anchor: Option<usize>,
    scroll: f32,
    hscroll: f32,
    tree_scroll: f32,
    reveal: bool,
    land: Option<Land>,
    /// Where a drag began, and whether in the line numbers: letting go there starts a comment.
    drag: Option<(usize, bool)>,

    pub review: Saved,
    review_loaded: bool,
    /// Where the saved review left off, until the list of files is in.
    resume_file: Option<String>,
    resume_writing: Option<review::Writing>,
    pub editor: Option<CommentEditor>,
    /// The chat or terminal a review goes to.
    pub target: Option<BufferId>,
    pub search: String,

    /// Its controls as last drawn (`serpis.debug.composer` reads them).
    pub hits: Vec<(String, Rect)>,
}

impl DiffBuffer {
    pub fn new(ed: &Editor, machine: Option<String>, dir: String, spec: Option<DiffSpec>, target: Option<BufferId>) -> Self {
        let style = ed.var_str("diff-view-style").unwrap_or_default();
        DiffBuffer {
            id: None,
            machine,
            root: dir.clone(),
            dir,
            spec,
            refs: None,
            merge_base: None,
            files: Vec::new(),
            error: None,
            listing: false,
            listed: None,
            list_token: 0,
            loaded: false,
            current: None,
            diff: None,
            shown: None,
            note: None,
            loading: false,
            file_token: 0,
            cache: HashMap::new(),
            split: style != "unified",
            split_now: style != "unified",
            whitespace: ed.var_str("diff-whitespace").and_then(|w| Whitespace::parse(&w)).unwrap_or_default(),
            wrap: ed.var_bool("diff-wrap"),
            show_tree: ed.var_bool("diff-file-tree"),
            expand_all: false,
            expanded: HashSet::new(),
            closed_dirs: HashSet::new(),
            rows: Vec::new(),
            hunks: Vec::new(),
            items: Vec::new(),
            row_item: Vec::new(),
            ys: Vec::new(),
            laid_out: None,
            items_dirty: true,
            cursor: 0,
            side: Which::New,
            anchor: None,
            scroll: 0.0,
            hscroll: 0.0,
            tree_scroll: 0.0,
            reveal: false,
            land: None,
            drag: None,
            review: Saved::default(),
            review_loaded: false,
            resume_file: None,
            resume_writing: None,
            editor: None,
            target,
            search: String::new(),
            hits: Vec::new(),
        }
    }

    pub fn entry(&self, path: &str) -> Option<&DiffEntry> {
        self.files.iter().find(|f| f.path == path)
    }

    fn options(&self, ed: &Editor) -> Options {
        let move_lines = ed.var("diff-move-min-lines").and_then(serde_json::Value::as_u64).unwrap_or(3) as usize;
        Options { whitespace: self.whitespace, move_lines }
    }

    /// Read what the repository can be compared with, then the comparison.
    pub fn start(&mut self, ed: &mut Editor) {
        let Some(id) = self.id else { return };
        let (dir, machine) = (self.dir.clone(), self.machine.clone());
        ask(ed, machine.as_deref(), Request::DiffRefs { dir }, move |ed, r| {
            ed.with_buffer_as::<DiffBuffer, _>(id, |b, ed| match r {
                Ok(Response::DiffRefs { refs }) => {
                    b.root = refs.root.clone();
                    if !b.review_loaded {
                        b.review = review::load(b.machine.as_deref(), &b.root);
                        b.review_loaded = true;
                        b.items_dirty = true;
                        // Back to an unfinished review: what it was comparing, where it was.
                        b.resume_file = b.review.file.take();
                        b.resume_writing = b.review.writing.take();
                        let saved = b.review.spec.take();
                        if b.spec.is_none() && (!b.review.comments.is_empty() || b.resume_writing.is_some()) {
                            b.spec = saved;
                        }
                    }
                    if b.spec.is_none() {
                        b.spec = Some(default_spec(ed, &refs));
                    }
                    b.refs = Some(refs);
                    b.refresh(ed);
                }
                Ok(_) => b.error = Some(format!("{} didn't understand that: update its serpis-node", b.machine.as_deref().unwrap_or("This machine"))),
                Err(e) => b.error = Some(e),
            });
        });
    }

    /// Show another comparison.
    pub fn set_spec(&mut self, ed: &mut Editor, spec: DiffSpec) {
        self.commit_comment(ed);
        self.spec = Some(spec);
        self.persist(ed);
        self.files.clear();
        self.loaded = false;
        self.error = None;
        self.current = None;
        self.clear_file();
        self.listing = false;
        self.refresh(ed);
    }

    fn clear_file(&mut self) {
        self.diff = None;
        self.shown = None;
        self.note = None;
        self.rows.clear();
        self.hunks.clear();
        self.expanded.clear();
        self.cursor = 0;
        self.anchor = None;
        self.scroll = 0.0;
        self.hscroll = 0.0;
        self.items_dirty = true;
    }

    /// Read the list of files again.
    pub fn refresh(&mut self, ed: &mut Editor) {
        let (Some(id), Some(spec)) = (self.id, self.spec.clone()) else {
            self.start(ed);
            return;
        };
        if self.listing {
            return;
        }
        self.listing = true;
        self.list_token += 1;
        let token = self.list_token;
        self.listed = Some(Instant::now());
        ask(ed, self.machine.as_deref(), Request::DiffList { dir: self.root.clone(), spec }, move |ed, r| {
            ed.with_buffer_as::<DiffBuffer, _>(id, |b, ed| {
                if b.list_token != token {
                    return;
                }
                b.listing = false;
                b.listed = Some(Instant::now());
                b.take_list(ed, r);
            });
        });
    }

    fn take_list(&mut self, ed: &mut Editor, r: Result<Response, String>) {
        let list = match r {
            Ok(Response::DiffList { list }) => list,
            Ok(_) => {
                self.error = Some(format!("{} didn't understand that: update its serpis-node", self.machine.as_deref().unwrap_or("This machine")));
                return;
            }
            Err(e) => {
                self.error = Some(e);
                self.loaded = true;
                return;
            }
        };
        self.error = None;
        self.loaded = true;
        self.merge_base = list.merge_base;
        self.files = list.files;
        // A file that changed since it was marked as viewed needs another look.
        let files = &self.files;
        let before = self.review.viewed.len();
        self.review.viewed.retain(|p, sig| files.iter().any(|f| f.path == *p && f.signature == *sig));
        if self.review.viewed.len() != before {
            self.persist(ed);
        }
        let keep = self.current.as_deref().is_some_and(|p| self.files.iter().any(|f| f.path == p));
        if !keep {
            self.commit_comment(ed);
            self.clear_file();
            let listed = |p: &String| self.files.iter().any(|f| f.path == *p);
            let back = self.resume_writing.as_ref().map(|w| w.path.clone()).filter(listed).or(self.resume_file.take().filter(listed));
            self.current = back.or_else(|| self.files.first().map(|f| f.path.clone()));
            self.land = Some(Land::FirstChange);
        }
        if let Some(w) = self.resume_writing.take() {
            self.resume_comment(ed, w);
        }
        self.load(ed);
    }

    /// Pick up a comment that was being written when serpis quit. Its file may no longer differ:
    /// then what was written is kept as a comment, not lost.
    fn resume_comment(&mut self, ed: &mut Editor, w: review::Writing) {
        if self.current.as_deref() == Some(w.path.as_str()) {
            self.begin_comment(ed, w.side, w.start, w.end, w.ask, &w.text);
            if let Some(e) = self.editor.as_mut() {
                e.id = w.id;
            }
        } else if let Some(c) = w.id.and_then(|id| self.review.comments.iter_mut().find(|c| c.id == id)) {
            c.text = w.text;
        } else {
            self.add_comment(w.path, w.side, w.start, w.end, w.text, w.ask);
        }
    }

    /// Show the current file, reading it if what is shown is out of date.
    pub fn load(&mut self, ed: &mut Editor) {
        let Some(id) = self.id else { return };
        let Some(entry) = self.current.as_deref().and_then(|p| self.entry(p)).cloned() else { return };
        let options = self.options(ed);
        let wanted = (entry.path.clone(), format!("{}/{:?}", entry.signature, options));
        if self.shown.as_ref() == Some(&wanted) {
            return;
        }
        let key = (entry.path.clone(), entry.signature.clone(), options.clone());
        if let Some(d) = self.cache.get(&key).cloned() {
            self.show_diff(ed, d, wanted);
            return;
        }
        let Some(spec) = self.spec.clone() else { return };
        self.file_token += 1;
        let token = self.file_token;
        self.loading = true;
        let request = Request::DiffFile { dir: self.root.clone(), spec, path: entry.path.clone(), old_path: entry.old_path.clone() };
        ask(ed, self.machine.as_deref(), request, move |ed, r| {
            let (old, new) = match r {
                Ok(Response::DiffFile { old, new }) => (old, new),
                other => {
                    let e = match other {
                        Err(e) => e,
                        _ => "unexpected answer".to_string(),
                    };
                    ed.with_buffer_as::<DiffBuffer, _>(id, |b, _| {
                        if b.file_token == token {
                            b.loading = false;
                            b.diff = None;
                            b.note = Some(format!("Couldn't read this file: {e}"));
                            b.items_dirty = true;
                        }
                    });
                    return;
                }
            };
            let note = if old.binary || new.binary {
                Some("Binary file")
            } else if old.too_large || new.too_large {
                Some("Too large to compare")
            } else {
                None
            };
            if let Some(n) = note {
                ed.with_buffer_as::<DiffBuffer, _>(id, |b, _| {
                    if b.file_token == token {
                        b.loading = false;
                        b.diff = None;
                        b.shown = Some(wanted);
                        b.note = Some(n.to_string());
                        b.rows.clear();
                        b.hunks.clear();
                        b.items_dirty = true;
                    }
                });
                return;
            }
            // Comparing can take a moment, and a window must not stop for it.
            let main = ed.main_sender();
            let path = entry.path.clone();
            ed.rt.spawn_blocking(move || {
                let d = Arc::new(engine::compare(&old.text, &new.text, &path, &options));
                main.send(move |ed| {
                    ed.with_buffer_as::<DiffBuffer, _>(id, |b, ed| {
                        // Keep what other files computed: going back is instant.
                        if b.cache.len() > 64 {
                            b.cache.clear();
                        }
                        b.cache.insert(key, d.clone());
                        if b.file_token == token {
                            b.show_diff(ed, d, wanted);
                        }
                    });
                });
            });
        });
    }

    fn show_diff(&mut self, ed: &mut Editor, d: Arc<FileDiff>, wanted: (String, String)) {
        let same_file = self.shown.as_ref().is_some_and(|s| s.0 == wanted.0);
        // Stay on the same line when the file changes underneath.
        let at = self.rows.get(self.cursor).copied();
        self.loading = false;
        self.note = None;
        self.shown = Some(wanted.clone());
        self.diff = Some(d.clone());
        self.rebuild_rows();
        if same_file {
            if let Some(r) = at {
                self.cursor = self.find_row(r.old, r.new);
            }
        } else {
            self.cursor = 0;
            self.anchor = None;
            self.scroll = 0.0;
            self.hscroll = 0.0;
        }
        // Comments follow their code.
        let mut moved = false;
        for c in self.review.comments.iter_mut().filter(|c| c.path == wanted.0) {
            let lines = if c.side == Which::New { &d.new.text } else { &d.old.text };
            let was = (c.start, c.end, c.outdated);
            c.reanchor(lines);
            moved |= was != (c.start, c.end, c.outdated);
        }
        if moved {
            self.persist(ed);
        }
        self.items_dirty = true;
        match self.land.take() {
            Some(Land::FirstChange) => self.go_to_change(0),
            Some(Land::LastChange) => self.go_to_change(usize::MAX),
            None => {}
        }
    }

    fn find_row(&self, old: Option<u32>, new: Option<u32>) -> usize {
        let hit = |want: Option<u32>, have: Option<u32>| want.is_some() && want == have;
        self.rows.iter().position(|r| hit(new, r.new)).or_else(|| self.rows.iter().position(|r| hit(old, r.old))).unwrap_or(0).min(self.rows.len().saturating_sub(1))
    }

    /// The rows of the view in use.
    fn rebuild_rows(&mut self) {
        let Some(d) = self.diff.clone() else { return };
        self.rows = if self.split_now { d.rows.clone() } else { d.unified_rows() };
        self.hunks = engine::hunks_of(&self.rows, &d.old.lines, &d.new.lines);
        self.items_dirty = true;
    }

    /// Changes `n` and `p` stop at: not the ones only in spacing, unless that is all there is.
    fn stops(&self) -> Vec<usize> {
        let real: Vec<usize> = self.hunks.iter().filter(|h| !h.format_only).map(|h| h.rows.start).collect();
        if real.is_empty() { self.hunks.iter().map(|h| h.rows.start).collect() } else { real }
    }

    /// Put the cursor on change `i` (past the end: the last).
    fn go_to_change(&mut self, i: usize) {
        let stops = self.stops();
        if let Some(r) = stops.get(i).or(stops.last()) {
            self.cursor = *r;
            self.anchor = None;
            self.reveal = true;
            // The side the change is on, when it is on one only.
            if let Some(row) = self.rows.get(*r) {
                if row.new.is_some() {
                    self.side = Which::New;
                } else if row.old.is_some() {
                    self.side = Which::Old;
                }
            }
        }
    }

    /// Which change the cursor is in or past: (1-based, how many).
    pub fn change_position(&self) -> (usize, usize) {
        let stops = self.stops();
        (stops.iter().filter(|s| **s <= self.cursor).count(), stops.len())
    }

    pub fn select_file(&mut self, ed: &mut Editor, path: &str) {
        if self.current.as_deref() != Some(path) {
            // What is being written stays on the file it is about.
            self.commit_comment(ed);
            self.clear_file();
            self.current = Some(path.to_string());
            self.land = Some(Land::FirstChange);
        }
        self.load(ed);
    }

    /// The next or previous file: a change of file lands on its first or last change.
    pub fn step_file(&mut self, ed: &mut Editor, delta: isize, land: bool) -> bool {
        let at = self.current.as_deref().and_then(|p| self.files.iter().position(|f| f.path == p));
        let Some(at) = at else { return false };
        let to = at as isize + delta;
        if to < 0 || to >= self.files.len() as isize {
            return false;
        }
        let path = self.files[to as usize].path.clone();
        self.commit_comment(ed);
        self.clear_file();
        self.current = Some(path);
        self.land = Some(if land && delta < 0 { Land::LastChange } else { Land::FirstChange });
        self.load(ed);
        true
    }

    /// The next or previous change, on into the next or previous file.
    pub fn step_change(&mut self, ed: &mut Editor, delta: isize) -> Option<String> {
        let stops = self.stops();
        let target = if delta > 0 {
            stops.iter().find(|s| **s > self.cursor).copied()
        } else {
            stops.iter().rev().find(|s| **s < self.cursor).copied()
        };
        match target {
            Some(r) => {
                let i = stops.iter().position(|s| *s == r).unwrap_or(0);
                self.go_to_change(i);
                None
            }
            None if self.step_file(ed, delta, true) => None,
            None => Some(if delta > 0 { "That was the last change".to_string() } else { "That was the first change".to_string() }),
        }
    }

    pub fn persist(&self, ed: &mut Editor) {
        // Before the saved review was read, saving would overwrite it.
        if !self.review_loaded {
            return;
        }
        let mut saved = self.review.clone();
        saved.spec = self.spec.clone();
        saved.file = self.current.clone();
        saved.writing = self.editor.as_ref().filter(|e| !e.area.doc.is_empty()).map(|e| review::Writing { id: e.id, path: e.path.clone(), side: e.side, start: e.start, end: e.end, ask: e.ask, text: e.area.text() });
        if let Err(e) = review::save(self.machine.as_deref(), &self.root, &saved) {
            ed.warn(format!("Couldn't save the review: {e:#}"));
        }
    }

    /// Delete every comment, sent or not. How many there were.
    pub fn clear_comments(&mut self, ed: &mut Editor) -> usize {
        let n = self.review.comments.len();
        self.review.comments.clear();
        self.items_dirty = true;
        self.persist(ed);
        n
    }

    /// Comments on a file still open (sent or not).
    pub fn comments_on(&self, path: &str) -> usize {
        self.review.comments.iter().filter(|c| c.path == path && c.state != State::Resolved).count()
    }

    /// The same for everything in a folder.
    fn comments_under(&self, dir: &str) -> usize {
        let prefix = format!("{dir}/");
        self.review.comments.iter().filter(|c| c.path.starts_with(&prefix) && c.state != State::Resolved).count()
    }

    pub fn drafts(&self) -> Vec<&Comment> {
        self.review.comments.iter().filter(|c| c.state == State::Draft && !c.text.trim().is_empty()).collect()
    }

    fn side_of(&self, which: Which) -> Option<&engine::Side> {
        self.diff.as_deref().map(|d| if which == Which::New { &d.new } else { &d.old })
    }

    /// The row's line on `which`, 1-based.
    fn line_at(&self, row: usize, which: Which) -> Option<u32> {
        let r = self.rows.get(row)?;
        (if which == Which::New { r.new } else { r.old }).map(|l| l + 1)
    }

    /// The lines selected, or the cursor's: (side, first, last), 1-based.
    pub fn selected_lines(&self) -> Option<(Which, u32, u32)> {
        let (a, b) = (self.anchor.unwrap_or(self.cursor), self.cursor);
        let (a, b) = (a.min(b), a.max(b));
        let on = |which: Which| -> Option<(u32, u32)> {
            let lines: Vec<u32> = (a..=b).filter_map(|r| self.line_at(r, which)).collect();
            Some((*lines.iter().min()?, *lines.iter().max()?))
        };
        let other = if self.side == Which::New { Which::Old } else { Which::New };
        on(self.side).map(|(s, e)| (self.side, s, e)).or_else(|| on(other).map(|(s, e)| (other, s, e)))
    }

    /// Start a comment on these lines.
    pub fn begin_comment(&mut self, ed: &mut Editor, side: Which, start: u32, end: u32, ask: Ask, text: &str) {
        let Some(path) = self.current.clone() else { return };
        let mut area = TextArea::new(text);
        area.move_to_end();
        self.editor = Some(CommentEditor { id: None, path, side, start, end, ask, area });
        self.anchor = None;
        self.items_dirty = true;
        self.reveal = true;
        self.enter_editing(ed);
    }

    /// Change a comment.
    pub fn begin_edit(&mut self, ed: &mut Editor, id: u64) {
        let Some(c) = self.review.comments.iter().find(|c| c.id == id).cloned() else { return };
        let mut area = TextArea::new(&c.text);
        area.move_to_end();
        self.editor = Some(CommentEditor { id: Some(id), path: c.path, side: c.side, start: c.start, end: c.end, ask: c.ask, area });
        self.items_dirty = true;
        self.reveal = true;
        self.enter_editing(ed);
    }

    fn enter_editing(&mut self, ed: &mut Editor) {
        let Some(id) = self.id else { return };
        if let Some(m) = ed.meta_mut(id) {
            m.major = EDITING.to_string();
        }
        if crate::packages::vim::state_of(ed, id).is_some() {
            crate::packages::vim::set_state(ed, id, crate::packages::vim::State::Insert);
        }
    }

    /// Stop writing: the page's keys apply again.
    pub fn leave_editing(&mut self, ed: &mut Editor) {
        self.editor = None;
        self.items_dirty = true;
        let Some(id) = self.id else { return };
        if let Some(m) = ed.meta_mut(id) {
            m.major = KIND.to_string();
        }
        if crate::packages::vim::state_of(ed, id).is_some() {
            crate::packages::vim::set_state(ed, id, crate::packages::vim::State::Normal);
        }
    }

    /// Keep what is being written as a comment. Its id; None when there was nothing to keep.
    pub fn commit_comment(&mut self, ed: &mut Editor) -> Option<u64> {
        let e = self.editor.as_ref()?;
        let text = e.area.text().trim().to_string();
        if text.is_empty() {
            self.leave_editing(ed);
            return None;
        }
        let (edited, path, side, start, end, ask) = (e.id, e.path.clone(), e.side, e.start, e.end, e.ask);
        let id = match edited.and_then(|id| self.review.comments.iter_mut().find(|c| c.id == id)) {
            Some(c) => {
                c.text = text;
                c.ask = ask;
                c.state = State::Draft;
                c.id
            }
            None => self.add_comment(path, side, start, end, text, ask),
        };
        self.leave_editing(ed);
        self.persist(ed);
        ed.run_hook("diff-comment-added", &serde_json::json!({"id": id, "path": self.review.comments.iter().find(|c| c.id == id).map(|c| c.path.clone())}));
        Some(id)
    }

    /// A new comment on the current file's lines, as they are now.
    pub fn add_comment(&mut self, path: String, side: Which, start: u32, end: u32, text: String, ask: Ask) -> u64 {
        let code: Vec<String> = match (self.current.as_deref() == Some(path.as_str()), self.side_of(side)) {
            (true, Some(s)) => s.text.iter().skip(start.saturating_sub(1) as usize).take(end.saturating_sub(start) as usize + 1).cloned().collect(),
            _ => Vec::new(),
        };
        self.review.next_id = self.review.next_id.max(self.review.comments.iter().map(|c| c.id).max().unwrap_or(0)) + 1;
        let id = self.review.next_id;
        self.review.comments.push(Comment { id, path, side, start, end, text, ask, state: State::Draft, code, outdated: false });
        self.items_dirty = true;
        id
    }

    pub fn remove_comment(&mut self, ed: &mut Editor, id: u64) {
        self.review.comments.retain(|c| c.id != id);
        self.items_dirty = true;
        self.persist(ed);
    }

    pub fn toggle_resolved(&mut self, ed: &mut Editor, id: u64) {
        if let Some(c) = self.review.comments.iter_mut().find(|c| c.id == id) {
            c.state = if c.state == State::Resolved { State::Draft } else { State::Resolved };
        }
        self.items_dirty = true;
        self.persist(ed);
    }

    /// The comment at or just below the cursor's row.
    pub fn comment_at_cursor(&self) -> Option<u64> {
        let i = *self.row_item.get(self.cursor)? as usize;
        self.items[i + 1..].iter().take_while(|it| !matches!(it, Item::Row(_) | Item::Fold { .. })).find_map(|it| match it {
            Item::Comment(id) => Some(*id),
            _ => None,
        })
    }

    pub fn toggle_viewed(&mut self, ed: &mut Editor) -> bool {
        let Some(e) = self.current.as_deref().and_then(|p| self.entry(p)).cloned() else { return false };
        let now = self.review.viewed.remove(&e.path).is_none();
        if now {
            self.review.viewed.insert(e.path.clone(), e.signature);
        }
        self.persist(ed);
        now
    }

    pub fn expand_fold(&mut self, row: usize) {
        if let Some(r) = self.rows.get(row) {
            self.expanded.insert((r.old, r.new));
            self.items_dirty = true;
            self.reveal = true;
        }
    }

    /// Open or close everything folded.
    pub fn toggle_expand_all(&mut self) {
        self.expand_all = !self.expand_all;
        self.expanded.clear();
        self.items_dirty = true;
        self.reveal = true;
    }

    pub fn set_whitespace(&mut self, ed: &mut Editor, w: Whitespace) {
        self.whitespace = w;
        self.load(ed);
    }

    pub fn set_split(&mut self, split: bool) {
        self.split = split;
        self.laid_out = None;
    }

    /// Go through the rows of the file for `needle`, from after the cursor.
    pub fn search_next(&mut self, needle: &str) -> bool {
        let Some(d) = self.diff.clone() else { return false };
        let n = self.rows.len();
        let needle = needle.to_lowercase();
        let hit = |r: &Row| {
            let has = |l: Option<u32>, s: &engine::Side| l.is_some_and(|l| s.text[l as usize].to_lowercase().contains(&needle));
            has(r.new, &d.new) || has(r.old, &d.old)
        };
        for k in 1..=n {
            let at = (self.cursor + k) % n;
            if hit(&self.rows[at]) {
                self.cursor = at;
                self.anchor = None;
                if matches!(self.row_item.get(at).and_then(|i| self.items.get(*i as usize)), Some(Item::Fold { .. })) {
                    self.expand_fold_containing(at);
                }
                self.reveal = true;
                return true;
            }
        }
        false
    }

    fn expand_fold_containing(&mut self, row: usize) {
        if let Some(Item::Fold { from, .. }) = self.row_item.get(row).and_then(|i| self.items.get(*i as usize)).cloned() {
            self.expand_fold(from);
        }
    }

    /// Move the cursor `delta` shown rows (a fold counts as one).
    pub fn move_cursor(&mut self, delta: isize, extend: bool) {
        self.ensure_items();
        if self.rows.is_empty() {
            return;
        }
        if extend && self.anchor.is_none() {
            self.anchor = Some(self.cursor);
        } else if !extend {
            self.anchor = None;
        }
        let mut item = self.row_item.get(self.cursor).copied().unwrap_or(0) as isize;
        let step = delta.signum();
        for _ in 0..delta.abs() {
            let mut next = item + step;
            while next >= 0 && (next as usize) < self.items.len() && !matches!(self.items[next as usize], Item::Row(_) | Item::Fold { .. }) {
                next += step;
            }
            if next < 0 || next as usize >= self.items.len() {
                break;
            }
            item = next;
        }
        match self.items.get(item as usize) {
            Some(Item::Row(r)) => self.cursor = *r,
            Some(Item::Fold { from, .. }) => self.cursor = *from,
            _ => {}
        }
        self.reveal = true;
    }

    pub fn cursor_to(&mut self, end: bool) {
        self.ensure_items();
        self.anchor = None;
        self.cursor = if end { self.rows.len().saturating_sub(1) } else { 0 };
        self.reveal = true;
    }

    /// Whether the cursor is on a fold, and open it. False when it isn't on one.
    pub fn open_fold_at_cursor(&mut self) -> bool {
        self.ensure_items();
        if let Some(Item::Fold { from, .. }) = self.row_item.get(self.cursor).and_then(|i| self.items.get(*i as usize)).cloned() {
            self.expand_fold(from);
            return true;
        }
        false
    }

    /// The page's items from its rows, folds and comments.
    fn ensure_items(&mut self) {
        if !self.items_dirty {
            return;
        }
        self.items_dirty = false;
        self.laid_out = None;
        let path = self.current.clone().unwrap_or_default();
        let context = CONTEXT.with(|c| c.get());
        let n = self.rows.len();
        let mut keep = vec![self.expand_all; n];
        for h in &self.hunks {
            let from = h.rows.start.saturating_sub(context);
            let to = (h.rows.end + context).min(n);
            keep[from..to].iter_mut().for_each(|k| *k = true);
        }
        // Lines with comments stay in view, the composer's too.
        let open = self.editor.as_ref().filter(|e| e.path == path).map(|e| (e.side, e.end));
        let anchored = |which: Which, line: u32| self.review.comments.iter().any(|c| c.path == path && c.side == which && !c.outdated && c.end == line) || open == Some((which, line));
        let mut after: HashMap<usize, Vec<u64>> = HashMap::new();
        let mut composer_row = None;
        for (i, r) in self.rows.iter().enumerate() {
            let new = r.new.map(|l| l + 1);
            let old = r.old.map(|l| l + 1);
            if new.is_some_and(|l| anchored(Which::New, l)) || old.is_some_and(|l| anchored(Which::Old, l)) {
                keep[i] = true;
                for c in self.review.comments.iter().filter(|c| c.path == path && !c.outdated) {
                    if (c.side == Which::New && new == Some(c.end)) || (c.side == Which::Old && old == Some(c.end)) {
                        after.entry(i).or_default().push(c.id);
                    }
                }
                if open.is_some_and(|(s, e)| (s == Which::New && new == Some(e)) || (s == Which::Old && old == Some(e))) {
                    composer_row = Some(i);
                }
            }
        }
        for (i, r) in self.rows.iter().enumerate() {
            if self.expanded.contains(&(r.old, r.new)) {
                // The whole fold that started here.
                let mut j = i;
                while j < n && !keep[j] {
                    keep[j] = true;
                    j += 1;
                }
            }
        }
        let mut items = Vec::with_capacity(n + 8);
        let mut row_item = vec![0u32; n];
        // Comments whose code is gone: above the file, still readable.
        for c in self.review.comments.iter().filter(|c| c.path == path && c.outdated) {
            items.push(Item::Comment(c.id));
        }
        // A comment whose line is gone still needs somewhere to be written.
        if open.is_some() && composer_row.is_none() {
            items.push(Item::Composer);
        }
        let mut i = 0;
        while i < n {
            if keep[i] {
                row_item[i] = items.len() as u32;
                items.push(Item::Row(i));
                let editing = self.editor.as_ref().and_then(|e| e.id);
                for id in after.get(&i).into_iter().flatten() {
                    if editing != Some(*id) {
                        items.push(Item::Comment(*id));
                    }
                }
                if composer_row == Some(i) {
                    items.push(Item::Composer);
                }
                i += 1;
            } else {
                let to = i + keep[i..].iter().take_while(|k| !**k).count();
                // Folding one line takes as much room as showing it.
                if to - i == 1 {
                    row_item[i] = items.len() as u32;
                    items.push(Item::Row(i));
                } else {
                    let at = items.len() as u32;
                    row_item[i..to].iter_mut().for_each(|x| *x = at);
                    items.push(Item::Fold { from: i, to });
                }
                i = to;
            }
        }
        self.items = items;
        self.row_item = row_item;
    }

    /// The page as text: what scripts and tests read.
    pub fn as_text(&mut self) -> String {
        self.ensure_items();
        let mut s = String::new();
        if let Some(spec) = &self.spec {
            s.push_str(&format!("{}\n", describe(spec)));
        }
        if let Some(e) = &self.error {
            s.push_str(&format!("error: {e}\n"));
        }
        for f in &self.files {
            let viewed = if self.review.viewed.contains_key(&f.path) { " viewed" } else { "" };
            s.push_str(&format!("{} {} +{} -{}{viewed}\n", f.status, f.path, f.added, f.removed));
        }
        if let Some(p) = &self.current {
            s.push_str(&format!("== {p}\n"));
        }
        if let Some(n) = &self.note {
            s.push_str(&format!("{n}\n"));
        }
        let Some(d) = self.diff.clone() else { return s };
        let sign = |k: Kind, minus: bool| match k {
            Kind::Same => ' ',
            Kind::Format => '~',
            Kind::Moved => '>',
            Kind::Changed if minus => '-',
            Kind::Changed => '+',
        };
        for it in &self.items {
            match it {
                Item::Row(r) => {
                    let r = self.rows[*r];
                    match (r.old, r.new) {
                        (Some(a), Some(b)) if d.old.lines[a as usize].kind == Kind::Same && d.new.lines[b as usize].kind == Kind::Same => s.push_str(&format!("  {} {} | {}\n", a + 1, b + 1, d.new.text[b as usize])),
                        (a, b) => {
                            if let Some(a) = a {
                                s.push_str(&format!("{} {} - | {}\n", sign(d.old.lines[a as usize].kind, true), a + 1, d.old.text[a as usize]));
                            }
                            if let Some(b) = b {
                                s.push_str(&format!("{} - {} | {}\n", sign(d.new.lines[b as usize].kind, false), b + 1, d.new.text[b as usize]));
                            }
                        }
                    }
                }
                Item::Fold { from, to } => s.push_str(&format!("⋯ {} unchanged lines\n", to - from)),
                Item::Comment(id) => {
                    if let Some(c) = self.review.comments.iter().find(|c| c.id == *id) {
                        s.push_str(&format!("[{} {}] {}: {}\n", c.ask.name(), c.state.name(), c.place(), c.text));
                    }
                }
                Item::Composer => {
                    if let Some(e) = &self.editor {
                        s.push_str(&format!("[writing {}] {}\n", e.ask.name(), e.area.text()));
                    }
                }
            }
        }
        s
    }
}

thread_local! {
    /// `diff-context-lines` as last read: folding needs it where no editor is at hand.
    static CONTEXT: std::cell::Cell<usize> = const { std::cell::Cell::new(3) };
}

/// Which comparison a page starts with (`diff-default-source`).
pub fn default_spec(ed: &Editor, refs: &DiffRefs) -> DiffSpec {
    let set = ed.var_str("diff-default-source").unwrap_or_else(|| "auto".into());
    if set != "auto"
        && let Some(s) = parse_source(&set)
    {
        return s;
    }
    let base = ed.var_str("diff-base-branch").filter(|b| !b.is_empty()).or_else(|| refs.default_branch.clone());
    match base {
        // On a branch of its own: what that branch would bring. On the main branch: what isn't
        // committed yet.
        Some(b) if !refs.detached && refs.head != b => against(&b),
        _ => uncommitted(),
    }
}

/// A file or folder in the tree.
#[derive(Debug, Clone, PartialEq)]
pub struct TreeRow {
    pub depth: usize,
    pub name: String,
    /// The folder's path, for closing it; or the file's.
    pub path: String,
    pub file: Option<usize>,
}

/// The tree of `files`: folders first at each level, those with a single folder in them joined
/// (`crates/serpis/src`).
pub fn tree_rows(files: &[DiffEntry], closed: &HashSet<String>) -> Vec<TreeRow> {
    #[derive(Default)]
    struct Node {
        dirs: BTreeMap<String, Node>,
        files: Vec<(String, usize)>,
    }
    let mut top = Node::default();
    for (i, f) in files.iter().enumerate() {
        let mut parts: Vec<&str> = f.path.split('/').collect();
        let name = parts.pop().unwrap_or_default().to_string();
        let mut at = &mut top;
        for d in parts {
            at = at.dirs.entry(d.to_string()).or_default();
        }
        at.files.push((name, i));
    }
    fn emit(n: &Node, prefix: &str, depth: usize, closed: &HashSet<String>, files: &[DiffEntry], out: &mut Vec<TreeRow>) {
        for (name, child) in &n.dirs {
            let (mut label, mut c) = (name.clone(), child);
            while c.files.is_empty() && c.dirs.len() == 1 {
                let (next, inner) = c.dirs.iter().next().expect("one folder");
                label = format!("{label}/{next}");
                c = inner;
            }
            let path = format!("{prefix}{label}");
            out.push(TreeRow { depth, name: label, path: path.clone(), file: None });
            if !closed.contains(&path) {
                emit(c, &format!("{path}/"), depth + 1, closed, files, out);
            }
        }
        for (name, i) in &n.files {
            out.push(TreeRow { depth, name: name.clone(), path: files[*i].path.clone(), file: Some(*i) });
        }
    }
    let mut out = Vec::new();
    emit(&top, "", 0, closed, files, &mut out);
    out
}

fn status_color(t: &Theme, status: &str) -> Color32 {
    match status {
        "A" => t.green,
        "D" => t.red,
        "R" | "C" => t.purple,
        "U" => t.yellow,
        _ => t.blue,
    }
}

/// One screen line of code: syntax colors, with the changed bytes on `mark_bg`.
#[allow(clippy::too_many_arguments)]
fn code_job(text: &str, slice: std::ops::Range<usize>, spans: Option<&Spans>, marks: &[std::ops::Range<usize>], base: Color32, mark_bg: Color32, font: &FontId, t: &Theme) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    let mut cuts: Vec<usize> = vec![slice.start, slice.end];
    let inside = |at: usize| at > slice.start && at < slice.end;
    for r in spans.into_iter().flatten().map(|(r, _)| r).chain(marks) {
        if inside(r.start) {
            cuts.push(r.start);
        }
        if inside(r.end) {
            cuts.push(r.end);
        }
    }
    cuts.sort_unstable();
    cuts.dedup();
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let Some(piece) = text.get(a..b) else { continue };
        let face = spans.and_then(|s| s.iter().find(|(r, _)| r.contains(&a))).map(|(_, f)| *f);
        let color = face.map_or(base, |f| t.face(f).1);
        let marked = marks.iter().any(|m| m.contains(&a));
        job.append(
            piece,
            0.0,
            TextFormat {
                font_id: font.clone(),
                color,
                background: if marked { mark_bg } else { Color32::TRANSPARENT },
                italics: face == Some(Face::SynComment),
                ..Default::default()
            },
        );
    }
    job
}

/// Diagonal lines where a side has no line, so a gap can't be read as an empty line.
fn hatch(p: &egui::Painter, r: Rect, color: Color32, k: f32) {
    let c = p.with_clip_rect(r.intersect(p.clip_rect()));
    let step = 7.0 * k;
    let mut x = r.left() - r.height();
    // Fixed to the page, not the row, so the stripes of neighboring rows line up.
    x -= (x + r.top()).rem_euclid(step);
    while x < r.right() {
        c.line_segment([egui::pos2(x, r.bottom()), egui::pos2(x + r.height(), r.top())], Stroke::new(1.0, color));
        x += step;
    }
}

/// Measures of the code column.
#[derive(Clone, Copy)]
struct Metrics {
    k: f32,
    char_w: f32,
    line_h: f32,
    /// Width of one line number column.
    num_w: f32,
}

impl DiffBuffer {
    fn row_screen_lines(&self, row: usize, cols: usize) -> usize {
        if !self.wrap {
            return 1;
        }
        let Some(d) = self.diff.as_deref() else { return 1 };
        let r = self.rows[row];
        let of = |s: &engine::Side, l: Option<u32>| l.map_or(1, |l| wrap_points(&s.text[l as usize], cols).len() + 1);
        of(&d.old, r.old).max(of(&d.new, r.new))
    }

    /// Lay the items out for this width.
    fn layout(&mut self, painter: &egui::Painter, t: &Theme, m: Metrics, width: f32, cols: usize, composer_h: f32) {
        let key = (width.to_bits(), cols as u32, composer_h.to_bits(), self.wrap, self.split_now);
        if self.laid_out == Some(key) && self.ys.len() == self.items.len() + 1 {
            return;
        }
        self.laid_out = Some(key);
        let k = m.k;
        let fold_h = (m.line_h * 1.5).round();
        let mut ys = Vec::with_capacity(self.items.len() + 1);
        let mut y = 0.0;
        for it in &self.items {
            ys.push(y);
            y += match it {
                Item::Row(r) => self.row_screen_lines(*r, cols) as f32 * m.line_h,
                Item::Fold { .. } => fold_h,
                Item::Composer => composer_h,
                Item::Comment(id) => self.review.comments.iter().find(|c| c.id == *id).map_or(0.0, |c| comment_height(painter, t, c, width, k)),
            };
        }
        ys.push(y);
        self.ys = ys;
    }

    /// The item at height `y` of the content.
    fn item_at(&self, y: f32) -> Option<usize> {
        if self.items.is_empty() || y < 0.0 {
            return None;
        }
        let i = self.ys.partition_point(|top| *top <= y);
        (i > 0 && i <= self.items.len()).then(|| i - 1)
    }
}

/// A bubble and a count, ending at `right`. Where it starts.
fn comment_count(p: &egui::Painter, t: &Theme, right: f32, cy: f32, n: usize, font: &FontId) -> f32 {
    let label = n.to_string();
    let w = chrome::text_width(p, &label, font.clone());
    chrome::text(p, egui::pos2(right - w, cy), &label, font.clone(), t.accent, w + 1.0);
    let size = font.size * 1.05;
    let from = right - w - 3.0 - size;
    chrome::bubble(p, egui::pos2(from + size / 2.0, cy), size, t.accent);
    from
}

fn comment_font(t: &Theme) -> FontId {
    FontId::proportional(t.font_size - 1.0)
}

fn comment_card_width(width: f32, k: f32) -> f32 {
    (width - 28.0 * k).clamp(120.0, 760.0 * k)
}

fn comment_height(p: &egui::Painter, t: &Theme, c: &Comment, width: f32, k: f32) -> f32 {
    let inner = comment_card_width(width, k) - 24.0 * k;
    let g = p.layout(c.text.clone(), comment_font(t), t.fg, inner);
    (g.size().y + 54.0 * k).round()
}

/// Something clicked this frame, run when the frame is done.
#[derive(Clone)]
struct Act(&'static str, Vec<serde_json::Value>);

/// A control: what it says, in a row that wraps. Returns whether it was clicked.
struct Controls<'a> {
    ui: &'a mut egui::Ui,
    painter: &'a egui::Painter,
    t: &'a Theme,
    font: FontId,
    id: egui::Id,
    left: f32,
    right: f32,
    at: Pos2,
    h: f32,
    gap: f32,
    hits: &'a mut Vec<(String, Rect)>,
}

impl Controls<'_> {
    fn width(&self, label: &str) -> f32 {
        chrome::text_width(self.painter, label, self.font.clone()) + 20.0 * self.t.ui_scale()
    }

    fn place(&mut self, w: f32) -> Rect {
        if self.at.x > self.left && self.at.x + w > self.right {
            self.at = egui::pos2(self.left, self.at.y + self.h + self.gap);
        }
        let w = w.min(self.right - self.left);
        let r = Rect::from_min_size(self.at, egui::vec2(w, self.h));
        self.at.x += w + self.gap;
        r
    }

    fn button(&mut self, key: &str, label: &str, tip: &str, on: bool) -> bool {
        let w = self.width(label);
        let r = self.place(w);
        self.at_rect(r, key, label, tip, on)
    }

    /// A button that opens a list: a chevron after its label.
    fn menu_button(&mut self, key: &str, label: &str, tip: &str, on: bool) -> bool {
        let k = self.t.ui_scale();
        let w = self.width(label) + 14.0 * k;
        let r = self.place(w);
        let clicked = self.at_rect(r, key, label, tip, on);
        chrome::chevron(self.painter, egui::pos2(r.right() - 11.0 * k, r.center().y), true, self.t.fg_faint);
        clicked
    }

    fn at_rect(&mut self, r: Rect, key: &str, label: &str, tip: &str, on: bool) -> bool {
        let k = self.t.ui_scale();
        let resp = self.ui.interact(r, self.id.with(key), Sense::click()).on_hover_text(tip);
        let (bg, fg) = if on {
            (chrome::tint(self.t.accent, 0.18), self.t.accent)
        } else if resp.hovered() {
            (chrome::hover_bg(self.t), self.t.fg)
        } else {
            (self.t.chip(), self.t.fg_dim)
        };
        self.painter.rect_filled(r, 7.0 * k, bg);
        chrome::text(self.painter, egui::pos2(r.left() + 10.0 * k, r.center().y), label, self.font.clone(), fg, r.width() - 14.0 * k);
        self.hits.push((key.to_string(), r));
        resp.clicked()
    }

    fn next_row(&self) -> f32 {
        self.at.y + self.h
    }
}

impl Buffer for DiffBuffer {
    fn kind(&self) -> &str {
        KIND
    }

    fn poll(&mut self, ed: &mut Editor) {
        let Some(id) = self.id else { return };
        let every = ed.var("diff-refresh-seconds").and_then(serde_json::Value::as_f64).unwrap_or(2.0);
        if every <= 0.0 || self.listing || self.spec.is_none() || !ed.visible_buffers().contains(&id) {
            return;
        }
        // Rewriting history can't happen under a fixed pair of commits, but a branch moves.
        if self.listed.is_none_or(|t| t.elapsed() >= Duration::from_secs_f64(every)) {
            self.refresh(ed);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, rect: Rect, focused: bool, ed: &mut Editor) {
        let t = ed.theme.clone();
        let k = t.ui_scale();
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, t.bg);
        CONTEXT.with(|c| c.set(ed.var("diff-context-lines").and_then(serde_json::Value::as_u64).unwrap_or(3) as usize));
        let id = ui.scope_id().with("diff-view");
        let mut hits: Vec<(String, Rect)> = Vec::new();
        let mut acts: Vec<Act> = Vec::new();
        let small = FontId::proportional(t.font_size - 2.5);
        let ui_font = FontId::proportional(t.font_size - 1.5);
        let mono = t.mono();

        // The controls.
        let (pad, top) = (14.0 * k, rect.top() + 10.0 * k);
        let mut pick_source = None;
        let unified_only = !self.split_now && self.split;
        let default_base = ed.var_str("diff-base-branch").filter(|b| !b.is_empty()).or_else(|| self.refs.as_ref().and_then(|r| r.default_branch.clone()));
        let (controls_bottom, toggle_split, toggle_wrap, toggle_all, toggle_tree) = {
            let mut c = Controls { ui, painter: &painter, t: &t, font: ui_font.clone(), id, left: rect.left() + pad, right: rect.right() - pad, at: egui::pos2(rect.left() + pad, top), h: 26.0 * k, gap: 6.0 * k, hits: &mut hits };
            let label = self.spec.as_ref().map_or_else(|| "Reading the repository…".to_string(), describe);
            if c.menu_button("source", &label, "What is compared (diff-select-source, d)", false) {
                acts.push(Act("diff-select-source", vec![]));
            }
            let spec = self.spec.clone();
            if c.button("uncommitted", "Uncommitted", "Everything not committed yet, untracked files too", spec == Some(uncommitted())) {
                pick_source = Some(uncommitted());
            }
            if c.button("staged", "Staged", "What the next commit would contain", spec == Some(staged())) {
                pick_source = Some(staged());
            }
            if let Some(b) = &default_base {
                let on = spec == Some(against(b));
                if c.button("branch", &format!("vs {b}"), "What this branch would bring, from where it forked", on) {
                    pick_source = Some(against(b));
                }
            }
            let view = if self.split_now { "Side by side" } else { "Unified" };
            let tip = if unified_only { "Too narrow for side by side (diff-split-min-width)" } else { "Side by side or unified (diff-toggle-view, s)" };
            let toggle_split = c.button("view", view, tip, false);
            if c.button("whitespace", &format!("Spacing: {}", self.whitespace.name()), "smart: spacing changes dim, and n and p skip them. hide: they look unchanged. exact: every byte counts (diff-cycle-whitespace, w)", self.whitespace != Whitespace::Smart) {
                acts.push(Act("diff-cycle-whitespace", vec![]));
            }
            let toggle_wrap = c.button("wrap", "Wrap", "Wrap long lines (diff-toggle-wrap, z)", self.wrap);
            let toggle_all = c.button("expand", "Expand", "Show or fold the unchanged lines (diff-toggle-expand, x)", self.expand_all);
            let toggle_tree = c.button("tree", "Files", "The list of files (diff-toggle-tree, b)", self.show_tree);
            (c.next_row(), toggle_split, toggle_wrap, toggle_all, toggle_tree)
        };
        if let Some(s) = pick_source {
            self.set_spec(ed, s);
        }
        if toggle_split {
            self.set_split(!self.split);
        }
        if toggle_wrap {
            self.wrap = !self.wrap;
            self.laid_out = None;
        }
        if toggle_tree {
            self.show_tree = !self.show_tree;
        }
        if toggle_all {
            self.toggle_expand_all();
        }
        let mut y = controls_bottom + 10.0 * k;
        painter.hline(rect.left()..=rect.right(), y, Stroke::new(1.0, t.border));
        y += 1.0;

        // The review bar, while there are comments.
        let drafts = self.drafts().len();
        let total_comments = self.review.comments.len();
        let bar_h = if total_comments > 0 { 44.0 * k } else { 0.0 };
        let body = Rect::from_min_max(egui::pos2(rect.left(), y), egui::pos2(rect.right(), rect.bottom() - bar_h));
        if total_comments > 0 {
            let bar = Rect::from_min_max(egui::pos2(rect.left(), body.bottom()), rect.max);
            painter.rect_filled(bar, 0.0, t.bg_alt);
            painter.hline(rect.left()..=rect.right(), bar.top(), Stroke::new(1.0, t.border));
            let cy = bar.center().y;
            let plural = |n: usize| if n == 1 { "comment" } else { "comments" };
            // Comments on files this comparison doesn't list can't be seen: say so.
            let elsewhere = self.drafts().iter().filter(|c| self.entry(&c.path).is_none()).count();
            let what = match (drafts, elsewhere) {
                (0, _) => format!("{total_comments} {} sent", plural(total_comments)),
                (n, 0) => format!("{n} {} to send", plural(n)),
                (n, e) => format!("{n} {} to send, {e} on files not listed here", plural(n)),
            };
            let (bh, mut right) = (26.0 * k, bar.right() - pad);
            let mut row: Vec<(&str, Rect)> = Vec::new();
            if drafts > 0 {
                let send_w = chrome::text_width(&painter, "Send review", ui_font.clone()) + 24.0 * k;
                let send = Rect::from_min_size(egui::pos2(right - send_w, cy - bh / 2.0), egui::vec2(send_w, bh));
                // A chat has no title before its first message; its name will do.
                let target = self.target.filter(|b| ed.buffers.contains_key(*b));
                let name = target.and_then(|b| ed.with_buffer(b, |b, _| b.title())).filter(|n| !n.is_empty()).or_else(|| target.and_then(|b| ed.meta(b).map(|m| m.name.clone())));
                let to = format!("To: {}", name.unwrap_or_else(|| "choose…".into()));
                let room = (send.left() - bar.left() - 240.0 * k).max(70.0 * k);
                let to_w = (chrome::text_width(&painter, &to, ui_font.clone()) + 36.0 * k).min(room);
                let to_r = Rect::from_min_size(egui::pos2(send.left() - 6.0 * k - to_w, cy - bh / 2.0), egui::vec2(to_w, bh));
                let resp = ui.interact(send, id.with("send"), Sense::click()).on_hover_text("Send the comments to the agent (diff-send-review, S)");
                painter.rect_filled(send, 7.0 * k, if resp.hovered() { chrome::tint(t.accent, 0.85) } else { t.accent });
                painter.text(send.center(), egui::Align2::CENTER_CENTER, "Send review", ui_font.clone(), chrome::on(t.accent, &t));
                if resp.clicked() {
                    acts.push(Act("diff-send-review", vec![]));
                }
                let resp = ui.interact(to_r, id.with("send-target"), Sense::click()).on_hover_text("Which chat or terminal gets it (diff-select-target)");
                painter.rect_filled(to_r, 7.0 * k, if resp.hovered() { chrome::hover_bg(&t) } else { t.chip() });
                chrome::text(&painter, egui::pos2(to_r.left() + 10.0 * k, cy), &to, ui_font.clone(), t.fg_dim, to_r.width() - 28.0 * k);
                chrome::chevron(&painter, egui::pos2(to_r.right() - 11.0 * k, cy), true, t.fg_faint);
                if resp.clicked() {
                    acts.push(Act("diff-select-target", vec![]));
                }
                hits.push(("send".into(), send));
                hits.push(("send-target".into(), to_r));
                row.push(("send", send));
                row.push(("target", to_r));
                right = to_r.left() - 6.0 * k;
            }
            let clear = "Clear all";
            let clear_w = chrome::text_width(&painter, clear, ui_font.clone()) + 20.0 * k;
            let clear_r = Rect::from_min_size(egui::pos2(right - clear_w, cy - bh / 2.0), egui::vec2(clear_w, bh));
            let resp = ui.interact(clear_r, id.with("clear-comments"), Sense::click()).on_hover_text("Delete every comment of this review, after asking (diff-comments-clear, C)");
            if resp.hovered() {
                painter.rect_filled(clear_r, 7.0 * k, chrome::hover_bg(&t));
            }
            painter.text(clear_r.center(), egui::Align2::CENTER_CENTER, clear, ui_font.clone(), if resp.hovered() { t.fg } else { t.fg_dim });
            if resp.clicked() {
                acts.push(Act("diff-comments-clear", vec![]));
            }
            hits.push(("clear-comments".into(), clear_r));
            row.push(("clear", clear_r));
            chrome::text(&painter, egui::pos2(bar.left() + pad, cy), &what, ui_font.clone(), t.fg_dim, (clear_r.left() - bar.left() - pad - 8.0 * k).max(0.0));
            ed.check_row("diff-review-bar", bar, &row);
        }

        // The file tree.
        let tree_w_var = ed.var("diff-tree-width").and_then(serde_json::Value::as_f64).unwrap_or(250.0) as f32 * k;
        let tree_fits = body.width() - tree_w_var >= 360.0 * k;
        let tree_w = if self.show_tree && tree_fits && !self.files.is_empty() { tree_w_var } else { 0.0 };
        if tree_w > 0.0 {
            let tree = Rect::from_min_size(body.min, egui::vec2(tree_w, body.height()));
            self.draw_tree(ui, &painter, &t, tree, id, &mut hits, &mut acts);
            painter.vline(tree.right(), body.y_range(), Stroke::new(1.0, t.border));
        }
        let main = Rect::from_min_max(egui::pos2(body.left() + tree_w + 1.0, body.top()), body.max);

        // Nothing to show?
        let center = |msg: &str, c: Color32| {
            let g = painter.layout(msg.to_string(), ui_font.clone(), c, (main.width() - 48.0 * k).max(80.0));
            painter.galley(main.center() - g.size() / 2.0, g, c);
        };
        if let Some(e) = &self.error {
            center(e, t.red);
        } else if self.spec.is_none() || !self.loaded {
            chrome::spinner(&painter, main.center(), 9.0 * k, ui.input(|i| i.time), t.fg_faint);
            ui.ctx().request_repaint();
        } else if self.files.is_empty() {
            center("No differences here.", t.fg_faint);
        } else {
            self.draw_file(ui, &painter, &t, main, focused, id, ed, &mut hits, &mut acts, &small, &mono);
        }
        self.hits = hits;
        // What an agent writes shows up without any input event.
        ui.ctx().request_repaint_after(Duration::from_millis(500));
        for Act(cmd, params) in acts {
            ed.main_sender().send(move |ed| {
                let args = Args { params, ..Default::default() };
                if let Err(e) = ed.run_command(cmd, &args) {
                    ed.error(format!("{cmd}: {e:#}"));
                }
            });
        }
    }

    fn handle_text(&mut self, text: &str, _ed: &mut Editor) -> bool {
        let Some(e) = self.editor.as_mut() else { return false };
        crate::commands::insert_replacing_selection(&mut e.area, text);
        true
    }

    fn surface(&mut self) -> Option<&mut dyn TextSurface> {
        self.editor.as_mut().map(|e| &mut e.area as &mut dyn TextSurface)
    }

    fn surface_ref(&self) -> Option<&dyn TextSurface> {
        self.editor.as_ref().map(|e| &e.area as &dyn TextSurface)
    }

    fn full_text(&mut self) -> Option<String> {
        Some(self.as_text())
    }

    fn on_kill(&mut self, ed: &mut Editor) {
        // Quitting comes through here too: what is being written outlives it.
        self.persist(ed);
    }

    fn modeline(&self) -> Modeline {
        let name = self.root.rsplit('/').next().unwrap_or(&self.root);
        let what = self.spec.as_ref().map_or_else(String::new, describe);
        Modeline { title: format!("{what} · {name}"), status: vec![format!("{} files", self.files.len())], ..Default::default() }
    }

    fn title(&self) -> String {
        self.modeline().title
    }

    fn directory(&self) -> Option<std::path::PathBuf> {
        Some(std::path::PathBuf::from(&self.root))
    }

    fn machine(&self) -> Option<String> {
        self.machine.clone()
    }

    impl_any!();
}

impl DiffBuffer {
    #[allow(clippy::too_many_arguments)]
    fn draw_tree(&mut self, ui: &mut egui::Ui, painter: &egui::Painter, t: &Theme, tree: Rect, id: egui::Id, hits: &mut Vec<(String, Rect)>, acts: &mut Vec<Act>) {
        let k = t.ui_scale();
        painter.rect_filled(tree, 0.0, t.bg_alt);
        let (added, removed) = self.files.iter().fold((0, 0), |(a, r), f| (a + f.added, r + f.removed));
        let head_h = 30.0 * k;
        let small = FontId::proportional(t.font_size - 2.5);
        let cy = tree.top() + head_h / 2.0;
        let n = self.files.len();
        chrome::text(painter, egui::pos2(tree.left() + 12.0 * k, cy), &format!("{n} {}", if n == 1 { "file" } else { "files" }), small.clone(), t.fg_dim, 70.0 * k);
        let stat = format!("−{removed}");
        let w = chrome::text_width(painter, &stat, small.clone());
        chrome::text(painter, egui::pos2(tree.right() - 12.0 * k - w, cy), &stat, small.clone(), t.red, w + 1.0);
        let plus = format!("+{added}");
        let pw = chrome::text_width(painter, &plus, small.clone());
        chrome::text(painter, egui::pos2(tree.right() - 18.0 * k - w - pw, cy), &plus, small.clone(), t.green, pw + 1.0);

        let list = Rect::from_min_max(egui::pos2(tree.left(), tree.top() + head_h), tree.max);
        let rows = tree_rows(&self.files, &self.closed_dirs);
        let row_h = (t.font_size * 1.8).round();
        let total = rows.len() as f32 * row_h;
        if ui.rect_contains_pointer(list) {
            self.tree_scroll -= ui.input(|i| i.smooth_scroll_delta.y);
        }
        self.tree_scroll = self.tree_scroll.clamp(0.0, (total - list.height()).max(0.0));
        let clip = painter.with_clip_rect(list);
        let font = FontId::proportional(t.font_size - 1.5);
        let mut y = list.top() - self.tree_scroll;
        let mut toggle = None;
        for row in &rows {
            let r = Rect::from_min_size(egui::pos2(tree.left(), y), egui::vec2(tree.width(), row_h));
            y += row_h;
            if r.bottom() < list.top() || r.top() > list.bottom() {
                continue;
            }
            let shown = r.intersect(list);
            let key = if row.file.is_some() { format!("file-{}", row.path) } else { format!("dir-{}", row.path) };
            let resp = ui.interact(shown, id.with(&key), Sense::click());
            hits.push((key, shown));
            let x = r.left() + 12.0 * k + row.depth as f32 * 12.0 * k;
            let cy = r.center().y;
            match row.file {
                None => {
                    if resp.hovered() {
                        clip.rect_filled(r, 0.0, chrome::hover_bg(t));
                    }
                    let open = !self.closed_dirs.contains(&row.path);
                    chrome::chevron(&clip, egui::pos2(x + 3.0 * k, cy), open, t.fg_faint);
                    // A closed folder says what is in it.
                    let inside = if open { 0 } else { self.comments_under(&row.path) };
                    let mut end = r.right() - 10.0 * k;
                    if inside > 0 {
                        end = comment_count(&clip, t, end, cy, inside, &small);
                    }
                    chrome::text(&clip, egui::pos2(x + 14.0 * k, cy), &row.name, font.clone(), t.fg_dim, end - x - 18.0 * k);
                    if resp.clicked() {
                        toggle = Some(row.path.clone());
                    }
                }
                Some(i) => {
                    let f = &self.files[i];
                    let selected = self.current.as_deref() == Some(f.path.as_str());
                    if selected {
                        clip.rect_filled(r, 0.0, chrome::selected_bg(t));
                    } else if resp.hovered() {
                        clip.rect_filled(r, 0.0, chrome::hover_bg(t));
                    }
                    let viewed = self.review.viewed.contains_key(&f.path);
                    clip.text(egui::pos2(x + 3.0 * k, cy), egui::Align2::CENTER_CENTER, &f.status, small.clone(), if viewed { t.fg_faint } else { status_color(t, &f.status) });
                    // What fits on the right: a count of comments, then the size of the change.
                    let mut right = r.right() - 10.0 * k;
                    let comments = self.comments_on(&f.path);
                    if comments > 0 {
                        let from = comment_count(&clip, t, right, cy, comments, &small);
                        hits.push((format!("file-comments-{}", f.path), Rect::from_min_max(egui::pos2(from, r.top()), egui::pos2(right, r.bottom()))));
                        right = from - 5.0 * k;
                    }
                    let name_x = x + 14.0 * k;
                    let name_w = chrome::text_width(&clip, &row.name, font.clone());
                    if viewed {
                        chrome::check(&clip, egui::pos2(right - 5.0 * k, cy), small.size, t.green);
                        right -= 14.0 * k;
                    } else if !f.binary {
                        let stat = format!("+{} −{}", f.added, f.removed);
                        let w = chrome::text_width(&clip, &stat, small.clone());
                        if name_x + name_w + w + 10.0 * k < right {
                            let pw = chrome::text_width(&clip, &format!("+{} ", f.added), small.clone());
                            chrome::text(&clip, egui::pos2(right - w, cy), &format!("+{}", f.added), small.clone(), t.green, w);
                            chrome::text(&clip, egui::pos2(right - w + pw, cy), &format!("−{}", f.removed), small.clone(), t.red, w);
                            right -= w + 8.0 * k;
                        }
                    }
                    chrome::text(&clip, egui::pos2(name_x, cy), &row.name, font.clone(), if viewed { t.fg_faint } else if selected { t.fg } else { t.fg_dim }, right - name_x);
                    if resp.clicked() {
                        acts.push(Act("diff-select-file", vec![serde_json::json!(f.path)]));
                    }
                }
            }
        }
        if let Some(p) = toggle
            && !self.closed_dirs.remove(&p)
        {
            self.closed_dirs.insert(p);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_file(&mut self, ui: &mut egui::Ui, painter: &egui::Painter, t: &Theme, main: Rect, focused: bool, id: egui::Id, ed: &mut Editor, hits: &mut Vec<(String, Rect)>, acts: &mut Vec<Act>, small: &FontId, mono: &FontId) {
        let k = t.ui_scale();
        // The file's header.
        let head_h = 34.0 * k;
        let head = Rect::from_min_size(main.min, egui::vec2(main.width(), head_h));
        painter.rect_filled(head, 0.0, t.bg_elevated);
        painter.hline(main.x_range(), head.bottom(), Stroke::new(1.0, t.border));
        let entry = self.current.as_deref().and_then(|p| self.entry(p)).cloned().unwrap_or_default();
        let cy = head.center().y;
        let mut x = head.left() + 12.0 * k;
        let badge = chrome::pill(painter, x, cy, &entry.status, small.clone(), status_color(t, &entry.status), chrome::tint(status_color(t, &entry.status), 0.16), false);
        x = badge.right() + 8.0 * k;
        // The controls on the right, then whatever fits of the path.
        let ui_font = FontId::proportional(t.font_size - 2.0);
        let viewed = self.review.viewed.contains_key(&entry.path);
        let label = "Viewed";
        let tick = if viewed { 16.0 * k } else { 0.0 };
        let vw = chrome::text_width(painter, label, ui_font.clone()) + 18.0 * k + tick;
        let vr = Rect::from_min_size(egui::pos2(head.right() - 10.0 * k - vw, cy - 12.0 * k), egui::vec2(vw, 24.0 * k));
        let resp = ui.interact(vr, id.with("viewed"), Sense::click()).on_hover_text("Mark as viewed, and go to the next file (diff-mark-viewed, v). Changing clears it.");
        painter.rect_filled(vr, 6.0 * k, if viewed { chrome::tint(t.green, 0.18) } else if resp.hovered() { chrome::hover_bg(t) } else { t.chip() });
        if viewed {
            chrome::check(painter, egui::pos2(vr.left() + 13.0 * k, cy), ui_font.size, t.green);
        }
        painter.text(vr.center() + egui::vec2(tick / 2.0, 0.0), egui::Align2::CENTER_CENTER, label, ui_font.clone(), if viewed { t.green } else { t.fg_dim });
        if resp.clicked() {
            acts.push(Act("diff-mark-viewed", vec![]));
        }
        hits.push(("viewed".into(), vr));
        let (at, of) = self.change_position();
        let d = self.diff.clone();
        let mut right = vr.left() - 10.0 * k;
        if of > 0 {
            let label = format!("Change {} of {of}", at.max(1));
            let w = chrome::text_width(painter, &label, small.clone());
            if right - w > x + 120.0 * k {
                chrome::text(painter, egui::pos2(right - w, cy), &label, small.clone(), t.fg_faint, w + 1.0);
                right -= w + 10.0 * k;
            }
        }
        if let Some(d) = &d {
            let (a, r) = (format!("+{}", d.added), format!("−{}", d.removed));
            let (aw, rw) = (chrome::text_width(painter, &a, small.clone()), chrome::text_width(painter, &r, small.clone()));
            if right - aw - rw > x + 90.0 * k {
                chrome::text(painter, egui::pos2(right - rw, cy), &r, small.clone(), t.red, rw + 1.0);
                chrome::text(painter, egui::pos2(right - rw - aw - 6.0 * k, cy), &a, small.clone(), t.green, aw + 1.0);
                right -= aw + rw + 16.0 * k;
            }
        }
        let on_file = self.comments_on(&entry.path);
        if on_file > 0 && right - 40.0 * k > x + 60.0 * k {
            right = comment_count(painter, t, right, cy, on_file, small) - 10.0 * k;
        }
        let path = match &entry.old_path {
            Some(o) => format!("{o} → {}", entry.path),
            None => entry.path.clone(),
        };
        chrome::text(painter, egui::pos2(x, cy), &path, FontId::proportional(t.font_size - 1.0), t.fg, (right - x).max(0.0));

        let content = Rect::from_min_max(egui::pos2(main.left(), head.bottom() + 1.0), main.max);
        if let Some(n) = &self.note {
            let c = content.center();
            painter.text(c, egui::Align2::CENTER_CENTER, n, FontId::proportional(t.font_size - 1.5), t.fg_faint);
            return;
        }
        let Some(d) = d else {
            chrome::spinner(painter, content.center(), 9.0 * k, ui.input(|i| i.time), t.fg_faint);
            ui.ctx().request_repaint();
            return;
        };

        // Which view fits.
        let min_split = ed.var("diff-split-min-width").and_then(serde_json::Value::as_f64).unwrap_or(600.0) as f32 * k;
        let split_now = self.split && content.width() >= min_split;
        if split_now != self.split_now {
            let at = self.rows.get(self.cursor).copied();
            self.split_now = split_now;
            self.rebuild_rows();
            if let Some(r) = at {
                self.cursor = self.find_row(r.old, r.new);
            }
            self.reveal = true;
        }
        self.ensure_items();

        let strip_w = 12.0 * k;
        let page = Rect::from_min_max(content.min, egui::pos2(content.right() - strip_w, content.bottom()));
        let strip = Rect::from_min_max(egui::pos2(page.right(), content.top()), content.max);
        let char_w = chrome::text_width(painter, "0000000000", mono.clone()) / 10.0;
        let digits = d.old.text.len().max(d.new.text.len()).max(1).to_string().len().max(2);
        let m = Metrics { k, char_w, line_h: (t.mono_size * 1.5).round(), num_w: char_w * digits as f32 + 10.0 * k };
        // The panes: (rect, side), or one for the unified view.
        let pane_w = if self.split_now { (page.width() / 2.0).floor() } else { page.width() };
        let gutter = if self.split_now { m.num_w + 4.0 * k } else { m.num_w * 2.0 + 4.0 * k };
        let code_w = (pane_w - gutter - 10.0 * k).max(40.0);
        let cols = ((code_w / char_w).floor() as usize).max(8);

        // The composer's height.
        let mut composer_h = 0.0;
        if let Some(e) = self.editor.as_mut() {
            e.area.view.padding = egui::vec2(10.0 * k, 8.0 * k);
            let src = PlainLines { lines: &e.area.doc.lines, face: Face::Default };
            let w = comment_card_width(page.width(), k);
            let text_h = e.area.view.measure(ui, w, &src, t).clamp(60.0 * k, 240.0 * k);
            composer_h = text_h + 60.0 * k;
        }
        self.layout(painter, t, m, page.width(), cols, composer_h);
        let total = self.ys.last().copied().unwrap_or(0.0);

        // Input over the code: scrolling, then the mouse on rows.
        let resp = ui.interact(page, id.with("rows"), Sense::click_and_drag());
        if ui.rect_contains_pointer(page) {
            let (dx, dy) = ui.input(|i| (i.smooth_scroll_delta.x, i.smooth_scroll_delta.y));
            if dy != 0.0 {
                self.scroll -= dy;
                self.reveal = false;
            }
            if dx != 0.0 && !self.wrap {
                self.hscroll = (self.hscroll - dx).max(0.0);
            }
        }
        if self.wrap {
            self.hscroll = 0.0;
        }
        if self.reveal {
            self.reveal = false;
            let item = if self.editor.is_some() { self.items.iter().position(|i| *i == Item::Composer) } else { None }.or_else(|| self.row_item.get(self.cursor).map(|i| *i as usize));
            if let Some(i) = item {
                let (top, bottom) = (self.ys[i], self.ys[i + 1]);
                let margin = m.line_h * 3.0;
                if top - margin < self.scroll {
                    self.scroll = top - margin;
                } else if bottom + margin > self.scroll + page.height() {
                    self.scroll = bottom + margin - page.height();
                }
            }
        }
        self.scroll = self.scroll.clamp(0.0, (total - page.height()).max(0.0));

        // Which pane and row the pointer is on.
        let pointer = ui.input(|i| i.pointer.latest_pos()).filter(|p| page.contains(*p));
        let locate = |this: &Self, p: Pos2| -> Option<(usize, Which, bool)> {
            let item = this.item_at(p.y - page.top() + this.scroll)?;
            let row = match this.items[item] {
                Item::Row(r) => r,
                Item::Fold { from, .. } => from,
                _ => return None,
            };
            let (which, in_gutter) = if this.split_now {
                let second = p.x >= page.left() + pane_w;
                let x0 = if second { page.left() + pane_w } else { page.left() };
                (if second { Which::New } else { Which::Old }, p.x - x0 < gutter)
            } else {
                (if this.rows[row].new.is_some() { Which::New } else { Which::Old }, p.x - page.left() < gutter)
            };
            Some((row, which, in_gutter))
        };
        let hovered = pointer.and_then(|p| locate(self, p));

        // Comment cards and the composer take their own clicks, so the rows are drawn first.
        let clip = painter.with_clip_rect(page);
        let first = self.ys.partition_point(|top| *top <= self.scroll).saturating_sub(1);
        let mut item = first;
        let mut cards: Vec<(Item, Rect)> = Vec::new();
        let selection = self.anchor.map(|a| (a.min(self.cursor), a.max(self.cursor)));
        let comment_lines: HashSet<(bool, u32)> = self.review.comments.iter().filter(|c| Some(c.path.as_str()) == self.current.as_deref() && !c.outdated && c.state != State::Resolved).flat_map(|c| (c.start..=c.end).map(move |l| (c.side == Which::New, l))).collect();
        while item < self.items.len() {
            let top = page.top() + self.ys[item] - self.scroll;
            let h = self.ys[item + 1] - self.ys[item];
            if top > page.bottom() {
                break;
            }
            let row_rect = Rect::from_min_size(egui::pos2(page.left(), top), egui::vec2(page.width(), h));
            match self.items[item].clone() {
                Item::Row(r) => {
                    let row = self.rows[r];
                    let active = r == self.cursor || selection.is_some_and(|(a, b)| r >= a && r <= b);
                    if self.split_now {
                        let left = Rect::from_min_size(row_rect.min, egui::vec2(pane_w, h));
                        let right = Rect::from_min_max(egui::pos2(left.right(), top), row_rect.max);
                        self.paint_split_side(&clip, t, m, left, gutter, &d.old, row.old, Which::Old, active, focused, &hovered, r, &comment_lines, code_w, cols, mono, hits);
                        self.paint_split_side(&clip, t, m, right, gutter, &d.new, row.new, Which::New, active, focused, &hovered, r, &comment_lines, code_w, cols, mono, hits);
                    } else {
                        self.paint_unified(&clip, t, m, row_rect, gutter, &d, row, active, focused, &hovered, r, &comment_lines, code_w, cols, mono, hits);
                    }
                }
                Item::Fold { from, to } => {
                    let selected = self.cursor >= from && self.cursor < to;
                    let hover = hovered.is_some_and(|(r, _, _)| r >= from && r < to);
                    clip.rect_filled(row_rect, 0.0, if selected && focused { chrome::selected_bg(t) } else if hover { chrome::hover_bg(t) } else { t.bg_alt });
                    let n = to - from;
                    let scope = self.scope_above(&d, from);
                    let label = format!("{n} unchanged lines{}", scope.map_or(String::new(), |s| format!("  ·  {s}")));
                    chrome::dots(&clip, egui::pos2(row_rect.left() + 20.0 * k, row_rect.center().y), small.size, t.fg_faint);
                    chrome::text(&clip, egui::pos2(row_rect.left() + 34.0 * k, row_rect.center().y), &label, small.clone(), t.fg_faint, row_rect.width() - 44.0 * k);
                    hits.push((format!("fold-{from}"), row_rect));
                }
                it @ (Item::Comment(_) | Item::Composer) => cards.push((it, row_rect)),
            }
            item += 1;
        }
        if self.split_now {
            clip.vline(page.left() + pane_w, page.y_range(), Stroke::new(1.0, t.border));
        }

        // The mouse on rows: a click puts the cursor there or opens a fold; a drag selects; in
        // the line numbers, clicking or dragging starts a comment on those lines.
        let at_press = resp.interact_pointer_pos().and_then(|p| locate(self, p));
        // egui reports a drag once the pointer has moved: it began where the button went down.
        if resp.drag_started()
            && let Some((row, which, in_gutter)) = ui.input(|i| i.pointer.press_origin()).and_then(|p| locate(self, p))
        {
            self.drag = Some((row, in_gutter));
            self.anchor = Some(row);
            self.cursor = at_press.map_or(row, |(r, _, _)| r);
            self.side = which;
        }
        if resp.dragged()
            && self.drag.is_some()
            && let Some((row, _, _)) = at_press
        {
            self.cursor = row;
        }
        if resp.drag_stopped()
            && let Some((_, true)) = self.drag.take()
        {
            acts.push(Act("diff-comment", vec![]));
        }
        if resp.clicked()
            && let Some(p) = resp.interact_pointer_pos()
            && let Some((row, which, in_gutter)) = locate(self, p)
        {
            let on_fold = matches!(self.row_item.get(row).and_then(|i| self.items.get(*i as usize)), Some(Item::Fold { .. }));
            self.cursor = row;
            self.anchor = None;
            self.side = which;
            if on_fold {
                self.expand_fold(row);
            } else if in_gutter && self.line_at(row, which).is_some() {
                acts.push(Act("diff-comment", vec![]));
            }
        }
        let wants_pointer = matches!(hovered, Some((_, _, true)));
        if wants_pointer {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }

        for (it, r) in cards {
            let w = comment_card_width(page.width(), k);
            // Under the side it is about, when there is room for it there.
            let on_new = match &it {
                Item::Comment(cid) => self.review.comments.iter().any(|c| c.id == *cid && c.side == Which::New),
                _ => self.editor.as_ref().is_some_and(|e| e.side == Which::New),
            };
            let x0 = if self.split_now && on_new { (page.left() + pane_w + 14.0 * k).min(page.right() - w - 14.0 * k).max(r.left() + 14.0 * k) } else { r.left() + 14.0 * k };
            let card = Rect::from_min_size(egui::pos2(x0, r.top() + 4.0 * k), egui::vec2(w, r.height() - 8.0 * k));
            match it {
                Item::Comment(cid) => self.draw_comment(ui, &clip, t, card, cid, id, hits, acts),
                Item::Composer => self.draw_composer(ui, &clip, t, card, focused, id, ed, hits, acts),
                _ => {}
            }
        }

        // The strip: where the changes and comments are, and what is in view. A click goes there.
        painter.rect_filled(strip, 0.0, t.bg_alt);
        if total > 0.0 {
            let (top, h) = (strip.top(), strip.height());
            let y_of = |content_y: f32| top + content_y / total * h;
            for hunk in &self.hunks {
                let Some(&i) = self.row_item.get(hunk.rows.start) else { continue };
                let Some(&j) = self.row_item.get(hunk.rows.end - 1) else { continue };
                let (a, b) = (y_of(self.ys[i as usize]), y_of(self.ys[j as usize + 1]).max(y_of(self.ys[i as usize]) + 2.0));
                let (has_add, has_del, moved) = self.rows[hunk.rows.clone()].iter().fold((false, false, false), |(ad, de, mv), r| {
                    let n = r.new.map(|l| d.new.lines[l as usize].kind);
                    let o = r.old.map(|l| d.old.lines[l as usize].kind);
                    (ad || n == Some(Kind::Changed), de || o == Some(Kind::Changed), mv || n == Some(Kind::Moved) || o == Some(Kind::Moved))
                });
                let color = if hunk.format_only {
                    t.fg_faint
                } else if moved && !has_add && !has_del {
                    t.purple
                } else if has_add && has_del {
                    t.yellow
                } else if has_add {
                    t.green
                } else {
                    t.red
                };
                painter.rect_filled(Rect::from_min_max(egui::pos2(strip.left() + 2.0 * k, a), egui::pos2(strip.right() - 2.0 * k, b)), 1.0, color);
            }
            for (i, it) in self.items.iter().enumerate() {
                if let Item::Comment(_) = it {
                    painter.circle_filled(egui::pos2(strip.center().x, y_of(self.ys[i])), 2.5 * k, t.accent);
                }
            }
            if total > page.height() {
                let view = Rect::from_min_max(egui::pos2(strip.left(), y_of(self.scroll)), egui::pos2(strip.right(), y_of(self.scroll + page.height())));
                painter.rect_filled(view, 2.0, chrome::tint(t.fg, 0.10));
                let sresp = ui.interact(strip, id.with("strip"), Sense::click_and_drag());
                if (sresp.clicked() || sresp.dragged())
                    && let Some(p) = sresp.interact_pointer_pos()
                {
                    self.scroll = ((p.y - top) / h * total - page.height() / 2.0).clamp(0.0, (total - page.height()).max(0.0));
                }
            }
        }
    }

    /// The nearest line above `row` with less indentation than anything below it: the function
    /// it is in, more or less.
    fn scope_above(&self, d: &FileDiff, row: usize) -> Option<String> {
        let line = self.rows.get(row)?.new?;
        let indent = |s: &str| s.len() - s.trim_start().len();
        let mut limit = d.new.text.get(line as usize).map_or(usize::MAX, |l| indent(l));
        if limit == 0 {
            limit = 1;
        }
        for l in (0..line as usize).rev().take(400) {
            let s = &d.new.text[l];
            let t = s.trim();
            if !t.is_empty() && indent(s) < limit && t.chars().next().is_some_and(char::is_alphabetic) {
                return Some(t.chars().take(70).collect());
            }
        }
        None
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_split_side(&self, p: &egui::Painter, t: &Theme, m: Metrics, pane: Rect, gutter: f32, side: &engine::Side, line: Option<u32>, which: Which, active: bool, focused: bool, hovered: &Option<(usize, Which, bool)>, row: usize, comment_lines: &HashSet<(bool, u32)>, code_w: f32, cols: usize, font: &FontId, hits: &mut Vec<(String, Rect)>) {
        let k = m.k;
        let Some(l) = line else {
            hatch(p, pane, chrome::tint(t.fg_faint, 0.25), k);
            if active && focused && self.side == which {
                p.rect_filled(pane, 0.0, chrome::selected_bg(t));
            }
            return;
        };
        let info = &side.lines[l as usize];
        self.paint_line_bg(p, t, pane, info.kind, which);
        if active && (self.side == which) {
            p.rect_filled(pane, 0.0, if focused { chrome::selected_bg(t) } else { chrome::hover_bg(t) });
        }
        self.paint_bar(p, t, pane, info.kind, which, k);
        let num = (l + 1).to_string();
        let num_right = pane.left() + m.num_w;
        let this_hover = hovered.is_some_and(|(r, w, g)| r == row && w == which && g);
        let gutter_rect = Rect::from_min_size(pane.min, egui::vec2(gutter, m.line_h));
        hits.push((format!("gutter-{}-{}", which.name(), l + 1), gutter_rect));
        self.paint_number(p, t, m, num_right, pane.top(), &num, comment_lines.contains(&(which == Which::New, l + 1)), this_hover, active);
        let code = Rect::from_min_max(egui::pos2(pane.left() + gutter + 6.0 * k, pane.top()), egui::pos2(pane.left() + gutter + 6.0 * k + code_w, pane.bottom()));
        self.paint_code(p, t, m, code, side, l as usize, which, cols, font);
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_unified(&self, p: &egui::Painter, t: &Theme, m: Metrics, r: Rect, gutter: f32, d: &FileDiff, row: Row, active: bool, focused: bool, hovered: &Option<(usize, Which, bool)>, ri: usize, comment_lines: &HashSet<(bool, u32)>, code_w: f32, cols: usize, font: &FontId, hits: &mut Vec<(String, Rect)>) {
        let k = m.k;
        let (which, l) = match (row.old, row.new) {
            (_, Some(n)) => (Which::New, n),
            (Some(o), None) => (Which::Old, o),
            _ => return,
        };
        let side = if which == Which::New { &d.new } else { &d.old };
        let info = &side.lines[l as usize];
        self.paint_line_bg(p, t, r, info.kind, which);
        if active {
            p.rect_filled(r, 0.0, if focused { chrome::selected_bg(t) } else { chrome::hover_bg(t) });
        }
        self.paint_bar(p, t, r, info.kind, which, k);
        let this_hover = hovered.is_some_and(|(h, w, g)| h == ri && w == which && g);
        if let Some(o) = row.old {
            let flagged = comment_lines.contains(&(false, o + 1));
            self.paint_number(p, t, m, r.left() + m.num_w, r.top(), &(o + 1).to_string(), flagged, this_hover && which == Which::Old, active);
            hits.push((format!("gutter-old-{}", o + 1), Rect::from_min_size(r.min, egui::vec2(gutter, m.line_h))));
        }
        if let Some(n) = row.new {
            let flagged = comment_lines.contains(&(true, n + 1));
            self.paint_number(p, t, m, r.left() + m.num_w * 2.0, r.top(), &(n + 1).to_string(), flagged, this_hover, active);
            hits.push((format!("gutter-new-{}", n + 1), Rect::from_min_size(r.min, egui::vec2(gutter, m.line_h))));
        }
        let code = Rect::from_min_max(egui::pos2(r.left() + gutter + 6.0 * k, r.top()), egui::pos2(r.left() + gutter + 6.0 * k + code_w, r.bottom()));
        self.paint_code(p, t, m, code, side, l as usize, which, cols, font);
    }

    fn paint_line_bg(&self, p: &egui::Painter, t: &Theme, r: Rect, kind: Kind, which: Which) {
        let bg = match kind {
            Kind::Changed if which == Which::New => t.diff_add_bg,
            Kind::Changed => t.diff_del_bg,
            Kind::Moved => chrome::tint(t.purple, 0.10),
            _ => return,
        };
        p.rect_filled(r, 0.0, bg);
    }

    fn paint_bar(&self, p: &egui::Painter, t: &Theme, r: Rect, kind: Kind, which: Which, k: f32) {
        let color = match kind {
            Kind::Same => return,
            Kind::Format => t.fg_faint,
            Kind::Moved => t.purple,
            Kind::Changed if which == Which::New => t.diff_add_fg,
            Kind::Changed => t.diff_del_fg,
        };
        p.rect_filled(Rect::from_min_size(r.min, egui::vec2(2.5 * k, r.height())), 0.0, color);
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_number(&self, p: &egui::Painter, t: &Theme, m: Metrics, right: f32, top: f32, num: &str, commented: bool, hover: bool, active: bool) {
        let color = if commented { t.accent } else if active { t.fg_dim } else { t.fg_faint };
        let font = FontId::monospace(t.mono_size - 1.5);
        if hover {
            // Where a click starts a comment.
            let c = egui::pos2(right - m.char_w * 0.6, top + m.line_h / 2.0);
            let r = Rect::from_center_size(c, egui::Vec2::splat(m.line_h - 3.0));
            p.rect_filled(r, 4.0 * m.k, t.accent);
            chrome::plus(p, r.shrink(4.0 * m.k), chrome::on(t.accent, t));
            return;
        }
        p.text(egui::pos2(right - 2.0 * m.k, top + m.line_h / 2.0), egui::Align2::RIGHT_CENTER, num, font, color);
        if commented {
            p.circle_filled(egui::pos2(right - m.num_w + 5.0 * m.k, top + m.line_h / 2.0), 2.5 * m.k, t.accent);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_code(&self, p: &egui::Painter, t: &Theme, m: Metrics, code: Rect, side: &engine::Side, line: usize, which: Which, cols: usize, font: &FontId) {
        let text = &side.text[line];
        if text.is_empty() {
            return;
        }
        let info = &side.lines[line];
        let spans = side.spans.get(line);
        let base = if info.kind == Kind::Format { t.fg_dim } else { t.fg };
        let strong = if which == Which::New { t.diff_add_fg } else { t.diff_del_fg };
        let mark_bg = if info.kind == Kind::Moved { chrome::tint(t.purple, 0.25) } else { chrome::tint(strong, 0.32) };
        let clip = p.with_clip_rect(code.intersect(p.clip_rect()));
        let mut starts = vec![0];
        if self.wrap {
            starts.extend(wrap_points(text, cols));
        }
        for (n, &s) in starts.iter().enumerate() {
            let e = starts.get(n + 1).copied().unwrap_or(text.len());
            let y = code.top() + n as f32 * m.line_h;
            if y > p.clip_rect().bottom() {
                break;
            }
            if y + m.line_h < p.clip_rect().top() {
                continue;
            }
            let job = code_job(text, s..e, spans, &info.marks, base, mark_bg, font, t);
            let g = clip.layout_job(job);
            let gy = y + (m.line_h - g.size().y) / 2.0;
            let hang = if n > 0 { hang_of(text, cols) as f32 * m.char_w } else { 0.0 };
            clip.galley(egui::pos2(code.left() - self.hscroll + hang, gy), g, base);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_comment(&mut self, ui: &mut egui::Ui, p: &egui::Painter, t: &Theme, card: Rect, cid: u64, id: egui::Id, hits: &mut Vec<(String, Rect)>, acts: &mut Vec<Act>) {
        let k = t.ui_scale();
        let Some(c) = self.review.comments.iter().find(|c| c.id == cid).cloned() else { return };
        let accent = match (c.state, c.ask) {
            (State::Resolved, _) => t.fg_faint,
            (_, Ask::Fix) => t.accent,
            (_, Ask::Question) => t.blue,
        };
        p.rect_filled(card, 8.0 * k, t.bg_elevated);
        p.rect_stroke(card, 8.0 * k, Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
        p.rect_filled(Rect::from_min_size(card.min + egui::vec2(0.0, 6.0 * k), egui::vec2(2.5 * k, card.height() - 12.0 * k)), 1.0, accent);
        let small = FontId::proportional(t.font_size - 2.5);
        let head_y = card.top() + 15.0 * k;
        let x = card.left() + 12.0 * k;
        let badge = match c.ask {
            Ask::Fix => "Fix",
            Ask::Question => "Question",
        };
        let b = chrome::pill(p, x, head_y, badge, small.clone(), accent, chrome::tint(accent, 0.16), false);
        // The buttons, right to left; narrow, only those that fit.
        let mut right = card.right() - 8.0 * k;
        let mut row: Vec<(&str, Rect)> = vec![("badge", b)];
        let state_note = match (c.state, c.outdated) {
            (State::Sent, _) => "sent",
            (State::Resolved, _) => "resolved",
            (_, true) => "outdated",
            _ => "",
        };
        let mut button = |p: &egui::Painter, ui: &mut egui::Ui, key: String, label: &str, right: &mut f32, min_x: f32| -> Option<(bool, Rect)> {
            let w = chrome::text_width(p, label, small.clone()) + 12.0 * k;
            if *right - w < min_x {
                return None;
            }
            let r = Rect::from_min_size(egui::pos2(*right - w, head_y - 9.0 * k), egui::vec2(w, 18.0 * k));
            let resp = ui.interact(r, id.with(&key), Sense::click());
            if resp.hovered() {
                p.rect_filled(r, 4.0 * k, chrome::hover_bg(t));
            }
            p.text(r.center(), egui::Align2::CENTER_CENTER, label, small.clone(), if resp.hovered() { t.fg } else { t.fg_dim });
            hits.push((key, r));
            *right = r.left() - 2.0 * k;
            Some((resp.clicked(), r))
        };
        let min_x = b.right() + 8.0 * k;
        if let Some((clicked, r)) = button(p, ui, format!("comment-delete-{cid}"), "Delete", &mut right, min_x) {
            row.push(("delete", r));
            if clicked {
                acts.push(Act("diff-comment-delete", vec![serde_json::json!(cid)]));
            }
        }
        let resolve = if c.state == State::Resolved { "Reopen" } else { "Resolve" };
        if let Some((clicked, r)) = button(p, ui, format!("comment-resolve-{cid}"), resolve, &mut right, min_x) {
            row.push(("resolve", r));
            if clicked {
                acts.push(Act("diff-comment-resolve", vec![serde_json::json!(cid)]));
            }
        }
        if let Some((clicked, r)) = button(p, ui, format!("comment-edit-{cid}"), "Edit", &mut right, min_x) {
            row.push(("edit", r));
            if clicked {
                acts.push(Act("diff-comment-edit", vec![serde_json::json!(cid)]));
            }
        }
        let place = if state_note.is_empty() { c.place() } else { format!("{} · {state_note}", c.place()) };
        chrome::text(p, egui::pos2(b.right() + 8.0 * k, head_y), &place, small.clone(), t.fg_faint, (right - b.right() - 12.0 * k).max(0.0));
        let color = if c.state == State::Resolved { t.fg_dim } else { t.fg };
        let g = p.layout(c.text.clone(), comment_font(t), color, card.width() - 24.0 * k);
        p.galley(egui::pos2(x, card.top() + 30.0 * k), g, color);
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_composer(&mut self, ui: &mut egui::Ui, p: &egui::Painter, t: &Theme, card: Rect, focused: bool, id: egui::Id, ed: &mut Editor, hits: &mut Vec<(String, Rect)>, acts: &mut Vec<Act>) {
        let k = t.ui_scale();
        let Some(bid) = self.id else { return };
        p.rect_filled(card, 8.0 * k, t.bg_elevated);
        p.rect_stroke(card, 8.0 * k, Stroke::new(1.0, if focused { t.accent } else { t.border }), egui::StrokeKind::Inside);
        let toolbar_h = 38.0 * k;
        let text_rect = Rect::from_min_max(card.min, egui::pos2(card.right(), card.bottom() - toolbar_h));
        let shape = if focused { ed.cursor_shape(bid) } else { None }.unwrap_or(CursorShape::Bar);
        let selection = if focused { crate::packages::vim::selection_kind(ed, bid) } else { Default::default() };
        let Some(e) = self.editor.as_mut() else { return };
        let src = PlainLines { lines: &e.area.doc.lines, face: Face::Default };
        e.area.view.show(ui, text_rect, &src, t, &ViewOptions { focused, cursor: shape, show_cursor: focused, selection });
        let empty = e.area.doc.is_empty();
        let (ask, place) = (e.ask, if e.end > e.start { format!("Lines {}–{}", e.start, e.end) } else { format!("Line {}", e.start) });
        let font = FontId::proportional(t.font_size - 2.0);
        if empty {
            let hint = if ask == Ask::Fix { "What should change?" } else { "What do you want to know?" };
            p.text(text_rect.min + e.area.view.padding + egui::vec2(2.0, 0.0), egui::Align2::LEFT_TOP, hint, FontId::proportional(t.font_size), t.fg_faint);
        }
        // The toolbar: fix or question on the left, saving on the right. Keeps the essentials
        // when narrow.
        let cy = card.bottom() - toolbar_h / 2.0;
        let h = 24.0 * k;
        let mut x = card.left() + 8.0 * k;
        let mut placed: Vec<(&str, Rect)> = Vec::new();
        let width = |s: &str| chrome::text_width(p, s, font.clone()) + 18.0 * k;
        for (label, which, key) in [("Fix", Ask::Fix, "ask-fix"), ("Question", Ask::Question, "ask-question")] {
            let r = Rect::from_min_size(egui::pos2(x, cy - h / 2.0), egui::vec2(width(label), h));
            let resp = ui.interact(r, id.with(key), Sense::click()).on_hover_text("Ask for a change, or for an answer (diff-comment-cycle-kind, M-k)");
            let on = ask == which;
            p.rect_filled(r, 6.0 * k, if on { chrome::tint(t.accent, 0.2) } else if resp.hovered() { chrome::hover_bg(t) } else { t.chip() });
            p.text(r.center(), egui::Align2::CENTER_CENTER, label, font.clone(), if on { t.accent } else { t.fg_dim });
            if resp.clicked() {
                acts.push(Act("diff-comment-cycle-kind", vec![serde_json::json!(which.name())]));
            }
            hits.push((key.to_string(), r));
            placed.push((key, r));
            x = r.right() + 4.0 * k;
        }
        let mut right = card.right() - 8.0 * k;
        let keys = crate::commands::all_bindings(ed);
        let key = keys.get("diff-comment-save").cloned();
        let key_w = key.as_ref().map_or(0.0, |key| chrome::keys_width(p, key, font.clone()) + 8.0 * k);
        let save_w = width("Comment") + key_w;
        let r = Rect::from_min_size(egui::pos2(right - save_w, cy - h / 2.0), egui::vec2(save_w, h));
        let resp = ui.interact(r, id.with("comment-save"), Sense::click()).on_hover_text("Keep it, to send with the rest of the review");
        p.rect_filled(r, 6.0 * k, if resp.hovered() { chrome::tint(t.accent, 0.85) } else { t.accent });
        let ink = chrome::on(t.accent, t);
        let word = chrome::text(p, egui::pos2(r.left() + 9.0 * k, cy), "Comment", font.clone(), ink, r.width());
        if let Some(key) = &key {
            chrome::paint_keys(p, r.left() + 9.0 * k + word + 8.0 * k, cy, key, font.clone(), chrome::tint(ink, 0.75));
        }
        if resp.clicked() {
            acts.push(Act("diff-comment-save", vec![]));
        }
        hits.push(("comment-save".into(), r));
        placed.push(("save", r));
        right = r.left() - 4.0 * k;
        let send = "Send now";
        let sw = width(send);
        if right - sw > x + 4.0 * k {
            let r = Rect::from_min_size(egui::pos2(right - sw, cy - h / 2.0), egui::vec2(sw, h));
            let resp = ui.interact(r, id.with("comment-send"), Sense::click()).on_hover_text("Send this comment to the agent now, on its own (diff-comment-send)");
            p.rect_filled(r, 6.0 * k, if resp.hovered() { chrome::hover_bg(t) } else { t.chip() });
            p.text(r.center(), egui::Align2::CENTER_CENTER, send, font.clone(), t.fg_dim);
            if resp.clicked() {
                acts.push(Act("diff-comment-send", vec![]));
            }
            hits.push(("comment-send".into(), r));
            placed.push(("send", r));
            right = r.left() - 4.0 * k;
        }
        let cancel = "Cancel";
        let cw = width(cancel);
        if right - cw > x + 4.0 * k {
            let r = Rect::from_min_size(egui::pos2(right - cw, cy - h / 2.0), egui::vec2(cw, h));
            let resp = ui.interact(r, id.with("comment-cancel"), Sense::click()).on_hover_text("Drop it (diff-comment-cancel)");
            if resp.hovered() {
                p.rect_filled(r, 6.0 * k, chrome::hover_bg(t));
            }
            p.text(r.center(), egui::Align2::CENTER_CENTER, cancel, font.clone(), t.fg_dim);
            if resp.clicked() {
                acts.push(Act("diff-comment-cancel", vec![]));
            }
            hits.push(("comment-cancel".into(), r));
            placed.push(("cancel", r));
            right = r.left() - 4.0 * k;
        }
        if right - x > 40.0 * k {
            chrome::text(p, egui::pos2(x + 6.0 * k, cy), &place, FontId::proportional(t.font_size - 2.5), t.fg_faint, right - x - 8.0 * k);
        }
        ed.check_row("diff-composer", card, &placed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_read_as_comparisons() {
        assert_eq!(parse_source("uncommitted"), Some(uncommitted()));
        assert_eq!(parse_source(" staged "), Some(staged()));
        assert_eq!(parse_source("main"), Some(against("main")));
        let s = parse_source("a..b").unwrap();
        assert_eq!((s.base, s.target, s.merge_base), (DiffSide::Rev("a".into()), DiffSide::Rev("b".into()), false));
        let s = parse_source("a...b").unwrap();
        assert!(s.merge_base);
        let s = parse_source("commit:abc").unwrap();
        assert_eq!(describe(&s), "Commit abc");
        assert_eq!(parse_source(""), None);
        assert_eq!(describe(&uncommitted()), "Uncommitted changes");
        assert_eq!(describe(&against("main")), "main → worktree");
    }

    #[test]
    fn long_lines_wrap_where_they_can_break() {
        assert!(wrap_points("short", 80).is_empty());
        // At a space, when one is near.
        assert_eq!(wrap_points("alpha beta gamma delta", 12), vec![11, 17]);
        // Anywhere, when none is.
        // With room for the continuation's indent (two columns here).
        assert_eq!(wrap_points(&"x".repeat(25), 10), vec![10, 18]);
        // Between characters, whatever their width.
        let cjk = "漢字漢字漢字漢字";
        for at in wrap_points(cjk, 8) {
            assert!(cjk.is_char_boundary(at));
        }
        assert!(!wrap_points(cjk, 8).is_empty());
        // Nothing overflows, however deep the indentation.
        let deep = format!("{}{}", " ".repeat(40), "word ".repeat(10));
        let mut starts = vec![0];
        starts.extend(wrap_points(&deep, 20));
        for (n, s) in starts.iter().enumerate() {
            let e = starts.get(n + 1).copied().unwrap_or(deep.len());
            let hang = if n > 0 { hang_of(&deep, 20) } else { 0 };
            assert!(deep[*s..e].trim_end().chars().count() + hang <= 20, "{:?}", &deep[*s..e]);
        }
    }

    fn entry(path: &str) -> DiffEntry {
        DiffEntry { path: path.into(), status: "M".into(), ..Default::default() }
    }

    #[test]
    fn the_tree_joins_lone_folders_and_closes() {
        let files = vec![entry("README.md"), entry("crates/serpis/src/a.rs"), entry("crates/serpis/src/b.rs"), entry("crates/serpis/src/diff/c.rs"), entry("docs/x.md")];
        let rows = tree_rows(&files, &HashSet::new());
        let names: Vec<(usize, &str)> = rows.iter().map(|r| (r.depth, r.name.as_str())).collect();
        assert_eq!(names, [(0, "crates/serpis/src"), (1, "diff"), (2, "c.rs"), (1, "a.rs"), (1, "b.rs"), (0, "docs"), (1, "x.md"), (0, "README.md")]);
        let closed: HashSet<String> = ["crates/serpis/src".to_string()].into();
        let rows = tree_rows(&files, &closed);
        assert_eq!(rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["crates/serpis/src", "docs", "x.md", "README.md"]);
        assert_eq!(rows[2].path, "docs/x.md");
    }
}
