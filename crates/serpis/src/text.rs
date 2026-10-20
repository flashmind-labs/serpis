//! The shared text primitives.
//!
//! - [`TextSurface`]: the uniform view of any buffer's text. Motions, vim, search, yank and
//!   jump packages (avy/flash) are written against it, so they work the same in text
//!   buffers, chat transcripts and terminals.
//! - [`Doc`]: editable lines with grouped undo/redo.
//! - [`DocView`]: renders styled lines with cursor, selection, wrapping and scrolling, and maps
//!   between text positions and screen rectangles.

use std::ops::Range;
use std::sync::Arc;

use egui::text::{CCursor, LayoutJob, TextFormat};
use egui::{Color32, Galley, Pos2, Rect, Sense, Stroke, Vec2};

use crate::theme::{Face, Theme};

/// A position in text: line index and column in characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

impl Pos {
    pub const ZERO: Pos = Pos { line: 0, col: 0 };
    pub fn new(line: usize, col: usize) -> Self {
        Pos { line, col }
    }
}

/// Order two positions.
pub fn ordered(a: Pos, b: Pos) -> (Pos, Pos) {
    if a <= b { (a, b) } else { (b, a) }
}

pub fn char_len(s: &str) -> usize {
    s.chars().count()
}

/// Byte index of the `col`-th char (clamped to the end).
pub fn byte_idx(s: &str, col: usize) -> usize {
    s.char_indices().nth(col).map(|(i, _)| i).unwrap_or(s.len())
}

/// How the cursor is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorShape {
    #[default]
    Bar,
    Block,
    Underline,
    Hollow,
}

/// The uniform text interface every text-like buffer exposes.
pub trait TextSurface {
    fn line_count(&self) -> usize;
    fn line(&self, i: usize) -> String;
    fn cursor(&self) -> Pos;
    fn set_cursor(&mut self, p: Pos);
    /// Selection anchor (mark). The selection is `anchor..cursor` when set.
    fn anchor(&self) -> Option<Pos>;
    fn set_anchor(&mut self, a: Option<Pos>);
    /// Whether text at `p` may be edited.
    fn editable_at(&self, _p: Pos) -> bool {
        false
    }
    /// Insert text, returning the position after it. Read-only surfaces ignore it.
    fn insert(&mut self, at: Pos, _text: &str) -> Pos {
        at
    }
    /// Delete `from..to`, returning the removed text.
    fn delete(&mut self, _from: Pos, _to: Pos) -> String {
        String::new()
    }
    fn undo(&mut self) -> bool {
        false
    }
    fn redo(&mut self) -> bool {
        false
    }
    /// Start a new undo group (called before each command).
    fn undo_boundary(&mut self) {}
    /// Column where the user's text starts on `line`: past a terminal prompt, else 0.
    fn text_start(&self, _line: usize) -> usize {
        0
    }
    /// Lines currently on screen.
    fn visible_lines(&self) -> Range<usize>;
    /// Screen rectangle of the character at `p`, if visible. Used by overlays such as jump labels.
    fn screen_rect(&self, p: Pos) -> Option<Rect>;
    /// Ask the view to scroll the cursor into view on the next frame.
    fn reveal_cursor(&mut self);
    /// Lines per page, for paging motions.
    fn page_lines(&self) -> usize {
        let r = self.visible_lines();
        (r.end - r.start).max(1)
    }

    // Provided helpers.

    fn line_len(&self, i: usize) -> usize {
        char_len(&self.line(i))
    }
    fn clamp(&self, p: Pos) -> Pos {
        let n = self.line_count();
        if n == 0 {
            return Pos::ZERO;
        }
        let line = p.line.min(n - 1);
        Pos::new(line, p.col.min(self.line_len(line)))
    }
    fn selection(&self) -> Option<(Pos, Pos)> {
        self.anchor().map(|a| ordered(a, self.cursor()))
    }
    fn text_range(&self, from: Pos, to: Pos) -> String {
        let (a, b) = ordered(from, to);
        let mut out = String::new();
        for l in a.line..=b.line.min(self.line_count().saturating_sub(1)) {
            let s = self.line(l);
            let start = if l == a.line { byte_idx(&s, a.col) } else { 0 };
            let end = if l == b.line { byte_idx(&s, b.col) } else { s.len() };
            out.push_str(&s[start.min(end)..end]);
            if l != b.line {
                out.push('\n');
            }
        }
        out
    }
    fn char_at(&self, p: Pos) -> Option<char> {
        let s = self.line(p.line);
        s.chars().nth(p.col).or(if p.line + 1 < self.line_count() && p.col == char_len(&s) { Some('\n') } else { None })
    }
    fn all_text(&self) -> String {
        (0..self.line_count()).map(|i| self.line(i)).collect::<Vec<_>>().join("\n")
    }
}

