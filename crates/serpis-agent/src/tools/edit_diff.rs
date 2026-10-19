//! Exact/fuzzy text replacement and diff rendering (pi `edit-diff.ts`).
//! Offsets are byte offsets into UTF-8 strings throughout.

use similar::{ChangeTag, DiffOp, TextDiff};
use unicode_normalization::UnicodeNormalization;

#[derive(Debug, Clone)]
pub struct Edit {
    pub old_text: String,
    pub new_text: String,
}

pub fn detect_line_ending(content: &str) -> &'static str {
    match (content.find("\r\n"), content.find('\n')) {
        (_, None) | (None, _) => "\n",
        (Some(crlf), Some(lf)) => {
            if crlf < lf { "\r\n" } else { "\n" }
        }
    }
}

pub fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

pub fn restore_line_endings(text: &str, ending: &str) -> String {
    if ending == "\r\n" { text.replace('\n', "\r\n") } else { text.to_string() }
}

/// Split a leading UTF-8 BOM.
pub fn split_bom(text: &str) -> (&str, &str) {
    match text.strip_prefix('\u{FEFF}') {
        Some(rest) => ("\u{FEFF}", rest),
        None => ("", text),
    }
}

/// NFKC, strip trailing whitespace per line, ASCII-fy smart quotes, dashes and special spaces.
pub fn normalize_for_fuzzy_match(text: &str) -> String {
    let nfkc: String = text.nfkc().collect();
    nfkc.split('\n')
        .map(|l| l.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
        .chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
            '\u{00A0}' | '\u{2002}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
            c => c,
        })
        .collect()
}

pub struct FuzzyMatch {
    pub found: bool,
    pub index: usize,
    pub match_length: usize,
    pub used_fuzzy: bool,
}

pub fn fuzzy_find_text(content: &str, old_text: &str) -> FuzzyMatch {
    if let Some(i) = content.find(old_text) {
        return FuzzyMatch {
            found: true,
            index: i,
            match_length: old_text.len(),
            used_fuzzy: false,
        };
    }
    let fc = normalize_for_fuzzy_match(content);
    let fo = normalize_for_fuzzy_match(old_text);
    match fc.find(&fo) {
        Some(i) => FuzzyMatch {
            found: true,
            index: i,
            match_length: fo.len(),
            used_fuzzy: true,
        },
        None => FuzzyMatch {
            found: false,
            index: 0,
            match_length: 0,
            used_fuzzy: false,
        },
    }
}

fn count_occurrences(content: &str, old_text: &str) -> usize {
    let fc = normalize_for_fuzzy_match(content);
    let fo = normalize_for_fuzzy_match(old_text);
    if fo.is_empty() {
        return 0;
    }
    fc.matches(&fo).count()
}

#[derive(Debug, Clone)]
struct Replacement {
    edit_index: usize,
    index: usize,
    length: usize,
    new_text: String,
}

fn lines_with_endings(content: &str) -> Vec<&str> {
    content.split_inclusive('\n').collect()
}

fn line_spans(content: &str) -> Vec<(usize, usize)> {
    let mut off = 0;
    lines_with_endings(content)
        .into_iter()
        .map(|l| {
            let s = (off, off + l.len());
            off = s.1;
            s
        })
        .collect()
}

fn apply_replacements(content: &str, reps: &[Replacement], offset: usize) -> String {
    let mut result = content.to_string();
    for r in reps.iter().rev() {
        let i = r.index - offset;
        result.replace_range(i..i + r.length, &r.new_text);
    }
    result
}

/// Rewrite only the lines touched by fuzzy-matched replacements; copy every other line
/// byte-for-byte from the original.
fn apply_preserving_unchanged_lines(original: &str, base: &str, reps: &[Replacement]) -> Result<String, String> {
    let orig_lines = lines_with_endings(original);
    let base_lines = line_spans(base);
    if orig_lines.len() != base_lines.len() {
        return Err("Cannot preserve unchanged lines because the base content has a different line count.".into());
    }
    let mut sorted = reps.to_vec();
    sorted.sort_by_key(|r| r.index);
    let mut groups: Vec<(usize, usize, Vec<Replacement>)> = Vec::new();
    for r in sorted {
        let start_line = base_lines
            .iter()
            .position(|(s, e)| r.index >= *s && r.index < *e)
            .ok_or("Replacement range is outside the base content.")?;
        let end = r.index + r.length;
        let mut end_line = start_line;
        while end_line < base_lines.len() && base_lines[end_line].1 < end {
            end_line += 1;
        }
        if end_line >= base_lines.len() {
            return Err("Replacement range is outside the base content.".into());
        }
        let end_line = end_line + 1;
        if let Some(last) = groups.last_mut() {
            if start_line < last.1 {
                last.1 = last.1.max(end_line);
                last.2.push(r);
                continue;
            }
        }
        groups.push((start_line, end_line, vec![r]));
    }
    let mut out = String::new();
    let mut idx = 0;
    for (start, end, reps) in &groups {
        out.push_str(&orig_lines[idx..*start].concat());
        let gs = base_lines[*start].0;
        let ge = base_lines[end - 1].1;
        out.push_str(&apply_replacements(&base[gs..ge], reps, gs));
        idx = *end;
    }
    out.push_str(&orig_lines[idx..].concat());
    Ok(out)
}

