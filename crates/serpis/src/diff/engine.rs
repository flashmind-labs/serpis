//! Comparing two versions of a file, pure and without any UI.
//!
//! Line diffs mark a whole line for a renamed variable and a whole block for a change of
//! indentation. This compares *tokens* instead, as difftastic does, with bat's grammars telling
//! strings and comments apart, so it knows about every language serpis highlights:
//!
//! 1. Lines are compared by their tokens (patience), so re-indented lines still match. Hunks
//!    that could sit in several places slide to where they read best (a whole function, not the
//!    end of one and the start of the next).
//! 2. Inside each changed chunk the tokens are compared across line breaks. A token that exists
//!    on both sides isn't a change, wherever the line breaks went: re-wrapping a call changes
//!    nothing, and adding one argument marks one argument.
//! 3. Those matches decide which old line sits across from which new line, as JetBrains' side by
//!    side view does. Lines that share little aren't paired or marked word by word.
//! 4. Blocks that were cut here and pasted there are found, and shown as moved.
//!
//! This isn't a syntax tree comparison: a token here has no idea which node it belongs to, so a
//! change of nesting reads as tokens added around unchanged ones. It works for any language with
//! a grammar, and never fails on code that doesn't parse (half of what an agent is writing).

use std::collections::HashMap;
use std::ops::Range;
use std::time::{Duration, Instant};

use similar::{Algorithm, DiffOp, capture_diff_slices_deadline};

use crate::highlight::{self, Spans};
use crate::theme::Face;

/// What whitespace means when comparing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Whitespace {
    /// Whitespace between tokens isn't a change, but it shows: dimmed, skipped by next-change.
    /// It still counts inside strings, and for indentation where indentation is syntax.
    #[default]
    Smart,
    /// As `Smart`, but those lines look unchanged and fold away.
    Hide,
    /// Every byte counts.
    Exact,
}