// ---------------------------------------------------------------------------
// Doc: editable lines with undo
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Op {
    Insert { at: Pos, text: String },
    Delete { at: Pos, text: String },
}

#[derive(Debug, Clone, Default)]
struct Group {
    ops: Vec<Op>,
    cursor_before: Pos,
}

#[derive(Debug, Clone)]
pub struct Doc {
    pub lines: Vec<String>,
    undo: Vec<Group>,
    redo: Vec<Group>,
    open_group: bool,
    pub version: u64,
    pub modified: bool,
}

impl Default for Doc {
    fn default() -> Self {
        Doc {
            lines: vec![String::new()],
            undo: Vec::new(),
            redo: Vec::new(),
            open_group: false,
            version: 0,
            modified: false,
        }
    }
}

/// Position after inserting `text` at `at`.
fn end_of_insert(at: Pos, text: &str) -> Pos {
    let mut parts = text.split('\n');
    let first = parts.next().unwrap_or("");
    let rest: Vec<&str> = parts.collect();
    match rest.last() {
        None => Pos::new(at.line, at.col + char_len(first)),
        Some(last) => Pos::new(at.line + rest.len(), char_len(last)),
    }
}

impl Doc {
    pub fn from_text(text: &str) -> Self {
        Doc {
            lines: text.split('\n').map(str::to_string).collect(),
            ..Default::default()
        }
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn set_text(&mut self, text: &str) {
        self.lines = text.split('\n').map(str::to_string).collect();
        self.undo.clear();
        self.redo.clear();
        self.version += 1;
    }

    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    pub fn clamp(&self, p: Pos) -> Pos {
        let line = p.line.min(self.lines.len() - 1);
        Pos::new(line, p.col.min(char_len(&self.lines[line])))
    }

    pub fn boundary(&mut self) {
        self.open_group = false;
    }

    fn record(&mut self, op: Op, cursor: Pos) {
        self.redo.clear();
        if !self.open_group || self.undo.is_empty() {
            self.undo.push(Group {
                ops: Vec::new(),
                cursor_before: cursor,
            });
            self.open_group = true;
            if self.undo.len() > 1000 {
                self.undo.remove(0);
            }
        }
        self.undo.last_mut().unwrap().ops.push(op);
    }

    fn raw_insert(&mut self, at: Pos, text: &str) -> Pos {
        let at = self.clamp(at);
        let line = &self.lines[at.line];
        let bi = byte_idx(line, at.col);
        let tail = line[bi..].to_string();
        let head = line[..bi].to_string();
        let mut new_lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        let end = end_of_insert(at, text);
        new_lines[0] = head + &new_lines[0];
        new_lines.last_mut().unwrap().push_str(&tail);
        self.lines.splice(at.line..=at.line, new_lines);
        self.version += 1;
        self.modified = true;
        end
    }

    fn raw_delete(&mut self, from: Pos, to: Pos) -> String {
        let (a, b) = ordered(self.clamp(from), self.clamp(to));
        if a == b {
            return String::new();
        }
        let first = &self.lines[a.line];
        let last = &self.lines[b.line];
        let head = first[..byte_idx(first, a.col)].to_string();
        let tail = last[byte_idx(last, b.col)..].to_string();
        let mut removed = String::new();
        for l in a.line..=b.line {
            let s = &self.lines[l];
            let st = if l == a.line { byte_idx(s, a.col) } else { 0 };
            let en = if l == b.line { byte_idx(s, b.col) } else { s.len() };
            removed.push_str(&s[st..en]);
            if l != b.line {
                removed.push('\n');
            }
        }
        self.lines.splice(a.line..=b.line, [head + &tail]);
        self.version += 1;
        self.modified = true;
        removed
    }

    pub fn insert(&mut self, at: Pos, text: &str, cursor: Pos) -> Pos {
        let at = self.clamp(at);
        self.record(Op::Insert { at, text: text.to_string() }, cursor);
        self.raw_insert(at, text)
    }

    pub fn delete(&mut self, from: Pos, to: Pos, cursor: Pos) -> String {
        let (a, _) = ordered(self.clamp(from), self.clamp(to));
        let removed = self.raw_delete(from, to);
        if !removed.is_empty() {
            self.record(Op::Delete { at: a, text: removed.clone() }, cursor);
        }
        removed
    }

    /// Undo the last group; returns the cursor to restore.
    pub fn undo(&mut self) -> Option<Pos> {
        self.open_group = false;
        let g = self.undo.pop()?;
        for op in g.ops.iter().rev() {
            match op {
                Op::Insert { at, text } => {
                    let end = end_of_insert(*at, text);
                    self.raw_delete(*at, end);
                }
                Op::Delete { at, text } => {
                    self.raw_insert(*at, text);
                }
            }
        }
        let c = g.cursor_before;
        self.redo.push(g);
        Some(c)
    }

    pub fn redo(&mut self) -> Option<Pos> {
        self.open_group = false;
        let g = self.redo.pop()?;
        let mut cursor = g.cursor_before;
        for op in &g.ops {
            cursor = match op {
                Op::Insert { at, text } => self.raw_insert(*at, text),
                Op::Delete { at, text } => {
                    self.raw_delete(*at, end_of_insert(*at, text));
                    *at
                }
            };
        }
        self.undo.push(g);
        Some(cursor)
    }
}

// ---------------------------------------------------------------------------
// Styled lines and the view
// ---------------------------------------------------------------------------

/// Background block decoration (chat cards, diff hunks).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Block {
    pub bg: Color32,
    pub bar: Option<Color32>,
    pub top: bool,
    pub bottom: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct StyledLine {
    pub text: String,
    pub face: Face,
    /// Byte ranges overriding `face`.
    pub spans: Vec<(Range<usize>, Face)>,
    pub block: Option<Block>,
    /// Left indentation in points.
    pub indent: f32,
    /// Extra space above the line in points.
    pub gap_above: f32,
}

impl StyledLine {
    pub fn new(text: impl Into<String>, face: Face) -> Self {
        StyledLine {
            text: text.into(),
            face,
            ..Default::default()
        }
    }
    pub fn block(mut self, b: Block) -> Self {
        self.block = Some(b);
        self
    }
    pub fn indent(mut self, i: f32) -> Self {
        self.indent = i;
        self
    }
    pub fn gap(mut self, g: f32) -> Self {
        self.gap_above = g;
        self
    }
}

pub fn layout_job(line: &StyledLine, theme: &Theme, wrap_width: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap_width;
    job.halign = egui::Align::LEFT;
    let fmt = |face: Face| {
        let (font_id, color, background) = theme.face(face);
        TextFormat {
            font_id,
            color,
            background,
            italics: face == Face::Thinking,
            ..Default::default()
        }
    };
    if line.spans.is_empty() {
        job.append(&line.text, 0.0, fmt(line.face));
        return job;
    }
    let mut spans = line.spans.clone();
    spans.sort_by_key(|(r, _)| r.start);
    let mut at = 0;
    for (r, face) in spans {
        let (s, e) = (r.start.min(line.text.len()), r.end.min(line.text.len()));
        if s < at || !line.text.is_char_boundary(s) || !line.text.is_char_boundary(e) {
            continue;
        }
        if s > at {
            job.append(&line.text[at..s], 0.0, fmt(line.face));
        }
        job.append(&line.text[s..e], 0.0, fmt(face));
        at = e;
    }
    if at < line.text.len() {
        job.append(&line.text[at..], 0.0, fmt(line.face));
    }
    job
}

/// Source of styled lines for a [`DocView`].
pub trait LineSource {
    fn len(&self) -> usize;
    fn styled(&self, i: usize) -> StyledLine;
    /// Cheap per-line content hash used to invalidate cached heights.
    fn hash(&self, i: usize) -> u64;
}

/// Plain monospace text from a [`Doc`].
pub struct PlainLines<'a> {
    pub lines: &'a [String],
    pub face: Face,
}

