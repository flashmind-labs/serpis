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

use egui::text::{CCursor, LayoutJob};
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

/// A card behind consecutive lines (messages, tool calls, code, tables), like a CSS box: the
/// first line has `top`, the last `bottom`; lines in between continue it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Block {
    pub bg: Color32,
    /// A colored strip on the left edge.
    pub bar: Option<Color32>,
    pub top: bool,
    pub bottom: bool,
    /// A 1px outline around the whole card.
    pub border: Option<Color32>,
    /// Shrink to the widest line and sit at the right edge (chat bubbles), at most this
    /// fraction of the column.
    pub shrink: Option<f32>,
}

impl Block {
    pub fn new(bg: Color32) -> Self {
        Block { bg, bar: None, top: false, bottom: false, border: None, shrink: None }
    }
    pub fn bar(self, c: Color32) -> Self {
        Block { bar: Some(c), ..self }
    }
    pub fn border(self, c: Color32) -> Self {
        Block { border: Some(c), ..self }
    }
    pub fn shrink(self, frac: f32) -> Self {
        Block { shrink: Some(frac), ..self }
    }
    /// This block as line `i` of `n`.
    pub fn nth(self, i: usize, n: usize) -> Self {
        Block { top: i == 0, bottom: i + 1 == n, ..self }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct StyledLine {
    pub text: String,
    pub face: Face,
    /// Byte ranges overriding `face`.
    pub spans: Vec<(Range<usize>, Face)>,
    pub block: Option<Block>,
    /// Left inset in points (inside a block: the padding; a block's right padding matches it).
    pub indent: f32,
    /// Space above the line, outside its block.
    pub gap_above: f32,
    /// Never wrap: shrink the text to fit the width instead.
    pub fit: bool,
    /// Padding above and below the text, inside its block.
    pub pad_top: f32,
    pub pad_bottom: f32,
    /// A hairline under the line, inside its block (table rows, card headers).
    pub rule_below: Option<Color32>,
    /// Extra horizontal space before byte offsets (table columns).
    pub pads: Vec<(usize, f32)>,
    /// A marker hanging left of the text (list bullets and numbers).
    pub marker: Option<(String, Face)>,
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
    pub fn pad(mut self, top: f32, bottom: f32) -> Self {
        self.pad_top = top;
        self.pad_bottom = bottom;
        self
    }
    pub fn rule(mut self, c: Color32) -> Self {
        self.rule_below = Some(c);
        self
    }
    pub fn marker(mut self, m: impl Into<String>, face: Face) -> Self {
        self.marker = Some((m.into(), face));
        self
    }
    pub fn spans(mut self, spans: Vec<(Range<usize>, Face)>) -> Self {
        self.spans = spans;
        self
    }

    /// Hash of everything that affects layout and paint.
    pub fn layout_hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.text.hash(&mut h);
        (self.face as u8).hash(&mut h);
        for (r, f) in &self.spans {
            (r.start, r.end, *f as u8).hash(&mut h);
        }
        for v in [self.indent, self.gap_above, self.pad_top, self.pad_bottom] {
            v.to_bits().hash(&mut h);
        }
        self.fit.hash(&mut h);
        self.rule_below.map(|c| c.to_array()).hash(&mut h);
        for (o, w) in &self.pads {
            (*o, w.to_bits()).hash(&mut h);
        }
        self.marker.as_ref().map(|(m, f)| (m, *f as u8)).hash(&mut h);
        if let Some(b) = &self.block {
            (b.bg.to_array(), b.bar.map(|c| c.to_array()), b.top, b.bottom, b.border.map(|c| c.to_array()), b.shrink.map(f32::to_bits)).hash(&mut h);
        }
        h.finish()
    }
}

pub fn layout_job(line: &StyledLine, theme: &Theme, wrap_width: f32) -> LayoutJob {
    layout_job_scaled(line, theme, wrap_width, 1.0)
}

/// Lay out a line; a `fit` line is shrunk (to half size at most) rather than wrapped.
pub fn layout_line(painter: &egui::Painter, line: &StyledLine, theme: &Theme, wrap_width: f32) -> std::sync::Arc<Galley> {
    if !line.fit {
        return painter.layout_job(layout_job(line, theme, wrap_width));
    }
    let g = painter.layout_job(layout_job(line, theme, f32::INFINITY));
    if g.size().x <= wrap_width || !wrap_width.is_finite() {
        return g;
    }
    let k = (wrap_width / g.size().x).max(0.5);
    painter.layout_job(layout_job_scaled(line, theme, f32::INFINITY, k))
}

/// [`layout_job`] with every font scaled by `scale`.
pub fn layout_job_scaled(line: &StyledLine, theme: &Theme, wrap_width: f32, scale: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap_width;
    job.halign = egui::Align::LEFT;
    // Spans share the line's line height, so bold and inline code sit on its baseline.
    let base_height = theme.format(line.face).line_height;
    let fmt = |face: Face| {
        let mut f = theme.format(face);
        if base_height.is_some() {
            f.line_height = base_height;
        }
        f.font_id.size *= scale;
        f.extra_letter_spacing *= scale;
        f.line_height = f.line_height.map(|h| h * scale);
        f
    };
    if line.spans.is_empty() && line.pads.is_empty() {
        job.append(&line.text, 0.0, fmt(line.face));
        return job;
    }
    // Faces per byte range, then cut at the pad offsets (each pad leads its section).
    let mut runs: Vec<(Range<usize>, Face)> = Vec::new();
    let mut spans = line.spans.clone();
    spans.sort_by_key(|(r, _)| r.start);
    let mut at = 0;
    for (r, face) in spans {
        let (s, e) = (r.start.min(line.text.len()), r.end.min(line.text.len()));
        if s < at || !line.text.is_char_boundary(s) || !line.text.is_char_boundary(e) {
            continue;
        }
        if s > at {
            runs.push((at..s, line.face));
        }
        runs.push((s..e, face));
        at = e;
    }
    if at < line.text.len() || runs.is_empty() {
        runs.push((at..line.text.len(), line.face));
    }
    let pad_at = |o: usize| line.pads.iter().filter(|(p, _)| *p == o).map(|(_, w)| *w).sum::<f32>() * scale;
    for (r, face) in runs {
        let mut cuts: Vec<usize> = line.pads.iter().map(|(o, _)| *o).filter(|o| *o > r.start && *o < r.end && line.text.is_char_boundary(*o)).collect();
        cuts.sort_unstable();
        cuts.dedup();
        let mut from = r.start;
        for c in cuts.into_iter().chain(std::iter::once(r.end)) {
            job.append(&line.text[from..c], pad_at(from), fmt(face));
            from = c;
        }
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
        self[i].layout_hash()
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
    /// Content column width; wider views center it (like a max-width page).
    pub max_width: f32,
    reveal: bool,
    /// Per line: (layout hash, slot height, natural text width plus insets, card).
    heights: Vec<(u64, f32, f32, Option<Block>)>,
    width_key: f32,
    /// [`Theme::layout_key`] the cached heights were measured with.
    style_key: u64,
    placed: Vec<Placed>,
    viewport: Rect,
    /// Preferred column for vertical motion.
    pub goal_col: Option<usize>,
    /// Where the scrollbar thumb was grabbed (offset from its top) during a drag.
    bar_grab: Option<f32>,
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
            max_width: f32::INFINITY,
            reveal: false,
            heights: Vec::new(),
            width_key: 0.0,
            style_key: 0,
            placed: Vec::new(),
            viewport: Rect::NOTHING,
            goal_col: None,
            bar_grab: None,
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
    /// The content column for a view `width` wide: (left inset, column width).
    fn column(&self, width: f32) -> (f32, f32) {
        let side = self.padding.x.max((width - self.max_width) / 2.0);
        (side, (width - 2.0 * side).max(20.0))
    }

    fn wrap_width(&self, line: &StyledLine, col_w: f32) -> f32 {
        wrap_width(self.wrap, line, col_w)
    }
}

/// The wrap width of `line` in a column `col_w` wide (cards and indents take their share).
fn wrap_width(wrap: bool, line: &StyledLine, col_w: f32) -> f32 {
    {
        if !wrap {
            return f32::INFINITY;
        }
        let w = match &line.block {
            Some(b) => col_w * b.shrink.unwrap_or(1.0) - 2.0 * line.indent,
            None => col_w - line.indent,
        };
        w.max(20.0)
    }
}

impl DocView {
    /// Lay out lines whose content or width changed (cached by layout hash, column width and
    /// theme); returns the content height including padding.
    fn update_heights(&mut self, painter: &egui::Painter, col_w: f32, src: &dyn LineSource, theme: &Theme) -> f32 {
        if (self.width_key - col_w).abs() > 0.5 || self.style_key != theme.layout_key() {
            self.heights.clear();
            self.width_key = col_w;
            self.style_key = theme.layout_key();
        }
        let n = src.len();
        self.heights.resize(n, (u64::MAX, 0.0, 0.0, None));
        for i in 0..n {
            let h = src.hash(i);
            if self.heights[i].0 != h {
                let line = src.styled(i);
                let g = layout_line(painter, &line, theme, self.wrap_width(&line, col_w));
                self.heights[i] = (h, g.size().y.max(1.0) + line.gap_above + line.pad_top + line.pad_bottom, g.size().x + 2.0 * line.indent, line.block);
            }
        }
        self.heights.iter().map(|h| h.1).sum::<f32>() + 2.0 * self.padding.y
    }

    /// The height `src` needs at `width`, wrapped lines included (to size a view to its text).
    pub fn measure(&mut self, ui: &egui::Ui, width: f32, src: &dyn LineSource, theme: &Theme) -> f32 {
        let (_, col_w) = self.column(width);
        self.update_heights(ui.painter(), col_w, src, theme)
    }

    pub fn show(&mut self, ui: &mut egui::Ui, rect: Rect, src: &dyn LineSource, theme: &Theme, opts: &ViewOptions) -> ViewResponse {
        let mut resp = ViewResponse::default();
        let painter = ui.painter_at(rect);
        let n = src.len();
        // The content column: centered when the view is wider than `max_width`.
        let (side, col_w) = self.column(rect.width());
        let col_left = rect.left() + side;
        let col_right = col_left + col_w;
        let wrap = self.wrap;
        let wrap_of = move |line: &StyledLine| -> f32 { wrap_width(wrap, line, col_w) };
        let total = self.update_heights(&painter, col_w, src, theme);
        let view_h = rect.height();
        let max_scroll = (total - view_h).max(0.0);

        // The scrollbar: drag the thumb, or click the track to jump there.
        let scrollable = max_scroll > 1.0;
        let track = Rect::from_min_max(egui::pos2(rect.right() - 12.0, rect.top()), rect.max);
        let bar_h = (view_h * view_h / total.max(1.0)).max(28.0).min(view_h);
        let thumb_top = |scroll: f32| rect.top() + if max_scroll > 0.0 { scroll / max_scroll } else { 0.0 } * (view_h - bar_h);
        let mut bar_active = false;
        if scrollable {
            let bresp = ui.interact(track, ui.scope_id().with(("docview-bar", rect.min.x as i32, rect.min.y as i32)), Sense::click_and_drag());
            bar_active = bresp.hovered() || bresp.dragged();
            if let Some(p) = bresp.interact_pointer_pos() {
                let top = thumb_top(self.scroll_y);
                if bresp.drag_started() || (bresp.clicked() && self.bar_grab.is_none()) {
                    // Grabbing the thumb keeps its offset; the track centers the thumb on the pointer.
                    self.bar_grab = Some(if (top..top + bar_h).contains(&p.y) { p.y - top } else { bar_h / 2.0 });
                }
                if let Some(grab) = self.bar_grab.filter(|_| bresp.dragged() || bresp.clicked() || bresp.drag_started()) {
                    let t = ((p.y - grab - rect.top()) / (view_h - bar_h).max(1.0)).clamp(0.0, 1.0);
                    self.scroll_y = t * max_scroll;
                    self.follow_tail = t >= 0.999;
                    resp.scrolled = true;
                }
            }
            if bresp.drag_stopped() || bresp.clicked() {
                self.bar_grab = None;
            }
            if bar_active {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Default);
            }
        }

        // Input: wheel and mouse (the scrollbar strip belongs to the scrollbar).
        let text_rect = if scrollable { Rect::from_min_max(rect.min, egui::pos2(track.left(), rect.bottom())) } else { rect };
        let response = ui.interact(text_rect, ui.scope_id().with(("docview", rect.min.x as i32, rect.min.y as i32)), Sense::click_and_drag());
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
        let radius = theme.radius + 4;
        // Outline of the card being drawn: top edge and horizontal extent.
        let mut outline: Option<(f32, f32, f32, Color32)> = None;
        // Width of the shrink-to-fit card a line belongs to.
        let mut shrink_w: Option<f32> = None;
        for i in 0..n {
            let h = self.heights[i].1;
            let below = y > rect.bottom();
            if below && outline.is_none() {
                break;
            }
            let line = if y + h < rect.top() || below { None } else { Some(src.styled(i)) };
            let block = self.heights[i].3;
            // A shrinking card is as wide as its widest line (measured across the group).
            if let Some(b) = block.filter(|b| b.shrink.is_some()) {
                if b.top || shrink_w.is_none() {
                    let first = (0..=i).rev().find(|j| self.heights[*j].3.is_none_or(|lb| lb.top)).unwrap_or(i);
                    let last = (i..n).find(|j| self.heights[*j].3.is_none_or(|lb| lb.bottom)).unwrap_or(n - 1);
                    let w = self.heights[first..=last].iter().map(|h| h.2).fold(0.0, f32::max);
                    shrink_w = Some(w.min(col_w * b.shrink.unwrap_or(1.0)));
                }
            }
            let (bx0, bx1) = match block {
                Some(b) if b.shrink.is_some() => (col_right - shrink_w.unwrap_or(col_w), col_right),
                _ => (col_left, col_right),
            };
            let Some(line) = line else {
                if let Some(b) = block {
                    if let (Some(c), None, false) = (b.border, outline, b.top) {
                        outline = Some((rect.top() - 40.0, bx0, bx1, c));
                    }
                    if b.bottom {
                        if let Some((top, x0, x1, c)) = outline.take() {
                            if below {
                                painter.rect_stroke(Rect::from_min_max(egui::pos2(x0, top), egui::pos2(x1, y)), radius, Stroke::new(1.0, c), egui::StrokeKind::Inside);
                            }
                        }
                        shrink_w = None;
                    }
                }
                y += h;
                if below && outline.is_none() {
                    break;
                }
                continue;
            };
            let g = layout_line(&painter, &line, theme, wrap_of(&line));
            let slot_top = y + line.gap_above;
            let origin = egui::pos2(if block.is_some() { bx0 + line.indent } else { col_left + line.indent }, slot_top + line.pad_top);
            let line_rect = Rect::from_min_max(egui::pos2(rect.left(), y), egui::pos2(rect.right(), y + h));
            if let Some(b) = block {
                let block_rect = Rect::from_min_max(egui::pos2(bx0, if b.top { slot_top } else { y }), egui::pos2(bx1, y + h));
                let cr = egui::CornerRadius {
                    nw: if b.top { radius } else { 0 },
                    ne: if b.top { radius } else { 0 },
                    sw: if b.bottom { radius } else { 0 },
                    se: if b.bottom { radius } else { 0 },
                };
                painter.rect_filled(block_rect, cr, b.bg);
                if let Some(bar) = b.bar {
                    painter.rect_filled(
                        Rect::from_min_size(block_rect.min, egui::vec2(3.0, block_rect.height())),
                        egui::CornerRadius { nw: cr.nw, sw: cr.sw, ne: 0, se: 0 },
                        bar,
                    );
                }
                if let Some(c) = b.border {
                    if b.top || outline.is_none() {
                        outline = Some((if b.top { slot_top } else { rect.top() - 40.0 }, bx0, bx1, c));
                    }
                }
            }
            if let (Some(c), false) = (line.rule_below, block.is_some_and(|b| b.bottom)) {
                painter.hline(bx0..=bx1, (y + h).round() - 0.5, Stroke::new(1.0, c));
            }
            if let Some((m, face)) = &line.marker {
                let mg = painter.layout_job(layout_job(&StyledLine::new(m.clone(), *face), theme, f32::INFINITY));
                let first_row = g.rows.first().map(|r| r.rect().height()).unwrap_or(mg.size().y);
                painter.galley(egui::pos2(origin.x - 8.0 - mg.size().x, origin.y + (first_row - mg.size().y) / 2.0), mg, theme.fg);
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
            if let Some(b) = block {
                if b.bottom {
                    if let Some((top, x0, x1, c)) = outline.take() {
                        painter.rect_stroke(Rect::from_min_max(egui::pos2(x0, top), egui::pos2(x1, y + h)), radius, Stroke::new(1.0, c), egui::StrokeKind::Inside);
                    }
                    shrink_w = None;
                }
            }
            self.placed.push(Placed {
                line: i,
                origin,
                galley: g,
                rect: line_rect,
            });
            y += h;
        }
        // A card running past the bottom edge.
        if let Some((top, x0, x1, c)) = outline {
            painter.rect_stroke(Rect::from_min_max(egui::pos2(x0, top), egui::pos2(x1, rect.bottom() + 40.0)), radius, Stroke::new(1.0, c), egui::StrokeKind::Inside);
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

        // Scrollbar: a thin thumb that widens under the pointer.
        if scrollable {
            let w = if bar_active { 7.0 } else { 4.0 };
            let bar = Rect::from_min_size(egui::pos2(rect.right() - w - 3.0, thumb_top(self.scroll_y)), egui::vec2(w, bar_h));
            let alpha = if bar_active { 0.9 } else if response.hovered() { 0.6 } else { 0.35 };
            painter.rect_filled(bar, w / 2.0, theme.fg_faint.gamma_multiply(alpha));
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
