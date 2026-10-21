//! Syntax highlighting for code shown in chats (read/write/edit tool output, fenced code blocks):
//! syntect parses with bat's grammars (two-face), and token scopes map to a few faces the theme
//! colors from its palette, so highlighting follows every theme without settings of its own.
//! Results are cached by content: transcripts rebuild often while a reply streams.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::{Mutex, OnceLock};

use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};

use crate::theme::Face;

/// Faces of one line: byte ranges into it.
pub type Spans = Vec<(Range<usize>, Face)>;

/// Code larger than this is shown plain (parsing it would stall the UI).
const MAX_BYTES: usize = 512 * 1024;
const CACHE_ENTRIES: usize = 256;

static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
static CACHE: OnceLock<Mutex<HashMap<u64, Vec<Spans>>>> = OnceLock::new();

fn syntaxes() -> &'static SyntaxSet {
    SYNTAXES.get_or_init(two_face::syntax::extra_newlines)
}

/// Load the grammars in the background, so the first highlight doesn't wait for them.
pub fn preload() {
    std::thread::spawn(|| {
        syntaxes();
    });
}

/// Scope prefixes, most specific first, and the face they get.
fn classes() -> &'static [(Scope, Face)] {
    static CLASSES: OnceLock<Vec<(Scope, Face)>> = OnceLock::new();
    CLASSES.get_or_init(|| {
        [
            ("comment", Face::SynComment),
            ("punctuation.definition.comment", Face::SynComment),
            ("string", Face::SynString),
            ("constant.character.escape", Face::SynConstant),
            ("constant", Face::SynConstant),
            ("entity.name.function", Face::SynFunction),
            ("support.function", Face::SynFunction),
            ("variable.function", Face::SynFunction),
            ("meta.function-call", Face::SynFunction),
            ("entity.name.type", Face::SynType),
            ("entity.name.class", Face::SynType),
            ("entity.name.struct", Face::SynType),
            ("entity.name.enum", Face::SynType),
            ("entity.name.trait", Face::SynType),
            ("entity.other.inherited-class", Face::SynType),
            ("support.type", Face::SynType),
            ("support.class", Face::SynType),
            ("storage.type", Face::SynKeyword),
            ("storage.modifier", Face::SynKeyword),
            ("keyword.operator", Face::Code),
            ("keyword", Face::SynKeyword),
            ("variable.language", Face::SynKeyword),
            ("entity.name.tag", Face::SynKeyword),
            ("entity.other.attribute-name", Face::SynAttribute),
            ("meta.annotation", Face::SynAttribute),
            ("support.macro", Face::SynAttribute),
            ("entity.name.macro", Face::SynAttribute),
            ("markup.heading", Face::SynKeyword),
            ("markup.bold", Face::SynType),
            ("markup.inline.raw", Face::SynString),
            ("markup.raw", Face::SynString),
        ]
        .into_iter()
        .filter_map(|(s, f)| Scope::new(s).ok().map(|s| (s, f)))
        .collect()
    })
}

/// The face for text under `stack`: the innermost scope with a class wins.
fn face_of(stack: &[Scope], base: Face) -> Face {
    for s in stack.iter().rev() {
        if let Some((_, f)) = classes().iter().find(|(p, _)| p.is_prefix_of(*s)) {
            return *f;
        }
    }
    base
}

/// The grammar for a fenced code block's info string ("rust", "py", "sh") or a file name
/// ("src/main.rs", "Makefile"), else from the first line ("#!/bin/sh").
pub fn find(hint: &str, first_line: &str) -> Option<&'static SyntaxReference> {
    let ss = syntaxes();
    let hint = hint.trim();
    let by_name = || {
        let path = std::path::Path::new(hint);
        let file = path.file_name().and_then(|f| f.to_str()).unwrap_or(hint);
        let ext = path.extension().and_then(|e| e.to_str());
        ss.find_syntax_by_extension(file).or_else(|| ext.and_then(|e| ss.find_syntax_by_extension(e)))
    };
    let token = hint.split([' ', ',', '{']).next().unwrap_or("");
    let found = if token.is_empty() { None } else { ss.find_syntax_by_token(token) };
    found
        .or_else(by_name)
        .or_else(|| ss.find_syntax_by_first_line(first_line))
        .filter(|s| s.name != "Plain Text")
}