fn hash_str(s: &str, extra: u64) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    extra.hash(&mut h);
    h.finish()
}

impl LineSource for PlainLines<'_> {
    fn len(&self) -> usize {
        self.lines.len()
    }
    fn styled(&self, i: usize) -> StyledLine {
        StyledLine::new(self.lines[i].clone(), self.face)
    }
    fn hash(&self, i: usize) -> u64 {
        hash_str(&self.lines[i], self.face as u64)
    }
}

impl LineSource for [StyledLine] {
    fn len(&self) -> usize {
        <[StyledLine]>::len(self)
    }
    fn styled(&self, i: usize) -> StyledLine {
        self[i].clone()
    }
    fn hash(&self, i: usize) -> u64 {
        let l = &self[i];
        hash_str(&l.text, (l.face as u64) ^ (l.spans.len() as u64) << 8 ^ (l.indent as u64) << 16 ^ (l.gap_above as u64) << 24)
    }
}

#[derive(Clone)]
struct Placed {
    line: usize,
    origin: Pos2,
    galley: Arc<Galley>,
    rect: Rect,
}

/// View state for a document: cursor, selection, scroll and the last layout.
#[derive(Clone)]
pub struct DocView {
    pub cursor: Pos,
    pub anchor: Option<Pos>,
    pub scroll_y: f32,
    /// Keep the view pinned to the end while new lines arrive (chat streaming).
    pub follow_tail: bool,
    pub wrap: bool,
    pub padding: Vec2,
    reveal: bool,
    heights: Vec<(u64, f32)>,
    width_key: f32,
    placed: Vec<Placed>,
    viewport: Rect,
    /// Preferred column for vertical motion.
    pub goal_col: Option<usize>,
    drag_anchor: Option<Pos>,
}