impl Whitespace {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "smart" => Whitespace::Smart,
            "hide" => Whitespace::Hide,
            "exact" => Whitespace::Exact,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Whitespace::Smart => "smart",
            Whitespace::Hide => "hide",
            Whitespace::Exact => "exact",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Options {
    pub whitespace: Whitespace,
    /// Blocks of at least this many lines count as moved (0: never).
    pub move_lines: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options { whitespace: Whitespace::Smart, move_lines: 3 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Kind {
    #[default]
    Same,
    /// Only whitespace or line breaks differ.
    Format,
    Changed,
    /// Removed here and added elsewhere in the file.
    Moved,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Line {
    pub kind: Kind,
    /// The bytes that changed. Empty on a changed line: all of it did.
    pub marks: Vec<Range<usize>>,
    /// Where a moved line went (or came from): its index on the other side.
    pub moved: Option<u32>,
}

/// One version of the file.
#[derive(Debug, Clone, Default)]
pub struct Side {
    pub text: Vec<String>,
    pub lines: Vec<Line>,
    /// Syntax colors per line (none when the language is unknown or the file huge).
    pub spans: Vec<Spans>,
    pub final_newline: bool,
    pub crlf: bool,
}

/// A row of the side by side view; indexes into each side's lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub old: Option<u32>,
    pub new: Option<u32>,
    /// Rows of one replacement: the unified view shows all their old lines, then their new ones.
    pub block: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub rows: Range<usize>,
    /// Only whitespace or line breaks differ.
    pub format_only: bool,
}

#[derive(Debug, Clone, Default)]
pub struct FileDiff {
    pub old: Side,
    pub new: Side,
    pub rows: Vec<Row>,
    pub hunks: Vec<Hunk>,
    pub added: u64,
    pub removed: u64,
}

/// Chunks with more tokens than this get line marks only, and a line diff can run this long.
const MAX_TOKENS: usize = 60_000;
const LINE_TIME: Duration = Duration::from_millis(400);
const TOKEN_TIME: Duration = Duration::from_millis(150);
/// A line shows word marks when at least this share of it is unchanged.
const MIN_KEPT: f32 = 0.34;
/// Two lines sit across from each other when they share at least this much.
const MIN_PAIRED: f32 = 0.3;

/// Files where indentation is syntax: there, changing it counts.
fn indentation_matters(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let ext = name.rsplit('.').next().unwrap_or("");
    matches!(ext, "py" | "pyi" | "yaml" | "yml" | "hs" | "nim" | "coffee" | "sass" | "pug" | "fs" | "fsx") || matches!(name, "Makefile" | "makefile" | "GNUmakefile") || ext == "mk"
}

fn weight(token: &str) -> usize {
    token.chars().filter(|c| c.is_alphanumeric()).count().max(1)
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The tokens of a line, as byte ranges.
fn tokenize(line: &str, spans: &Spans, ws: Whitespace, indent_counts: bool) -> Vec<Range<usize>> {
    let in_string = |at: usize| spans.iter().any(|(r, f)| *f == Face::SynString && r.contains(&at));
    let mut out = Vec::new();
    let mut it = line.char_indices().peekable();
    while let Some((start, c)) = it.next() {
        let run = |it: &mut std::iter::Peekable<std::str::CharIndices>, keep: &dyn Fn(char) -> bool| {
            let mut end = start + c.len_utf8();
            while let Some((i, n)) = it.peek().copied() {
                if !keep(n) {
                    break;
                }
                end = i + n.len_utf8();
                it.next();
            }
            end
        };
        if c.is_whitespace() {
            let end = run(&mut it, &char::is_whitespace);
            let counts = ws == Whitespace::Exact || (start == 0 && indent_counts && end < line.len()) || in_string(start);
            if counts {
                out.push(start..end);
            }
        } else if is_word(c) {
            out.push(start..run(&mut it, &is_word));
        } else {
            out.push(start..start + c.len_utf8());
        }
    }
    out
}

/// What a line is compared by: its tokens, so spacing between them doesn't matter.
fn line_key(line: &str, tokens: &[Range<usize>], ws: Whitespace) -> String {
    if ws == Whitespace::Exact {
        return line.to_string();
    }
    let mut key = String::with_capacity(line.len());
    for t in tokens {
        key.push_str(&line[t.clone()]);
        key.push('\u{1f}');
    }
    key
}

fn split_lines(text: &str) -> (Vec<String>, bool, bool) {
    let crlf = text.contains("\r\n");
    let final_newline = text.is_empty() || text.ends_with('\n');
    let mut lines: Vec<String> = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l).to_string()).collect();
    if lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    (lines, final_newline, crlf)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Chunk {
    old: Range<usize>,
    new: Range<usize>,
}

/// What lies between the matched runs. Only `Equal` ops are read: with patience, `similar`'s
/// inserts and deletes can carry indexes that don't line up.
fn chunks_of(ops: &[DiffOp], old_len: usize, new_len: usize) -> Vec<Chunk> {
    let mut out = Vec::new();
    let (mut x, mut y) = (0, 0);
    let equals = ops.iter().filter_map(|op| match *op {
        DiffOp::Equal { old_index, new_index, len } => Some((old_index, new_index, len)),
        _ => None,
    });
    for (o, n, len) in equals.chain(std::iter::once((old_len, new_len, 0))) {
        if o > x || n > y {
            out.push(Chunk { old: x..o, new: y..n });
        }
        x = o + len;
        y = n + len;
    }
    out
}

fn indent_of(line: &str) -> usize {
    line.chars().take_while(|c| c.is_whitespace()).map(|c| if c == '\t' { 4 } else { 1 }).sum()
}

/// How well a block of inserted (or deleted) lines reads at this position: it starts at its
/// outermost indentation and after a blank line, and ends on a blank line or a closing line.
fn slide_score(lines: &[String], at: Range<usize>) -> i32 {
    let blank = |i: usize| lines.get(i).is_none_or(|l| l.trim().is_empty());
    let mut score = -(indent_of(&lines[at.start]) as i32);
    if at.start == 0 || blank(at.start - 1) {
        score += 20;
    }
    if at.end >= lines.len() || blank(at.end) {
        score += 10;
    }
    if blank(at.end - 1) {
        score += 4;
    }
    if blank(at.start) {
        score -= 8;
    }
    score
}

/// Move pure insertions and deletions, among the positions with the same result, to the one
/// that reads best: `}\n\nfn b() {` becomes `fn b() { … }\n\n`.
fn slide(chunks: &mut [Chunk], old_keys: &[String], new_keys: &[String], old: &[String], new: &[String]) {
    for i in 0..chunks.len() {
        let c = chunks[i].clone();
        let inserted = c.old.is_empty();
        if inserted == c.new.is_empty() {
            continue;
        }
        let (run, keys, lines, other, across) = if inserted { (c.new.clone(), new_keys, new, c.old.start, old) } else { (c.old.clone(), old_keys, old, c.new.start, new) };
        let floor = if i == 0 { 0 } else if inserted { chunks[i - 1].new.end } else { chunks[i - 1].old.end };
        let ceil = chunks.get(i + 1).map_or(keys.len(), |n| if inserted { n.new.start } else { n.old.start });
        let (mut up, mut down) = (0, 0);
        while run.start - up > floor && keys[run.start - up - 1] == keys[run.end - up - 1] {
            up += 1;
        }
        while run.end + down < ceil && keys[run.start + down] == keys[run.end + down] {
            down += 1;
        }
        if up + down == 0 {
            continue;
        }
        // Lines that stay exactly as they were beat anything else: `    }` and `}` are the same
        // token, but the one with the same indentation is the one that didn't change.
        let low = run.start - up;
        let exact = |s: usize| (0..up + down).filter(|t| lines[if low + t < s { low + t } else { low + t + run.len() }] == across[other - up + t]).count() as i32;
        let best = (0..=up + down).map(|k| low + k).max_by_key(|&s| (exact(s) * 30 + slide_score(lines, s..s + run.len()), std::cmp::Reverse(s))).unwrap_or(run.start);
        let shift = best as isize - run.start as isize;
        let moved = |r: &Range<usize>| (r.start as isize + shift) as usize..(r.end as isize + shift) as usize;
        chunks[i] = Chunk { old: moved(&c.old), new: moved(&c.new) };
    }
}

/// A chunk that is a replacement and then more lines: `run(a);` → `run(\n a\n);\n}\n\nfn b() {`,
/// followed by an unchanged `}`. Trimming common ends pairs that `}` with the last one, which
/// reads as if the old function grew around the new one. When the line after the chunk is
/// trivial and the same line occurs where the replacement's tokens run out, match it there, and
/// what follows is an insertion of its own (which `slide` can then place).
fn split_tails(chunks: Vec<Chunk>, old_keys: &[String], new_keys: &[String], old: &[String], new: &[String], old_toks: &[Vec<Range<usize>>], new_toks: &[Vec<Range<usize>>]) -> Vec<Chunk> {
    let mut out = Vec::with_capacity(chunks.len());
    let trivial = |k: &str| !k.chars().any(char::is_alphanumeric) && !k.is_empty();
    let count = |t: &[Vec<Range<usize>>], r: Range<usize>| t[r].iter().map(Vec::len).sum::<usize>();
    for c in chunks {
        // The side that is longer may hold the line after the chunk earlier on.
        let (o_end, n_end) = (c.old.end, c.new.end);
        let mut done = false;
        if !c.old.is_empty() && !c.new.is_empty() && o_end < old.len() && n_end < new.len() && trivial(&old_keys[o_end]) {
            let need_new = count(old_toks, c.old.clone());
            let at_new = c.new.clone().rev().skip(1).rev().find(|&q| new_keys[q] == old_keys[o_end] && new[q] == old[o_end] && count(new_toks, c.new.start..q) >= need_new);
            if let Some(q) = at_new {
                out.push(Chunk { old: c.old.clone(), new: c.new.start..q });
                out.push(Chunk { old: o_end + 1..o_end + 1, new: q + 1..n_end + 1 });
                done = true;
            } else {
                let need_old = count(new_toks, c.new.clone());
                let at_old = c.old.clone().rev().skip(1).rev().find(|&q| old_keys[q] == new_keys[n_end] && old[q] == new[n_end] && count(old_toks, c.old.start..q) >= need_old);
                if let Some(q) = at_old {
                    out.push(Chunk { old: c.old.start..q, new: c.new.clone() });
                    out.push(Chunk { old: q + 1..o_end + 1, new: n_end + 1..n_end + 1 });
                    done = true;
                }
            }
        }
        if !done {
            out.push(c);
        }
    }
    out
}

/// Join chunks with only a trivial line between them (a brace, a blank line), so tokens are
/// compared across it: one change reads as one change.
fn join_near(chunks: Vec<Chunk>, old_keys: &[String]) -> Vec<Chunk> {
    let trivial = |k: &str| !k.chars().any(char::is_alphanumeric);
    let mut out: Vec<Chunk> = Vec::new();
    for c in chunks {
        if let Some(last) = out.last_mut() {
            let gap = last.old.end..c.old.start;
            // A chunk with nothing on one side stays as it is: there is nothing to compare.
            let both = !(last.old.is_empty() || last.new.is_empty() || c.old.is_empty() || c.new.is_empty());
            if both && gap.len() <= 1 && old_keys[gap].iter().all(|k| trivial(k)) {
                last.old.end = c.old.end;
                last.new.end = c.new.end;
                continue;
            }
        }
        out.push(c);
    }
    out
}

struct Tok {
    line: usize,
    range: Range<usize>,
}

fn chunk_tokens(lines: Range<usize>, tokens: &[Vec<Range<usize>>]) -> Vec<Tok> {
    lines.flat_map(|l| tokens[l].iter().map(move |r| Tok { line: l, range: r.clone() })).collect()
}

/// Matches `old[i] == new[j]` as (i, j) runs, without short runs of punctuation stranded between
/// changes: `foo(bar)` → `baz(qux)` shares a `(`, but nobody would call that unchanged.
fn token_matches(old: &[&str], new: &[&str]) -> Vec<(usize, usize)> {
    let ops = capture_diff_slices_deadline(Algorithm::Patience, old, new, Some(Instant::now() + TOKEN_TIME));
    let mut out = Vec::new();
    for (k, op) in ops.iter().enumerate() {
        let DiffOp::Equal { old_index, new_index, len } = *op else { continue };
        let strength: usize = old[old_index..old_index + len].iter().map(|t| if t.chars().any(char::is_alphanumeric) { weight(t) } else { 0 }).sum();
        let inner = k > 0 && k + 1 < ops.len();
        if inner && strength == 0 && len <= 2 {
            continue;
        }
        out.extend((0..len).map(|n| (old_index + n, new_index + n)));
    }
    out
}

/// The pairs of lines to show across from each other: strictly increasing on both sides, with
/// the most shared. Matches never cross, so the pairs come sorted and each only competes with
/// its neighbors.
fn pair_lines(shared: &[((usize, usize), usize)], total: impl Fn(bool, usize) -> usize) -> Vec<(usize, usize)> {
    let candidates: Vec<((usize, usize), usize)> = shared
        .iter()
        .copied()
        .filter(|((a, b), w)| 2.0 * *w as f32 / (total(true, *a) + total(false, *b)).max(1) as f32 >= MIN_PAIRED)
        .collect();
    let mut best: Vec<usize> = Vec::with_capacity(candidates.len());
    let mut from: Vec<Option<usize>> = Vec::with_capacity(candidates.len());
    // The best chain among the first k candidates ends where.
    let mut top: Vec<usize> = Vec::with_capacity(candidates.len());
    for (k, ((a, b), w)) in candidates.iter().enumerate() {
        let before = (0..k).rev().find(|&j| candidates[j].0.0 < *a && candidates[j].0.1 < *b);
        let prev = before.map(|j| top[j]);
        best.push(w + prev.map_or(0, |p| best[p]));
        from.push(prev);
        top.push(match k.checked_sub(1).map(|j| top[j]) {
            Some(t) if best[t] >= best[k] => t,
            _ => k,
        });
    }
    let mut out = Vec::new();
    let mut at = top.last().copied();
    while let Some(k) = at {
        out.push(candidates[k].0);
        at = from[k];
    }
    out.reverse();
    // A line split over several sits across from the first of them, not the longest piece.
    let whole = |(a, b): (usize, usize), w: usize| w >= total(true, a).min(total(false, b));
    for i in 0..out.len() {
        let (a, b) = out[i];
        let floor_b = if i == 0 { 0 } else { out[i - 1].1 + 1 };
        let floor_a = if i == 0 { 0 } else { out[i - 1].0 + 1 };
        let earlier = candidates.iter().find(|((x, y), w)| {
            let same_old = *x == a && *y < b && *y >= floor_b;
            let same_new = *y == b && *x < a && *x >= floor_a;
            (same_old || same_new) && whole((*x, *y), *w)
        });
        if let Some(((x, y), _)) = earlier {
            out[i] = (*x, *y);
        }
    }
    out
}

/// Line kinds and marks from which of a side's tokens matched.
fn classify(side: &mut [Line], text: &[String], lines: Range<usize>, toks: &[Tok], kept: &[bool], ws: Whitespace) {
    for l in lines {
        let of_line: Vec<usize> = toks.iter().enumerate().filter(|(_, t)| t.line == l).map(|(i, _)| i).collect();
        let total: usize = of_line.iter().map(|&i| weight(&text[l][toks[i].range.clone()])).sum();
        let same: usize = of_line.iter().filter(|&&i| kept[i]).map(|&i| weight(&text[l][toks[i].range.clone()])).sum();
        let info = &mut side[l];
        if of_line.is_empty() {
            info.kind = if ws == Whitespace::Exact && text[l].is_empty() { Kind::Changed } else { Kind::Format };
        } else if same == total {
            info.kind = Kind::Format;
        } else {
            info.kind = Kind::Changed;
            if same as f32 / total as f32 >= MIN_KEPT {
                // Changed tokens, joined across the spaces between them.
                let mut marks: Vec<Range<usize>> = Vec::new();
                for &i in of_line.iter().filter(|&&i| !kept[i]) {
                    let r = toks[i].range.clone();
                    match marks.last_mut() {
                        Some(m) if text[l][m.end..r.start].trim().is_empty() => m.end = r.end,
                        _ => marks.push(r),
                    }
                }
                info.marks = marks;
            }
        }
    }
}

/// Lines removed in one place and added in another: runs of at least `min` lines.
fn find_moves(old: &mut [Line], new: &mut [Line], old_keys: &[String], new_keys: &[String], min: usize) {
    if min == 0 {
        return;
    }
    let whole = |l: &Line| l.kind == Kind::Changed && l.marks.is_empty();
    let solid = |k: &str| k.chars().any(char::is_alphanumeric);
    let mut at: HashMap<&str, Vec<usize>> = HashMap::new();
    for (j, k) in new_keys.iter().enumerate() {
        if whole(&new[j]) && solid(k) {
            at.entry(k).or_default().push(j);
        }
    }
    let mut i = 0;
    while i < old.len() {
        let mut found = (0, 0);
        if whole(&old[i]) {
            for &j in at.get(old_keys[i].as_str()).into_iter().flatten() {
                let mut n = 0;
                while i + n < old.len() && j + n < new.len() && whole(&old[i + n]) && new[j + n].kind == Kind::Changed && new[j + n].moved.is_none() && new[j + n].marks.is_empty() && old_keys[i + n] == new_keys[j + n] {
                    n += 1;
                }
                if n > found.0 {
                    found = (n, j);
                }
            }
        }
        if found.0 >= min {
            for n in 0..found.0 {
                old[i + n].kind = Kind::Moved;
                old[i + n].moved = Some((found.1 + n) as u32);
                new[found.1 + n].kind = Kind::Moved;
                new[found.1 + n].moved = Some((i + n) as u32);
            }
            i += found.0;
        } else {
            i += 1;
        }
    }
}

/// Rows for the lines up to `(to_x, to_y)`, side by side: one replacement.
fn rows_between(rows: &mut Vec<Row>, block: &mut u32, x: &mut usize, y: &mut usize, to_x: usize, to_y: usize) {
    if *x >= to_x && *y >= to_y {
        return;
    }
    while *x < to_x || *y < to_y {
        let old = (*x < to_x).then_some(*x as u32);
        let new = (*y < to_y).then_some(*y as u32);
        rows.push(Row { old, new, block: *block });
        *x += usize::from(old.is_some());
        *y += usize::from(new.is_some());
    }
    *block += 1;
}

/// Consecutive rows with a difference, as hunks.
pub fn hunks_of(rows: &[Row], old: &[Line], new: &[Line]) -> Vec<Hunk> {
    let kinds = |r: &Row| [r.old.map(|l| old[l as usize].kind), r.new.map(|l| new[l as usize].kind)];
    let mut out: Vec<Hunk> = Vec::new();
    let mut open = false;
    for (i, r) in rows.iter().enumerate() {
        let k = kinds(r);
        let differs = k.iter().flatten().any(|k| *k != Kind::Same);
        if !differs {
            open = false;
            continue;
        }
        let real = k.iter().flatten().any(|k| matches!(k, Kind::Changed | Kind::Moved));
        match out.last_mut() {
            Some(h) if open => {
                h.rows.end = i + 1;
                h.format_only &= !real;
            }
            _ => out.push(Hunk { rows: i..i + 1, format_only: !real }),
        }
        open = true;
    }
    out
}

impl FileDiff {
    /// The rows of the unified view, each showing one line (or one unchanged line of both
    /// sides): a replacement's old lines, then its new ones.
    pub fn unified_rows(&self) -> Vec<Row> {
        let mut out = Vec::with_capacity(self.rows.len());
        let mut i = 0;
        while i < self.rows.len() {
            let b = self.rows[i].block;
            let end = i + self.rows[i..].iter().take_while(|r| r.block == b).count();
            let group = &self.rows[i..end];
            let same = |r: &Row| r.old.is_some_and(|l| self.old.lines[l as usize].kind == Kind::Same) && r.new.is_some_and(|l| self.new.lines[l as usize].kind == Kind::Same);
            if group.iter().all(same) {
                out.extend_from_slice(group);
            } else {
                out.extend(group.iter().filter_map(|r| r.old.map(|l| Row { old: Some(l), new: None, block: b })));
                out.extend(group.iter().filter_map(|r| r.new.map(|l| Row { old: None, new: Some(l), block: b })));
            }
            i = end;
        }
        out
    }
}

/// Compare two versions of the file at `path`.
pub fn compare(old_text: &str, new_text: &str, path: &str, options: &Options) -> FileDiff {
    let ws = options.whitespace;
    let (old, old_nl, old_crlf) = split_lines(old_text);
    let (new, new_nl, new_crlf) = split_lines(new_text);
    let colors = |lines: &[String]| -> Vec<Spans> {
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        highlight::highlight(path, &refs, Face::Code).unwrap_or_else(|| vec![Vec::new(); lines.len()])
    };
    let (old_spans, new_spans) = (colors(&old), colors(&new));
    let indent = ws != Whitespace::Exact && indentation_matters(path);
    let toks = |lines: &[String], spans: &[Spans]| -> Vec<Vec<Range<usize>>> { lines.iter().zip(spans).map(|(l, s)| tokenize(l, s, ws, indent)).collect() };
    let (old_toks, new_toks) = (toks(&old, &old_spans), toks(&new, &new_spans));
    let keys = |lines: &[String], t: &[Vec<Range<usize>>]| -> Vec<String> { lines.iter().zip(t).map(|(l, t)| line_key(l, t, ws)).collect() };
    let (old_keys, new_keys) = (keys(&old, &old_toks), keys(&new, &new_toks));

    let old_refs: Vec<&str> = old_keys.iter().map(String::as_str).collect();
    let new_refs: Vec<&str> = new_keys.iter().map(String::as_str).collect();
    let ops = capture_diff_slices_deadline(Algorithm::Patience, &old_refs, &new_refs, Some(Instant::now() + LINE_TIME));
    let chunks = chunks_of(&ops, old.len(), new.len());
    let mut chunks = split_tails(chunks, &old_keys, &new_keys, &old, &new, &old_toks, &new_toks);
    slide(&mut chunks, &old_keys, &new_keys, &old, &new);
    let chunks = join_near(chunks, &old_keys);

    let mut old_lines = vec![Line::default(); old.len()];
    let mut new_lines = vec![Line::default(); new.len()];
    let mut rows: Vec<Row> = Vec::with_capacity(old.len().max(new.len()));
    let mut block = 0u32;
    let (mut x, mut y) = (0usize, 0usize);
    for c in chunks.iter().chain(std::iter::once(&Chunk { old: old.len()..old.len(), new: new.len()..new.len() })) {
        // The unchanged lines before it, some perhaps spaced differently.
        while x < c.old.start {
            if old[x] != new[y] {
                old_lines[x].kind = Kind::Format;
                new_lines[y].kind = Kind::Format;
            }
            rows.push(Row { old: Some(x as u32), new: Some(y as u32), block });
            block += 1;
            x += 1;
            y += 1;
        }
        if c.old.is_empty() && c.new.is_empty() {
            continue;
        }
        let a = chunk_tokens(c.old.clone(), &old_toks);
        let b = chunk_tokens(c.new.clone(), &new_toks);
        let mut pairs = Vec::new();
        if a.len() <= MAX_TOKENS && b.len() <= MAX_TOKENS {
            let sa: Vec<&str> = a.iter().map(|t| &old[t.line][t.range.clone()]).collect();
            let sb: Vec<&str> = b.iter().map(|t| &new[t.line][t.range.clone()]).collect();
            let (mut kept_a, mut kept_b) = (vec![false; a.len()], vec![false; b.len()]);
            let mut shared: Vec<((usize, usize), usize)> = Vec::new();
            for (i, j) in token_matches(&sa, &sb) {
                kept_a[i] = true;
                kept_b[j] = true;
                let key = (a[i].line, b[j].line);
                let w = weight(sa[i]);
                match shared.last_mut() {
                    Some((k, n)) if *k == key => *n += w,
                    _ => shared.push((key, w)),
                }
            }
            classify(&mut old_lines, &old, c.old.clone(), &a, &kept_a, ws);
            classify(&mut new_lines, &new, c.new.clone(), &b, &kept_b, ws);
            let total = |side: &[Vec<Range<usize>>], text: &[String], l: usize| side[l].iter().map(|r| weight(&text[l][r.clone()])).sum::<usize>();
            pairs = pair_lines(&shared, |is_old, l| if is_old { total(&old_toks, &old, l) } else { total(&new_toks, &new, l) });
        } else {
            old_lines[c.old.clone()].iter_mut().for_each(|l| l.kind = Kind::Changed);
            new_lines[c.new.clone()].iter_mut().for_each(|l| l.kind = Kind::Changed);
        }
        for (pa, pb) in pairs {
            rows_between(&mut rows, &mut block, &mut x, &mut y, pa, pb);
            // Identical lines inside a chunk (a brace it was joined across) didn't change.
            if old[pa] == new[pb] {
                old_lines[pa] = Line::default();
                new_lines[pb] = Line::default();
            }
            rows.push(Row { old: Some(pa as u32), new: Some(pb as u32), block });
            block += 1;
            x = pa + 1;
            y = pb + 1;
        }
        rows_between(&mut rows, &mut block, &mut x, &mut y, c.old.end, c.new.end);
    }
    find_moves(&mut old_lines, &mut new_lines, &old_keys, &new_keys, options.move_lines);
    if ws == Whitespace::Hide {
        for l in old_lines.iter_mut().chain(new_lines.iter_mut()).filter(|l| l.kind == Kind::Format) {
            *l = Line::default();
        }
    }
    let counted = |lines: &[Line]| lines.iter().filter(|l| matches!(l.kind, Kind::Changed | Kind::Moved)).count() as u64;
    let hunks = hunks_of(&rows, &old_lines, &new_lines);
    FileDiff {
        added: counted(&new_lines),
        removed: counted(&old_lines),
        old: Side { text: old, lines: old_lines, spans: old_spans, final_newline: old_nl, crlf: old_crlf },
        new: Side { text: new, lines: new_lines, spans: new_spans, final_newline: new_nl, crlf: new_crlf },
        rows,
        hunks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn cmp(old: &str, new: &str, path: &str) -> FileDiff {
        compare(old, new, path, &Options::default())
    }

    fn marked(side: &Side, line: usize) -> Vec<String> {
        side.lines[line].marks.iter().map(|m| side.text[line][m.clone()].to_string()).collect()
    }

    fn kinds(side: &Side) -> Vec<Kind> {
        side.lines.iter().map(|l| l.kind).collect()
    }

    #[test]
    fn a_renamed_variable_marks_the_name_and_not_the_line() {
        let d = cmp("let total = price * count;\n", "let sum = price * count;\n", "a.rs");
        assert_eq!(d.rows.len(), 1);
        assert_eq!(marked(&d.old, 0), ["total"]);
        assert_eq!(marked(&d.new, 0), ["sum"]);
        assert_eq!((d.added, d.removed), (1, 1));
    }

    #[test]
    fn reindenting_a_block_is_formatting() {
        let old = "fn f() {\n    one();\n    two();\n}\n";
        let new = "fn f() {\n    if on {\n        one();\n        two();\n    }\n}\n";
        let d = cmp(old, new, "a.rs");
        assert_eq!(kinds(&d.new), [Kind::Same, Kind::Changed, Kind::Format, Kind::Format, Kind::Changed, Kind::Same]);
        assert_eq!((d.added, d.removed), (2, 0));
        // Exact counts every byte.
        let e = compare(old, new, "a.rs", &Options { whitespace: Whitespace::Exact, ..Default::default() });
        assert!(e.added >= 4, "{}", e.added);
    }

    #[test]
    fn rewrapping_a_call_changes_nothing() {
        let d = cmp("call(alpha, beta, gamma);\n", "call(\n    alpha,\n    beta,\n    gamma,\n);\n", "a.rs");
        // Only the trailing comma is new.
        assert_eq!(d.added, 1, "{:?}", kinds(&d.new));
        assert!(d.old.lines[0].kind == Kind::Format, "{:?}", d.old.lines[0]);
        assert_eq!(marked(&d.new, 3), [","]);
        // Across from the line that starts it.
        assert_eq!((d.rows[0].old, d.rows[0].new), (Some(0), Some(0)), "{:?}", d.rows);
        let one = cmp("call(alpha, beta);\n", "call(\n    alpha,\n    beta);\n", "a.rs");
        assert_eq!((one.added, one.removed), (0, 0));
        assert_eq!(one.hunks.len(), 1);
        assert!(one.hunks[0].format_only);
    }

    #[test]
    fn whitespace_counts_in_strings_and_where_indentation_is_syntax() {
        let d = cmp("say(\"a b\");\n", "say(\"a  b\");\n", "a.rs");
        assert_eq!(d.added, 1);
        assert_eq!(marked(&d.new, 0), ["  "]);
        let py = cmp("if x:\n    a()\nb()\n", "if x:\n    a()\n    b()\n", "a.py");
        assert_eq!(py.added, 1, "moving a line into a Python block changes the program");
        let rs = cmp("if x {\n    a();\nb();\n", "if x {\n    a();\n    b();\n", "a.rs");
        assert_eq!(rs.added, 0);
    }

    #[test]
    fn unrelated_lines_are_not_marked_word_by_word() {
        let d = cmp("alpha beta gamma delta;\n", "one(two, three) + four;\n", "a.rs");
        assert_eq!(d.old.lines[0].kind, Kind::Changed);
        assert!(d.old.lines[0].marks.is_empty() && d.new.lines[0].marks.is_empty());
        // Side by side all the same, and stacked in the unified view.
        assert_eq!(d.rows.len(), 1);
        let u = d.unified_rows();
        assert_eq!(u.len(), 2);
        assert_eq!((u[0].old, u[0].new, u[1].old, u[1].new), (Some(0), None, None, Some(0)));
    }

    #[test]
    fn similar_lines_sit_across_from_each_other() {
        let old = "first_thing(a);\nsecond_thing(b);\n";
        let new = "brand_new_line();\nfirst_thing(a, x);\nanother_new_one();\nsecond_thing(b, y);\n";
        let d = cmp(old, new, "a.rs");
        let pairs: Vec<(Option<u32>, Option<u32>)> = d.rows.iter().map(|r| (r.old, r.new)).collect();
        assert!(pairs.contains(&(Some(0), Some(1))), "{pairs:?}");
        assert!(pairs.contains(&(Some(1), Some(3))), "{pairs:?}");
    }

    #[test]
    fn an_inserted_function_slides_to_read_as_a_whole() {
        let old = "fn a() {\n    x();\n}\n\nfn c() {\n    z();\n}\n";
        let new = "fn a() {\n    x();\n}\n\nfn b() {\n    y();\n}\n\nfn c() {\n    z();\n}\n";
        let d = cmp(old, new, "a.rs");
        let changed: Vec<&str> = d.new.lines.iter().zip(&d.new.text).filter(|(l, _)| l.kind != Kind::Same).map(|(_, t)| t.as_str()).collect();
        assert_eq!(changed, ["fn b() {", "    y();", "}", ""]);
    }

    #[test]
    fn a_closing_line_pairs_with_the_nearest_one() {
        let old = "fn f() {\n    run(a, b);\n}\n";
        let new = "fn f() {\n    run(\n        a,\n        b\n    );\n}\n\nfn g() {\n    more();\n}\n";
        let d = cmp(old, new, "a.rs");
        // The old `}` is the first new `}`; the new function is all that was added.
        let row = d.rows.iter().find(|r| r.old == Some(2)).unwrap();
        assert_eq!(row.new, Some(5), "{:?}", d.rows);
        assert_eq!(d.new.lines[5].kind, Kind::Same);
        let changed: Vec<&str> = d.new.lines.iter().zip(&d.new.text).filter(|(l, _)| l.kind == Kind::Changed).map(|(_, t)| t.as_str()).collect();
        assert_eq!(changed, ["fn g() {", "    more();", "}"]);
        // Deleting the same way.
        let d = cmp(new, old, "a.rs");
        let row = d.rows.iter().find(|r| r.new == Some(2)).unwrap();
        assert_eq!(row.old, Some(5), "{:?}", d.rows);
    }

    #[test]
    fn moved_blocks_are_found() {
        let block = "fn moved_one() {\n    body_of_it();\n    more_of_it();\n}\n";
        let keep = "fn keep_this() {\n    still_here();\n}\n";
        let tail = "fn at_the_end() {\n    final_thing();\n}\n";
        let d = cmp(&format!("{block}{keep}{tail}"), &format!("{keep}{tail}{block}"), "a.rs");
        assert_eq!(d.old.lines[0].kind, Kind::Moved);
        assert_eq!(d.old.lines[0].moved, Some(6));
        assert_eq!(d.new.lines[6].moved, Some(0));
        let off = compare(&format!("{block}{keep}{tail}"), &format!("{keep}{tail}{block}"), "a.rs", &Options { move_lines: 0, ..Default::default() });
        assert_eq!(off.old.lines[0].kind, Kind::Changed);
    }

    #[test]
    fn hide_folds_formatting_away() {
        let d = compare("a(1);\nb(2);\n", "  a(1);\nb(3);\n", "a.rs", &Options { whitespace: Whitespace::Hide, ..Default::default() });
        assert_eq!(d.new.lines[0].kind, Kind::Same);
        assert_eq!(d.hunks.len(), 1);
        assert!(!d.hunks[0].format_only);
    }

    #[test]
    fn line_endings_and_final_newlines_are_noted_not_changed() {
        let d = cmp("a\nb\n", "a\r\nb", "a.txt");
        assert_eq!((d.added, d.removed), (0, 0));
        assert!(d.new.crlf && !d.old.crlf);
        assert!(d.old.final_newline && !d.new.final_newline);
        assert!(d.hunks.is_empty());
    }

    #[test]
    fn new_and_deleted_files() {
        let d = cmp("", "a\nb\n", "x.txt");
        assert_eq!((d.added, d.removed, d.rows.len()), (2, 0, 2));
        assert_eq!(d.hunks.len(), 1);
        let d = cmp("a\nb\n", "", "x.txt");
        assert_eq!((d.added, d.removed), (0, 2));
        assert!(cmp("", "", "x.txt").rows.is_empty());
    }

    #[test]
    fn a_big_rewrite_finishes_in_good_time() {
        let old: String = (0..30_000).map(|i| format!("let value_{i} = compute({i}, {});\n", i * 7)).collect();
        let new: String = (0..30_000).map(|i| format!("const other_{} = run({i});\n", i * 3)).collect();
        let t = Instant::now();
        let d = cmp(&old, &new, "big.txt");
        assert_eq!(d.rows.len(), 30_000);
        // Debug builds on a busy machine; what matters is that it is bounded.
        assert!(t.elapsed() < Duration::from_secs(20), "{:?}", t.elapsed());
    }

    fn text() -> impl Strategy<Value = String> {
        let line = prop_oneof![
            Just(String::new()),
            Just("}".to_string()),
            Just("    x = 1;".to_string()),
            Just("x = 1;".to_string()),
            Just("  say(\"é b\", y)".to_string()),
            Just("fn name(a, b) {".to_string()),
            "[ a-c(),é]{0,12}",
        ];
        prop::collection::vec(line, 0..24).prop_map(|l| l.join("\n"))
    }

    proptest! {
        /// Whatever the input, the rows show every line of both versions once, in order, and
        /// every mark can be sliced from its line.
        #[test]
        fn rows_rebuild_both_versions(old in text(), new in text(), mode in 0..3usize) {
            let whitespace = [Whitespace::Smart, Whitespace::Hide, Whitespace::Exact][mode];
            let d = compare(&old, &new, "a.rs", &Options { whitespace, move_lines: 2 });
            let of = |pick: fn(&Row) -> Option<u32>, rows: &[Row]| rows.iter().filter_map(pick).map(|l| l as usize).collect::<Vec<_>>();
            for rows in [d.rows.clone(), d.unified_rows()] {
                prop_assert_eq!(of(|r| r.old, &rows), (0..d.old.text.len()).collect::<Vec<_>>());
                prop_assert_eq!(of(|r| r.new, &rows), (0..d.new.text.len()).collect::<Vec<_>>());
            }
            for side in [&d.old, &d.new] {
                for (l, t) in side.lines.iter().zip(&side.text) {
                    for m in &l.marks {
                        prop_assert!(t.get(m.clone()).is_some());
                    }
                }
            }
            for h in &d.hunks {
                prop_assert!(h.rows.start < h.rows.end && h.rows.end <= d.rows.len());
            }
            // Identical text never shows a change, and different text in exact mode always does.
            if old == new {
                prop_assert!(d.hunks.is_empty());
            }
            if whitespace == Whitespace::Exact && d.old.text != d.new.text {
                prop_assert!(!d.hunks.is_empty());
            }
        }
    }
}
