//! Bounded-memory accumulator for streaming command output (pi `output-accumulator.ts`).
//! Keeps a decoded tail for display and spools the full output to a temp file once it
//! exceeds the limits.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use super::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, Limits, TruncatedBy, TruncationResult, truncate_tail};

pub struct OutputSnapshot {
    pub content: String,
    pub truncation: TruncationResult,
    pub full_output_path: Option<PathBuf>,
}

pub struct OutputAccumulator {
    max_lines: usize,
    max_bytes: usize,
    max_rolling_bytes: usize,
    temp_prefix: String,
    pending: Vec<u8>,
    raw_chunks: Vec<u8>,
    tail: String,
    tail_starts_at_line_boundary: bool,
    total_raw_bytes: usize,
    total_decoded_bytes: usize,
    completed_lines: usize,
    total_lines: usize,
    current_line_bytes: usize,
    has_open_line: bool,
    temp_path: Option<PathBuf>,
    temp_file: Option<File>,
}

impl OutputAccumulator {
    pub fn new(temp_prefix: &str) -> Self {
        OutputAccumulator {
            max_lines: DEFAULT_MAX_LINES,
            max_bytes: DEFAULT_MAX_BYTES,
            max_rolling_bytes: DEFAULT_MAX_BYTES * 2,
            temp_prefix: temp_prefix.to_string(),
            pending: Vec::new(),
            raw_chunks: Vec::new(),
            tail: String::new(),
            tail_starts_at_line_boundary: true,
            total_raw_bytes: 0,
            total_decoded_bytes: 0,
            completed_lines: 0,
            total_lines: 0,
            current_line_bytes: 0,
            has_open_line: false,
            temp_path: None,
            temp_file: None,
        }
    }

    pub fn append(&mut self, data: &[u8]) {
        self.total_raw_bytes += data.len();
        self.pending.extend_from_slice(data);
        // Decode the longest valid UTF-8 prefix; keep an incomplete trailing sequence pending.
        let valid = match std::str::from_utf8(&self.pending) {
            Ok(_) => self.pending.len(),
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            Err(_) => self.pending.len(),
        };
        let chunk: Vec<u8> = self.pending.drain(..valid).collect();
        let text = String::from_utf8_lossy(&chunk).into_owned();
        self.append_decoded(&text);
        if self.temp_file.is_some() || self.should_use_temp_file() {
            self.ensure_temp_file();
            if let Some(f) = &mut self.temp_file {
                let _ = f.write_all(data);
            }
        } else {
            self.raw_chunks.extend_from_slice(data);
        }
    }

    pub fn finish(&mut self) {
        if !self.pending.is_empty() {
            let rest = std::mem::take(&mut self.pending);
            let text = String::from_utf8_lossy(&rest).into_owned();
            self.append_decoded(&text);
        }
        if self.should_use_temp_file() {
            self.ensure_temp_file();
        }
        if let Some(f) = &mut self.temp_file {
            let _ = f.flush();
        }
    }

    pub fn last_line_bytes(&self) -> usize {
        self.current_line_bytes
    }

    pub fn snapshot(&mut self, persist_if_truncated: bool) -> OutputSnapshot {
        let text = self.snapshot_text();
        let mut t = truncate_tail(
            &text,
            Limits {
                max_lines: self.max_lines,
                max_bytes: self.max_bytes,
            },
        );
        let truncated = self.total_lines > self.max_lines || self.total_decoded_bytes > self.max_bytes;
        t.truncated_by = if truncated {
            t.truncated_by.or(Some(if self.total_decoded_bytes > self.max_bytes {
                TruncatedBy::Bytes
            } else {
                TruncatedBy::Lines
            }))
        } else {
            None
        };
        t.truncated = truncated;
        t.total_lines = self.total_lines;
        t.total_bytes = self.total_decoded_bytes;
        if persist_if_truncated && truncated {
            self.ensure_temp_file();
        }
        OutputSnapshot {
            content: t.content.clone(),
            truncation: t,
            full_output_path: self.temp_path.clone(),
        }
    }

    /// Full output, keeping head and tail halves around an omission marker when longer than `max_bytes`.
    pub fn read_full_output(&mut self, max_bytes: usize) -> (String, bool) {
        let Some(path) = &self.temp_path else {
            return (String::from_utf8_lossy(&self.raw_chunks).into_owned(), false);
        };
        let Ok(mut f) = File::open(path) else { return (String::new(), false) };
        let size = f.metadata().map(|m| m.len() as usize).unwrap_or(0);
        if size <= max_bytes {
            let mut buf = Vec::new();
            let _ = f.read_to_end(&mut buf);
            return (String::from_utf8_lossy(&buf).into_owned(), false);
        }
        let head_n = max_bytes / 2;
        let tail_n = max_bytes - head_n;
        let mut head = vec![0; head_n];
        let mut tail = vec![0; tail_n];
        let _ = f.read_exact(&mut head);
        let _ = f.seek(SeekFrom::Start((size - tail_n) as u64));
        let _ = f.read_exact(&mut tail);
        let start = tail.iter().position(|b| b & 0xc0 != 0x80).unwrap_or(tail.len());
        let omitted = size - head_n - tail_n;
        (
            format!(
                "{}\n\n[... {omitted} bytes omitted ...]\n\n{}",
                String::from_utf8_lossy(&head),
                String::from_utf8_lossy(&tail[start..])
            ),
            true,
        )
    }

    fn append_decoded(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.total_decoded_bytes += text.len();
        self.tail.push_str(text);
        if self.tail.len() > self.max_rolling_bytes * 2 {
            self.trim_tail();
        }
        let newlines = text.matches('\n').count();
        if newlines == 0 {
            self.current_line_bytes += text.len();
            self.has_open_line = true;
        } else {
            self.completed_lines += newlines;
            let after = &text[text.rfind('\n').unwrap() + 1..];
            self.current_line_bytes = after.len();
            self.has_open_line = !after.is_empty();
        }
        self.total_lines = self.completed_lines + usize::from(self.has_open_line);
    }

    fn trim_tail(&mut self) {
        if self.tail.len() <= self.max_rolling_bytes {
            return;
        }
        let mut start = self.tail.len() - self.max_rolling_bytes;
        while !self.tail.is_char_boundary(start) {
            start += 1;
        }
        self.tail_starts_at_line_boundary = self.tail.as_bytes()[start - 1] == b'\n';
        self.tail = self.tail[start..].to_string();
    }

    fn snapshot_text(&self) -> String {
        if self.tail_starts_at_line_boundary {
            return self.tail.clone();
        }
        match self.tail.find('\n') {
            Some(i) => self.tail[i + 1..].to_string(),
            None => self.tail.clone(),
        }
    }

    fn should_use_temp_file(&self) -> bool {
        self.total_raw_bytes > self.max_bytes || self.total_decoded_bytes > self.max_bytes || self.total_lines > self.max_lines
    }

    fn ensure_temp_file(&mut self) {
        if self.temp_path.is_some() {
            return;
        }
        let path = std::env::temp_dir().join(format!("{}-{}.log", self.temp_prefix, uuid::Uuid::new_v4().simple()));
        if let Ok(mut f) = File::create(&path) {
            let _ = f.write_all(&self.raw_chunks);
            self.raw_chunks.clear();
            self.temp_file = Some(f);
            self.temp_path = Some(path);
        }
    }
}