impl Default for DocView {
    fn default() -> Self {
        DocView {
            cursor: Pos::ZERO,
            anchor: None,
            scroll_y: 0.0,
            follow_tail: false,
            wrap: true,
            padding: egui::vec2(12.0, 8.0),
            reveal: false,
            heights: Vec::new(),
            width_key: 0.0,
            placed: Vec::new(),
            viewport: Rect::NOTHING,
            goal_col: None,
            drag_anchor: None,
        }
    }
}

pub struct ViewOptions {
    pub focused: bool,
    pub cursor: CursorShape,
    pub show_cursor: bool,
}

#[derive(Default)]
pub struct ViewResponse {
    /// A line was clicked (single click, no drag).
    pub clicked_line: Option<usize>,
    pub clicked: bool,
    /// The user scrolled up away from the tail.
    pub scrolled: bool,
}

impl DocView {
    pub fn reveal_cursor(&mut self) {
        self.reveal = true;
    }

    pub fn visible_lines(&self) -> Range<usize> {
        match (self.placed.first(), self.placed.last()) {
            (Some(a), Some(b)) => a.line..b.line + 1,
            _ => 0..0,
        }
    }

    pub fn viewport(&self) -> Rect {
        self.viewport
    }

    pub fn screen_rect(&self, p: Pos) -> Option<Rect> {
        let pl = self.placed.iter().find(|pl| pl.line == p.line)?;
        let r = pl.galley.pos_from_cursor(CCursor::new(p.col));
        let w = if r.width() < 1.0 { 7.0 } else { r.width() };
        Some(Rect::from_min_size(pl.origin + r.min.to_vec2(), egui::vec2(w.max(6.0), r.height())))
    }

    /// Text position under a screen point, if over the text.
    pub fn pos_at(&self, pt: Pos2) -> Option<Pos> {
        let pl = self
            .placed
            .iter()
            .find(|pl| pt.y >= pl.rect.top() && pt.y < pl.rect.bottom())
            .or_else(|| if pt.y < self.viewport.top() { self.placed.first() } else { self.placed.last() })?;
        let c = pl.galley.cursor_from_pos(pt - pl.origin);
        Some(Pos::new(pl.line, c.index.0))
    }