#[derive(Debug)]
pub struct AppliedEdits {
    pub base_content: String,
    pub new_content: String,
}

/// Apply replacements, all matched against the same original content (pi `applyEditsToNormalizedContent`).
pub fn apply_edits_to_normalized_content(normalized: &str, edits: &[Edit], path: &str) -> Result<AppliedEdits, String> {
    let edits: Vec<Edit> = edits
        .iter()
        .map(|e| Edit {
            old_text: normalize_to_lf(&e.old_text),
            new_text: normalize_to_lf(&e.new_text),
        })
        .collect();
    let n = edits.len();
    for (i, e) in edits.iter().enumerate() {
        if e.old_text.is_empty() {
            return Err(if n == 1 {
                format!("oldText must not be empty in {path}.")
            } else {
                format!("edits[{i}].oldText must not be empty in {path}.")
            });
        }
    }
    let used_fuzzy = edits.iter().any(|e| fuzzy_find_text(normalized, &e.old_text).used_fuzzy);
    let base = if used_fuzzy { normalize_for_fuzzy_match(normalized) } else { normalized.to_string() };
    let mut matched = Vec::new();
    for (i, e) in edits.iter().enumerate() {
        let m = fuzzy_find_text(&base, &e.old_text);
        if !m.found {
            return Err(if n == 1 {
                format!("Could not find the exact text in {path}. The old text must match exactly including all whitespace and newlines.")
            } else {
                format!("Could not find edits[{i}] in {path}. The oldText must match exactly including all whitespace and newlines.")
            });
        }
        let occ = count_occurrences(&base, &e.old_text);
        if occ > 1 {
            return Err(if n == 1 {
                format!("Found {occ} occurrences of the text in {path}. The text must be unique. Please provide more context to make it unique.")
            } else {
                format!("Found {occ} occurrences of edits[{i}] in {path}. Each oldText must be unique. Please provide more context to make it unique.")
            });
        }
        matched.push(Replacement {
            edit_index: i,
            index: m.index,
            length: m.match_length,
            new_text: e.new_text.clone(),
        });
    }
    matched.sort_by_key(|r| r.index);
    for w in matched.windows(2) {
        if w[0].index + w[0].length > w[1].index {
            return Err(format!(
                "edits[{}] and edits[{}] overlap in {path}. Merge them into one edit or target disjoint regions.",
                w[0].edit_index, w[1].edit_index
            ));
        }
    }
    let new_content = if used_fuzzy {
        apply_preserving_unchanged_lines(normalized, &base, &matched)?
    } else {
        apply_replacements(&base, &matched, 0)
    };
    if new_content == normalized {
        return Err(if n == 1 {
            format!("No changes made to {path}. The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected.")
        } else {
            format!("No changes made to {path}. The replacements produced identical content.")
        });
    }
    Ok(AppliedEdits {
        base_content: normalized.to_string(),
        new_content,
    })
}

/// Standard unified patch with file headers only.
pub fn generate_unified_patch(path: &str, old: &str, new: &str, context: usize) -> String {
    TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(context)
        .header(path, path)
        .to_string()
}

pub struct DiffString {
    pub diff: String,
    pub first_changed_line: Option<usize>,
}

