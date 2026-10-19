//! Shared truncation utilities for tool outputs (pi `truncate.ts`).
//!
//! Two independent limits, whichever is hit first wins: lines (default 2000)
//! and bytes (default 50KB). Never returns partial lines, except for the
//! tail-truncation edge case where the last line alone exceeds the byte limit.

use serde::Serialize;

pub const DEFAULT_MAX_LINES: usize = 2000;
pub const DEFAULT_MAX_BYTES: usize = 50 * 1024;
pub const GREP_MAX_LINE_LENGTH: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TruncatedBy {
    Lines,
    Bytes,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TruncationResult {
    pub content: String,
    pub truncated: bool,
    pub truncated_by: Option<TruncatedBy>,
    pub total_lines: usize,
    pub total_bytes: usize,
    pub output_lines: usize,
    pub output_bytes: usize,
    pub last_line_partial: bool,
    pub first_line_exceeds_limit: bool,
    pub max_lines: usize,
    pub max_bytes: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_lines: usize,
    pub max_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_lines: DEFAULT_MAX_LINES,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

impl Limits {
    pub fn bytes_only() -> Self {
        Limits {
            max_lines: usize::MAX,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

fn split_lines_for_counting(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = content.split('\n').collect();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

pub fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn untruncated(content: &str, total_lines: usize, limits: Limits) -> TruncationResult {
    TruncationResult {
        content: content.to_string(),
        truncated: false,
        truncated_by: None,
        total_lines,
        total_bytes: content.len(),
        output_lines: total_lines,
        output_bytes: content.len(),
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines: limits.max_lines,
        max_bytes: limits.max_bytes,
    }
}

/// Keep the first lines/bytes. Suitable for file reads.
pub fn truncate_head(content: &str, limits: Limits) -> TruncationResult {
    let total_bytes = content.len();
    let lines = split_lines_for_counting(content);
    let total_lines = lines.len();
    if total_lines <= limits.max_lines && total_bytes <= limits.max_bytes {
        return untruncated(content, total_lines, limits);
    }
    if lines[0].len() > limits.max_bytes {
        return TruncationResult {
            content: String::new(),
            truncated: true,
            truncated_by: Some(TruncatedBy::Bytes),
            total_lines,
            total_bytes,
            output_lines: 0,
            output_bytes: 0,
            last_line_partial: false,
            first_line_exceeds_limit: true,
            max_lines: limits.max_lines,
            max_bytes: limits.max_bytes,
        };
    }
    let mut out: Vec<&str> = Vec::new();
    let mut bytes = 0;
    let mut by = TruncatedBy::Lines;
    for (i, line) in lines.iter().enumerate() {
        if i >= limits.max_lines {
            break;
        }
        let lb = line.len() + usize::from(i > 0);
        if bytes + lb > limits.max_bytes {
            by = TruncatedBy::Bytes;
            break;
        }
        out.push(line);
        bytes += lb;
    }
    if out.len() >= limits.max_lines && bytes <= limits.max_bytes {
        by = TruncatedBy::Lines;
    }
    let content = out.join("\n");
    TruncationResult {
        output_bytes: content.len(),
        output_lines: out.len(),
        content,
        truncated: true,
        truncated_by: Some(by),
        total_lines,
        total_bytes,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines: limits.max_lines,
        max_bytes: limits.max_bytes,
    }
}

fn truncate_str_to_bytes_from_end(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut start = s.len() - max_bytes;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

/// Keep the last lines/bytes. Suitable for command output.
pub fn truncate_tail(content: &str, limits: Limits) -> TruncationResult {
    let total_bytes = content.len();
    let lines = split_lines_for_counting(content);
    let total_lines = lines.len();
    if total_lines <= limits.max_lines && total_bytes <= limits.max_bytes {
        return untruncated(content, total_lines, limits);
    }
    let mut out: Vec<String> = Vec::new();
    let mut bytes = 0;
    let mut by = TruncatedBy::Lines;
    let mut last_line_partial = false;
    for line in lines.iter().rev() {
        if out.len() >= limits.max_lines {
            break;
        }
        let lb = line.len() + usize::from(!out.is_empty());
        if bytes + lb > limits.max_bytes {
            by = TruncatedBy::Bytes;
            if out.is_empty() {
                let t = truncate_str_to_bytes_from_end(line, limits.max_bytes);
                bytes = t.len();
                out.push(t.to_string());
                last_line_partial = true;
            }
            break;
        }
        out.push(line.to_string());
        bytes += lb;
    }
    out.reverse();
    if out.len() >= limits.max_lines && bytes <= limits.max_bytes {
        by = TruncatedBy::Lines;
    }
    let content = out.join("\n");
    TruncationResult {
        output_bytes: content.len(),
        output_lines: out.len(),
        content,
        truncated: true,
        truncated_by: Some(by),
        total_lines,
        total_bytes,
        last_line_partial,
        first_line_exceeds_limit: false,
        max_lines: limits.max_lines,
        max_bytes: limits.max_bytes,
    }
}

/// Truncate a single line to `max_chars` characters with a `... [truncated]` suffix.
pub fn truncate_line(line: &str, max_chars: usize) -> (String, bool) {
    // JS string length counts UTF-16 units; char count is the closest stable equivalent.
    if line.chars().count() <= max_chars {
        return (line.to_string(), false);
    }
    (format!("{}... [truncated]", line.chars().take(max_chars).collect::<String>()), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_respects_line_limit() {
        let text = (1..=10).map(|i| format!("l{i}")).collect::<Vec<_>>().join("\n");
        let r = truncate_head(&text, Limits { max_lines: 3, max_bytes: 1000 });
        assert!(r.truncated);
        assert_eq!(r.content, "l1\nl2\nl3");
        assert_eq!(r.truncated_by, Some(TruncatedBy::Lines));
        assert_eq!(r.total_lines, 10);
    }

    #[test]
    fn tail_partial_last_line() {
        let r = truncate_tail("abcdef", Limits { max_lines: 10, max_bytes: 3 });
        assert_eq!(r.content, "def");
        assert!(r.last_line_partial);
    }

    #[test]
    fn head_first_line_too_long() {
        let r = truncate_head("aaaaaaaa\nb", Limits { max_lines: 10, max_bytes: 4 });
        assert!(r.first_line_exceeds_limit);
        assert_eq!(r.content, "");
    }

    #[test]
    fn sizes() {
        assert_eq!(format_size(500), "500B");
        assert_eq!(format_size(51200), "50.0KB");
    }
}