    /// Paint `src` into `rect`. Handles wheel scrolling and mouse cursor placement/selection.
    pub fn show(&mut self, ui: &mut egui::Ui, rect: Rect, src: &dyn LineSource, theme: &Theme, opts: &ViewOptions) -> ViewResponse {
        let mut resp = ViewResponse::default();
        let painter = ui.painter_at(rect);
        let n = src.len();
        let inner_w = (rect.width() - 2.0 * self.padding.x).max(20.0);
        let wrap_w = if self.wrap { inner_w } else { f32::INFINITY };

        // Heights (cached by line hash and width).
        if (self.width_key - inner_w).abs() > 0.5 {
            self.heights.clear();
            self.width_key = inner_w;
        }
        self.heights.resize(n, (u64::MAX, 0.0));
        for i in 0..n {
            let h = src.hash(i);
            if self.heights[i].0 != h {
                let line = src.styled(i);
                let g = painter.layout_job(layout_job(&line, theme, (wrap_w - line.indent).max(20.0)));
                self.heights[i] = (h, g.size().y.max(1.0) + line.gap_above);
            }
        }
        let total: f32 = self.heights.iter().map(|h| h.1).sum::<f32>() + 2.0 * self.padding.y;
        let view_h = rect.height();
        let max_scroll = (total - view_h).max(0.0);

        // Input: wheel and mouse.
        let response = ui.interact(rect, ui.scope_id().with(("docview", rect.min.x as i32, rect.min.y as i32)), Sense::click_and_drag());
        if response.hovered() {
            let dy = ui.input(|i| i.smooth_scroll_delta.y);
            if dy != 0.0 {
                self.scroll_y -= dy;
                if dy > 0.0 {
                    self.follow_tail = false;
                    resp.scrolled = true;
                }
            }
        }
        if self.reveal {
            self.reveal = false;
            let c = self.cursor.line.min(n.saturating_sub(1));
            let top: f32 = self.padding.y + self.heights[..c].iter().map(|h| h.1).sum::<f32>();
            let h = self.heights.get(c).map(|h| h.1).unwrap_or(16.0);
            let margin = (view_h * 0.1).min(40.0);
            if top < self.scroll_y + margin {
                self.scroll_y = top - margin;
            } else if top + h > self.scroll_y + view_h - margin {
                self.scroll_y = top + h - view_h + margin;
            }
        }
        if self.follow_tail {
            self.scroll_y = max_scroll;
        }
        self.scroll_y = self.scroll_y.clamp(0.0, max_scroll);
        if self.scroll_y >= max_scroll - 1.0 && resp.scrolled {
            // Scrolling back to the end re-enables following.
        }

        // Layout visible lines.
        self.viewport = rect;
        self.placed.clear();
        let mut y = rect.top() + self.padding.y - self.scroll_y;
        let selection = self.anchor.map(|a| ordered(a, self.cursor));
        for i in 0..n {
            let h = self.heights[i].1;
            if y + h < rect.top() {
                y += h;
                continue;
            }
            if y > rect.bottom() {
                break;
            }
            let line = src.styled(i);
            let g = painter.layout_job(layout_job(&line, theme, (wrap_w - line.indent).max(20.0)));
            let line_top = y + line.gap_above;
            let origin = egui::pos2(rect.left() + self.padding.x + line.indent, line_top);
            let line_rect = Rect::from_min_max(egui::pos2(rect.left(), y), egui::pos2(rect.right(), y + h));
            if let Some(b) = line.block {
                let block_rect = Rect::from_min_max(
                    egui::pos2(rect.left() + self.padding.x * 0.5, if b.top { line_top - 3.0 } else { y }),
                    egui::pos2(rect.right() - self.padding.x * 0.5, if b.bottom { y + h + 3.0 } else { y + h + 0.5 }),
                );
                let r = theme.radius;
                let cr = egui::CornerRadius {
                    nw: if b.top { r } else { 0 },
                    ne: if b.top { r } else { 0 },
                    sw: if b.bottom { r } else { 0 },
                    se: if b.bottom { r } else { 0 },
                };
                painter.rect_filled(block_rect, cr, b.bg);
                if let Some(bar) = b.bar {
                    painter.rect_filled(
                        Rect::from_min_size(block_rect.min, egui::vec2(3.0, block_rect.height())),
                        egui::CornerRadius { nw: cr.nw, sw: cr.sw, ne: 0, se: 0 },
                        bar,
                    );
                }
            }
            // Selection.
            if let Some((a, b)) = selection {
                if i >= a.line && i <= b.line {
                    let len = char_len(&line.text);
                    let s = if i == a.line { a.col } else { 0 };
                    let e = if i == b.line { b.col } else { len + 1 };
                    paint_range(&painter, &g, origin, s, e.min(len), e > len, theme.selection);
                }
            }
            painter.galley(origin, g.clone(), theme.fg);
            self.placed.push(Placed {
                line: i,
                origin,
                galley: g,
                rect: line_rect,
            });
            y += h;
        }

        // Cursor.
        if opts.show_cursor {
            if let Some(r) = self.screen_rect(self.cursor) {
                let color = if opts.focused { theme.cursor } else { theme.fg_faint };
                match (opts.cursor, opts.focused) {
                    (_, false) | (CursorShape::Hollow, _) => {
                        painter.rect_stroke(r, 1.0, Stroke::new(1.0, color), egui::StrokeKind::Inside);
                    }
                    (CursorShape::Block, true) => {
                        painter.rect_filled(r, 1.0, color.gamma_multiply(0.85));
                        if let Some(c) = self.char_under_cursor(src) {
                            if !c.chars().all(char::is_whitespace) {
                                let font = theme.mono();
                                painter.text(r.left_top(), egui::Align2::LEFT_TOP, c, font, theme.bg);
                            }
                        }
                    }
                    (CursorShape::Bar, true) => {
                        painter.rect_filled(Rect::from_min_size(r.min, egui::vec2(2.0, r.height())), 0.0, color);
                    }
                    (CursorShape::Underline, true) => {
                        painter.rect_filled(Rect::from_min_max(egui::pos2(r.left(), r.bottom() - 2.0), r.max), 0.0, color);
                    }
                }
            }
        }

        // Mouse placement and drag selection.
        if let Some(pt) = response.interact_pointer_pos() {
            if response.drag_started() || response.clicked() {
                if let Some(p) = self.pos_at(pt) {
                    self.drag_anchor = Some(p);
                    self.cursor = p;
                    self.anchor = None;
                    self.goal_col = None;
                }
            } else if response.dragged() {
                if let (Some(a), Some(p)) = (self.drag_anchor, self.pos_at(pt)) {
                    if p != a {
                        self.anchor = Some(a);
                    }
                    self.cursor = p;
                }
            }
        }
        if response.clicked() {
            resp.clicked = true;
            resp.clicked_line = response.interact_pointer_pos().and_then(|pt| self.pos_at(pt)).map(|p| p.line);
        }

        // Scrollbar.
        if total > view_h + 1.0 {
            let frac = view_h / total;
            let bar_h = (view_h * frac).max(24.0);
            let t = if max_scroll > 0.0 { self.scroll_y / max_scroll } else { 0.0 };
            let bar = Rect::from_min_size(
                egui::pos2(rect.right() - 5.0, rect.top() + t * (view_h - bar_h)),
                egui::vec2(3.0, bar_h),
            );
            painter.rect_filled(bar, 2.0, theme.fg_faint.gamma_multiply(if response.hovered() { 0.8 } else { 0.35 }));
        }
        resp
    }