enum Part<'a> {
    Equal(Vec<&'a str>),
    Removed(Vec<&'a str>),
    Added(Vec<&'a str>),
}

fn strip_nl(s: &str) -> &str {
    s.strip_suffix('\n').unwrap_or(s)
}

/// Display diff with line numbers: `+NN line`, `-NN line`, ` NN line`, `    ...` (pi `generateDiffString`).
pub fn generate_diff_string(old: &str, new: &str, context: usize) -> DiffString {
    let diff = TextDiff::from_lines(old, new);
    let old_lines: Vec<&str> = diff.old_slices().to_vec();
    let new_lines: Vec<&str> = diff.new_slices().to_vec();
    let mut parts: Vec<Part> = Vec::new();
    for op in diff.ops() {
        match *op {
            DiffOp::Equal { old_index, len, .. } => {
                parts.push(Part::Equal(old_lines[old_index..old_index + len].iter().map(|l| strip_nl(l)).collect()))
            }
            DiffOp::Delete { old_index, old_len, .. } => {
                parts.push(Part::Removed(old_lines[old_index..old_index + old_len].iter().map(|l| strip_nl(l)).collect()))
            }
            DiffOp::Insert { new_index, new_len, .. } => {
                parts.push(Part::Added(new_lines[new_index..new_index + new_len].iter().map(|l| strip_nl(l)).collect()))
            }
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                parts.push(Part::Removed(old_lines[old_index..old_index + old_len].iter().map(|l| strip_nl(l)).collect()));
                parts.push(Part::Added(new_lines[new_index..new_index + new_len].iter().map(|l| strip_nl(l)).collect()));
            }
        }
    }
    let _ = ChangeTag::Equal;
    let max_line = old.split('\n').count().max(new.split('\n').count());
    let w = max_line.to_string().len();
    let num = |n: usize| format!("{n:>w$}");
    let blank = " ".repeat(w);
    let mut out: Vec<String> = Vec::new();
    let (mut old_n, mut new_n) = (1usize, 1usize);
    let mut last_was_change = false;
    let mut first_changed = None;
    for i in 0..parts.len() {
        match &parts[i] {
            Part::Added(lines) => {
                first_changed.get_or_insert(new_n);
                for l in lines {
                    out.push(format!("+{} {l}", num(new_n)));
                    new_n += 1;
                }
                last_was_change = true;
            }
            Part::Removed(lines) => {
                first_changed.get_or_insert(new_n);
                for l in lines {
                    out.push(format!("-{} {l}", num(old_n)));
                    old_n += 1;
                }
                last_was_change = true;
            }
            Part::Equal(raw) => {
                let next_change = i + 1 < parts.len() && !matches!(parts[i + 1], Part::Equal(_));
                let push_ctx = |out: &mut Vec<String>, l: &str, old_n: &mut usize, new_n: &mut usize| {
                    out.push(format!(" {} {l}", num(*old_n)));
                    *old_n += 1;
                    *new_n += 1;
                };
                if last_was_change && next_change {
                    if raw.len() <= context * 2 {
                        for l in raw {
                            push_ctx(&mut out, l, &mut old_n, &mut new_n);
                        }
                    } else {
                        for l in &raw[..context] {
                            push_ctx(&mut out, l, &mut old_n, &mut new_n);
                        }
                        let skipped = raw.len() - 2 * context;
                        out.push(format!(" {blank} ..."));
                        old_n += skipped;
                        new_n += skipped;
                        for l in &raw[raw.len() - context..] {
                            push_ctx(&mut out, l, &mut old_n, &mut new_n);
                        }
                    }
                } else if last_was_change {
                    let shown = raw.len().min(context);
                    for l in &raw[..shown] {
                        push_ctx(&mut out, l, &mut old_n, &mut new_n);
                    }
                    let skipped = raw.len() - shown;
                    if skipped > 0 {
                        out.push(format!(" {blank} ..."));
                        old_n += skipped;
                        new_n += skipped;
                    }
                } else if next_change {
                    let skipped = raw.len().saturating_sub(context);
                    if skipped > 0 {
                        out.push(format!(" {blank} ..."));
                        old_n += skipped;
                        new_n += skipped;
                    }
                    for l in &raw[skipped..] {
                        push_ctx(&mut out, l, &mut old_n, &mut new_n);
                    }
                } else {
                    old_n += raw.len();
                    new_n += raw.len();
                }
                last_was_change = false;
            }
        }
    }
    DiffString {
        diff: out.join("\n"),
        first_changed_line: first_changed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(o: &str, n: &str) -> Edit {
        Edit {
            old_text: o.into(),
            new_text: n.into(),
        }
    }

    #[test]
    fn exact_multi_edit() {
        let r = apply_edits_to_normalized_content("a\nb\nc\n", &[e("a", "A"), e("c", "C")], "f").unwrap();
        assert_eq!(r.new_content, "A\nb\nC\n");
    }

    #[test]
    fn fuzzy_smart_quotes_preserves_other_lines() {
        let content = "x = \u{201C}hi\u{201D}   \nkeep\u{2019}s\n";
        let r = apply_edits_to_normalized_content(content, &[e("x = \"hi\"", "x = \"bye\"")], "f").unwrap();
        assert_eq!(r.new_content, "x = \"bye\"\nkeep\u{2019}s\n");
    }

    #[test]
    fn duplicate_and_overlap_errors() {
        let err = apply_edits_to_normalized_content("a a", &[e("a", "b")], "f").unwrap_err();
        assert!(err.contains("Found 2 occurrences"));
        let err = apply_edits_to_normalized_content("abcd", &[e("abc", "x"), e("bcd", "y")], "f").unwrap_err();
        assert!(err.contains("overlap"));
    }

    #[test]
    fn diff_string_numbers_lines() {
        let d = generate_diff_string("a\nb\nc\n", "a\nB\nc\n", 4);
        assert_eq!(d.diff, " 1 a\n-2 b\n+2 B\n 3 c");
        assert_eq!(d.first_changed_line, Some(2));
    }
}
