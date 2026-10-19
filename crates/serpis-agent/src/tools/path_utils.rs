//! Path resolution shared by the file tools (pi `path-utils.ts` + `utils/paths.ts`).

use std::path::{Component, Path, PathBuf};

use unicode_normalization::UnicodeNormalization;

const NARROW_NO_BREAK_SPACE: char = '\u{202F}';

fn is_unicode_space(c: char) -> bool {
    matches!(c, '\u{00A0}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}')
}

/// Normalize user/model path input: unicode spaces → ' ', strip a leading `@`, expand `~`, `file://`.
pub fn expand_path(input: &str) -> String {
    let mut s: String = input.chars().map(|c| if is_unicode_space(c) { ' ' } else { c }).collect();
    if let Some(rest) = s.strip_prefix('@') {
        s = rest.to_string();
    }
    if s == "~" {
        return dirs::home_dir().unwrap_or_default().to_string_lossy().into_owned();
    }
    if let Some(rest) = s.strip_prefix("~/") {
        return dirs::home_dir().unwrap_or_default().join(rest).to_string_lossy().into_owned();
    }
    if s.starts_with("file://") {
        if let Ok(u) = url::Url::parse(&s) {
            if let Ok(p) = u.to_file_path() {
                return p.to_string_lossy().into_owned();
            }
        }
    }
    s
}

/// Lexically normalize `.` and `..` like Node's `path.resolve`.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Resolve a path relative to `cwd`, handling `~` and absolute paths.
pub fn resolve_to_cwd(path: &str, cwd: &Path) -> PathBuf {
    let expanded = expand_path(path);
    let p = Path::new(&expanded);
    if p.is_absolute() { normalize(p) } else { normalize(&cwd.join(p)) }
}

fn exists(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok()
}

/// Resolve a path for reading, trying macOS filename variants (screenshot AM/PM spaces,
/// NFD normalization, curly apostrophes) when the literal path does not exist.
pub fn resolve_read_path(path: &str, cwd: &Path) -> PathBuf {
    let resolved = resolve_to_cwd(path, cwd);
    if exists(&resolved) {
        return resolved;
    }
    let s = resolved.to_string_lossy().into_owned();
    let am_pm = regex::Regex::new(r"(?i) (AM|PM)\.")
        .unwrap()
        .replace_all(&s, |c: &regex::Captures| format!("{NARROW_NO_BREAK_SPACE}{}.", &c[1]))
        .into_owned();
    let nfd: String = s.nfd().collect();
    let curly = s.replace('\'', "\u{2019}");
    let nfd_curly = nfd.replace('\'', "\u{2019}");
    for candidate in [am_pm, nfd, curly, nfd_curly] {
        if candidate != s && exists(Path::new(&candidate)) {
            return PathBuf::from(candidate);
        }
    }
    resolved
}

/// Display a path relative to `cwd` when it is inside it.
pub fn display_path(path: &Path, cwd: &Path) -> String {
    match path.strip_prefix(cwd) {
        Ok(rel) if rel.as_os_str().is_empty() => ".".into(),
        Ok(rel) => rel.to_string_lossy().into_owned(),
        Err(_) => path.to_string_lossy().into_owned(),
    }
}