    fn char_under_cursor(&self, src: &dyn LineSource) -> Option<String> {
        if self.cursor.line >= src.len() {
            return None;
        }
        src.styled(self.cursor.line).text.chars().nth(self.cursor.col).map(String::from)
    }
}

fn paint_range(painter: &egui::Painter, g: &Galley, origin: Pos2, s: usize, e: usize, to_eol: bool, color: Color32) {
    let a = g.pos_from_cursor(CCursor::new(s));
    let b = g.pos_from_cursor(CCursor::new(e));
    let right_edge = g.rect.width().max(b.max.x) + if to_eol { 6.0 } else { 0.0 };
    if (a.min.y - b.min.y).abs() < 1.0 {
        let r = Rect::from_min_max(a.min, egui::pos2(b.min.x + if to_eol { 6.0 } else { 0.0 }, a.max.y));
        painter.rect_filled(r.translate(origin.to_vec2()), 2.0, color);
        return;
    }
    painter.rect_filled(Rect::from_min_max(a.min, egui::pos2(right_edge, a.max.y)).translate(origin.to_vec2()), 2.0, color);
    if b.min.y > a.max.y + 0.5 {
        painter.rect_filled(Rect::from_min_max(egui::pos2(0.0, a.max.y), egui::pos2(right_edge, b.min.y)).translate(origin.to_vec2()), 2.0, color);
    }
    painter.rect_filled(Rect::from_min_max(egui::pos2(0.0, b.min.y), egui::pos2(b.min.x + if to_eol { 6.0 } else { 0.0 }, b.max.y)).translate(origin.to_vec2()), 2.0, color);
}