/// Faces for each of `lines` (code in the language `hint` names), or None when the language is
/// unknown or the code too large. Text outside any class keeps `base`.
pub fn highlight(hint: &str, lines: &[&str], base: Face) -> Option<Vec<Spans>> {
    if lines.iter().map(|l| l.len() + 1).sum::<usize>() > MAX_BYTES {
        return None;
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (hint, lines, base as u8).hash(&mut h);
    let key = h.finish();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(v) = cache.lock().unwrap().get(&key) {
        return Some(v.clone());
    }
    let syntax = find(hint, lines.first().copied().unwrap_or(""))?;
    let ss = syntaxes();
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut out = Vec::with_capacity(lines.len());
    let mut buf = String::new();
    for line in lines {
        buf.clear();
        buf.push_str(line);
        buf.push('\n');
        let Ok(ops) = state.parse_line(&buf, ss) else { return None };
        let mut spans: Spans = Vec::new();
        let mut at = 0;
        let push = |r: Range<usize>, f: Face, spans: &mut Spans| {
            let r = r.start.min(line.len())..r.end.min(line.len());
            if r.is_empty() || f == base {
                return;
            }
            match spans.last_mut() {
                Some((last, lf)) if *lf == f && last.end == r.start => last.end = r.end,
                _ => spans.push((r, f)),
            }
        };
        for (pos, op) in ops {
            if pos > at {
                push(at..pos, face_of(stack.as_slice(), base), &mut spans);
                at = pos;
            }
            let _ = stack.apply(&op);
        }
        push(at..buf.len(), face_of(stack.as_slice(), base), &mut spans);
        out.push(spans);
    }
    let mut c = cache.lock().unwrap();
    if c.len() >= CACHE_ENTRIES {
        c.clear();
    }
    c.insert(key, out.clone());
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn faces(hint: &str, code: &str) -> Vec<(String, Face)> {
        let lines: Vec<&str> = code.lines().collect();
        let spans = highlight(hint, &lines, Face::Code).expect("highlighted");
        spans.iter().zip(&lines).flat_map(|(s, l)| s.iter().map(|(r, f)| (l[r.clone()].to_string(), *f))).collect()
    }

    #[test]
    fn rust_gets_keywords_strings_and_comments() {
        let f = faces("rust", "// hi\nfn main() {\n    let s = \"x\";\n}");
        assert!(f.contains(&("// hi".into(), Face::SynComment)), "{f:?}");
        assert!(f.iter().any(|(t, f)| t == "fn" && *f == Face::SynKeyword), "{f:?}");
        assert!(f.iter().any(|(t, f)| t == "main" && *f == Face::SynFunction), "{f:?}");
        assert!(f.iter().any(|(t, f)| t.contains("\"x\"") && *f == Face::SynString), "{f:?}");
    }

    #[test]
    fn languages_from_paths_tokens_and_first_lines() {
        assert_eq!(find("src/main.rs", "").map(|s| s.name.as_str()), Some("Rust"));
        assert_eq!(find("py", "").map(|s| s.name.as_str()), Some("Python"));
        assert_eq!(find("Makefile", "").map(|s| s.name.as_str()), Some("Makefile"));
        assert!(find("", "#!/bin/bash").is_some());
        assert!(find("notes.unknownext", "hello").is_none());
        // Multi-line state carries over: the second line is still inside the comment.
        let f = faces("c", "/* a\n b */ int x;");
        assert!(f.contains(&(" b */".into(), Face::SynComment)), "{f:?}");
    }
}