/// A [`Doc`] plus its [`DocView`]: the editable text surface used by text buffers,
/// the chat prompt and the minibuffer.
#[derive(Clone, Default)]
pub struct TextArea {
    pub doc: Doc,
    pub view: DocView,
    pub read_only: bool,
}

impl TextArea {
    pub fn new(text: &str) -> Self {
        TextArea {
            doc: Doc::from_text(text),
            view: DocView::default(),
            read_only: false,
        }
    }
    pub fn text(&self) -> String {
        self.doc.text()
    }
    pub fn set_text(&mut self, text: &str) {
        self.doc.set_text(text);
        self.view.cursor = self.doc.clamp(self.view.cursor);
        self.view.anchor = None;
    }
    pub fn clear(&mut self) {
        self.set_text("");
        self.view.cursor = Pos::ZERO;
    }
    pub fn move_to_end(&mut self) {
        let l = self.doc.lines.len() - 1;
        self.view.cursor = Pos::new(l, char_len(&self.doc.lines[l]));
    }
}

impl TextSurface for TextArea {
    fn line_count(&self) -> usize {
        self.doc.lines.len()
    }
    fn line(&self, i: usize) -> String {
        self.doc.lines.get(i).cloned().unwrap_or_default()
    }
    fn cursor(&self) -> Pos {
        self.view.cursor
    }
    fn set_cursor(&mut self, p: Pos) {
        self.view.cursor = self.doc.clamp(p);
        self.view.reveal_cursor();
    }
    fn anchor(&self) -> Option<Pos> {
        self.view.anchor
    }
    fn set_anchor(&mut self, a: Option<Pos>) {
        self.view.anchor = a.map(|a| self.doc.clamp(a));
    }
    fn editable_at(&self, _p: Pos) -> bool {
        !self.read_only
    }
    fn insert(&mut self, at: Pos, text: &str) -> Pos {
        if self.read_only {
            return at;
        }
        let c = self.view.cursor;
        self.doc.insert(at, text, c)
    }
    fn delete(&mut self, from: Pos, to: Pos) -> String {
        if self.read_only {
            return String::new();
        }
        let c = self.view.cursor;
        self.doc.delete(from, to, c)
    }
    fn undo(&mut self) -> bool {
        match self.doc.undo() {
            Some(c) => {
                self.set_cursor(c);
                true
            }
            None => false,
        }
    }
    fn redo(&mut self) -> bool {
        match self.doc.redo() {
            Some(c) => {
                self.set_cursor(c);
                true
            }
            None => false,
        }
    }
    fn undo_boundary(&mut self) {
        self.doc.boundary();
    }
    fn visible_lines(&self) -> Range<usize> {
        self.view.visible_lines()
    }
    fn screen_rect(&self, p: Pos) -> Option<Rect> {
        self.view.screen_rect(p)
    }
    fn reveal_cursor(&mut self) {
        self.view.reveal_cursor();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_insert_delete_undo() {
        let mut d = Doc::from_text("hello\nworld");
        let end = d.insert(Pos::new(0, 5), " there\nbig", Pos::ZERO);
        assert_eq!(d.text(), "hello there\nbig\nworld");
        assert_eq!(end, Pos::new(1, 3));
        d.boundary();
        let removed = d.delete(Pos::new(0, 5), Pos::new(1, 3), end);
        assert_eq!(removed, " there\nbig");
        assert_eq!(d.text(), "hello\nworld");
        d.undo();
        assert_eq!(d.text(), "hello there\nbig\nworld");
        d.undo();
        assert_eq!(d.text(), "hello\nworld");
        d.redo();
        assert_eq!(d.text(), "hello there\nbig\nworld");
    }
}
