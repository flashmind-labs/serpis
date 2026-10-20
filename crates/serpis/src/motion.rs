//! Motions, text objects and search over any [`TextSurface`]. Shared by the regular, emacs
//! and vim schemes, and by navigation packages.

use crate::text::{Pos, TextSurface, char_len};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharClass {
    Space,
    Word,
    Punct,
}

pub fn class(c: char, big: bool) -> CharClass {
    if c.is_whitespace() {
        CharClass::Space
    } else if big || c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else {
        CharClass::Punct
    }
}

fn chars(s: &dyn TextSurface, line: usize) -> Vec<char> {
    s.line(line).chars().collect()
}

/// Step one character forward across line ends (`None` at end of text).
pub fn next_pos(s: &dyn TextSurface, p: Pos) -> Option<Pos> {
    if p.col < s.line_len(p.line) {
        Some(Pos::new(p.line, p.col + 1))
    } else if p.line + 1 < s.line_count() {
        Some(Pos::new(p.line + 1, 0))
    } else {
        None
    }
}

pub fn prev_pos(s: &dyn TextSurface, p: Pos) -> Option<Pos> {
    if p.col > 0 {
        Some(Pos::new(p.line, p.col - 1))
    } else if p.line > 0 {
        Some(Pos::new(p.line - 1, s.line_len(p.line - 1)))
    } else {
        None
    }
}

/// Character at `p` with line ends as `'\n'`.
fn at(s: &dyn TextSurface, p: Pos) -> char {
    s.line(p.line).chars().nth(p.col).unwrap_or('\n')
}

/// vim `w` / `W`.
pub fn word_forward(s: &dyn TextSurface, p: Pos, big: bool) -> Pos {
    let start_class = class(at(s, p), big);
    let mut q = p;
    // Skip the rest of the current word.
    if start_class != CharClass::Space {
        while let Some(n) = next_pos(s, q) {
            q = n;
            let c = at(s, q);
            if c == '\n' || class(c, big) != start_class {
                break;
            }
        }
    }
    // Skip whitespace (an empty line counts as a word).
    loop {
        let c = at(s, q);
        if c == '\n' && q.col == 0 && s.line_len(q.line) == 0 && q != p {
            return q;
        }
        if !c.is_whitespace() {
            return q;
        }
        match next_pos(s, q) {
            Some(n) => q = n,
            None => return s.clamp(Pos::new(q.line, s.line_len(q.line))),
        }
    }
}

/// vim `b` / `B`.
pub fn word_backward(s: &dyn TextSurface, p: Pos, big: bool) -> Pos {
    let mut q = match prev_pos(s, p) {
        Some(q) => q,
        None => return p,
    };
    while at(s, q).is_whitespace() {
        if q.col == 0 && s.line_len(q.line) == 0 {
            return q;
        }
        match prev_pos(s, q) {
            Some(n) => q = n,
            None => return q,
        }
    }
    let cl = class(at(s, q), big);
    while let Some(n) = prev_pos(s, q) {
        let c = at(s, n);
        if c == '\n' || class(c, big) != cl {
            break;
        }
        q = n;
    }
    q
}

/// vim `e` / `E`.
pub fn word_end(s: &dyn TextSurface, p: Pos, big: bool) -> Pos {
    let mut q = match next_pos(s, p) {
        Some(q) => q,
        None => return p,
    };
    while at(s, q).is_whitespace() {
        match next_pos(s, q) {
            Some(n) => q = n,
            None => return q,
        }
    }
    let cl = class(at(s, q), big);
    while let Some(n) = next_pos(s, q) {
        let c = at(s, n);
        if c == '\n' || class(c, big) != cl {
            break;
        }
        q = n;
    }
    q
}

/// Emacs `forward-word`: end of the next word.
pub fn emacs_forward_word(s: &dyn TextSurface, p: Pos) -> Pos {
    let mut q = p;
    while class(at(s, q), false) != CharClass::Word {
        match next_pos(s, q) {
            Some(n) => q = n,
            None => return q,
        }
    }
    while class(at(s, q), false) == CharClass::Word {
        match next_pos(s, q) {
            Some(n) => q = n,
            None => return s.clamp(Pos::new(q.line, q.col + 1)),
        }
    }
    q
}

pub fn emacs_backward_word(s: &dyn TextSurface, p: Pos) -> Pos {
    let mut q = p;
    loop {
        match prev_pos(s, q) {
            Some(n) if class(at(s, n), false) != CharClass::Word => q = n,
            Some(_) => break,
            None => return q,
        }
    }
    while let Some(n) = prev_pos(s, q) {
        if class(at(s, n), false) != CharClass::Word {
            break;
        }
        q = n;
    }
    q
}

pub fn line_first_nonblank(s: &dyn TextSurface, line: usize) -> Pos {
    let start = s.text_start(line);
    let col = start + chars(s, line).iter().skip(start).take_while(|c| c.is_whitespace()).count();
    Pos::new(line, col)
}

/// Vertical move keeping a goal column.
pub fn vertical(s: &dyn TextSurface, p: Pos, delta: isize, goal: usize) -> Pos {
    let n = s.line_count() as isize;
    let line = (p.line as isize + delta).clamp(0, (n - 1).max(0)) as usize;
    Pos::new(line, goal.min(s.line_len(line)))
}

/// vim `f`/`t`/`F`/`T` within the line.
pub fn find_in_line(s: &dyn TextSurface, p: Pos, ch: char, forward: bool, till: bool, count: usize) -> Option<Pos> {
    let cs = chars(s, p.line);
    let mut found = 0;
    if forward {
        let mut i = p.col + 1 + usize::from(till);
        while i < cs.len() {
            if cs[i] == ch {
                found += 1;
                if found == count {
                    return Some(Pos::new(p.line, if till { i - 1 } else { i }));
                }
            }
            i += 1;
        }
    } else {
        let mut i = p.col as isize - 1 - isize::from(till);
        while i >= 0 {
            if cs[i as usize] == ch {
                found += 1;
                if found == count {
                    let col = i as usize;
                    return Some(Pos::new(p.line, if till { col + 1 } else { col }));
                }
            }
            i -= 1;
        }
    }
    None
}

/// vim `}` / `{`: next/previous blank line.
pub fn paragraph(s: &dyn TextSurface, p: Pos, forward: bool) -> Pos {
    let n = s.line_count();
    let blank = |l: usize| s.line(l).trim().is_empty();
    let mut l = p.line;
    if forward {
        while l + 1 < n && blank(l) {
            l += 1;
        }
        while l + 1 < n && !blank(l) {
            l += 1;
        }
        Pos::new(l, if blank(l) { 0 } else { s.line_len(l) })
    } else {
        while l > 0 && blank(l) {
            l -= 1;
        }
        while l > 0 && !blank(l) {
            l -= 1;
        }
        Pos::new(l, 0)
    }
}

const PAIRS: [(char, char); 4] = [('(', ')'), ('[', ']'), ('{', '}'), ('<', '>')];

/// vim `%`.
pub fn matching_bracket(s: &dyn TextSurface, p: Pos) -> Option<Pos> {
    let cs = chars(s, p.line);
    let (idx, c) = cs.iter().enumerate().skip(p.col).find(|(_, c)| PAIRS.iter().any(|(o, cl)| *c == o || *c == cl))?;
    let start = Pos::new(p.line, idx);
    let (open, close, fwd) = PAIRS
        .iter()
        .find_map(|(o, cl)| if c == o { Some((*o, *cl, true)) } else if c == cl { Some((*o, *cl, false)) } else { None })?;
    let mut depth = 0i32;
    let mut q = start;
    loop {
        let ch = at(s, q);
        if ch == open {
            depth += if fwd { 1 } else { -1 };
        } else if ch == close {
            depth += if fwd { -1 } else { 1 };
        }
        if depth == 0 {
            return Some(q);
        }
        q = if fwd { next_pos(s, q)? } else { prev_pos(s, q)? };
    }
}

/// Text objects: `iw aw iW aW i" a" i' a' i( a( i[ a[ i{ a{ ip ap`. Returns `[start, end)`.
pub fn text_object(s: &dyn TextSurface, p: Pos, inner: bool, kind: char) -> Option<(Pos, Pos)> {
    match kind {
        'w' | 'W' => {
            let big = kind == 'W';
            let cs = chars(s, p.line);
            if cs.is_empty() {
                return None;
            }
            let col = p.col.min(cs.len() - 1);
            let cl = class(cs[col], big);
            let mut a = col;
            while a > 0 && class(cs[a - 1], big) == cl {
                a -= 1;
            }
            let mut b = col + 1;
            while b < cs.len() && class(cs[b], big) == cl {
                b += 1;
            }
            if !inner {
                let mut e = b;
                while e < cs.len() && cs[e].is_whitespace() {
                    e += 1;
                }
                if e > b {
                    b = e;
                } else {
                    while a > 0 && cs[a - 1].is_whitespace() {
                        a -= 1;
                    }
                }
            }
            Some((Pos::new(p.line, a), Pos::new(p.line, b)))
        }
        '"' | '\'' | '`' => {
            let cs = chars(s, p.line);
            let qs: Vec<usize> = cs.iter().enumerate().filter(|(_, c)| **c == kind).map(|(i, _)| i).collect();
            let pair = qs.chunks(2).find(|w| w.len() == 2 && w[0] <= p.col && p.col <= w[1]).or_else(|| qs.chunks(2).find(|w| w.len() == 2 && w[0] > p.col))?;
            let (a, b) = (pair[0], pair[1]);
            Some(if inner { (Pos::new(p.line, a + 1), Pos::new(p.line, b)) } else { (Pos::new(p.line, a), Pos::new(p.line, b + 1)) })
        }
        '(' | ')' | 'b' | '[' | ']' | '{' | '}' | 'B' | '<' | '>' => {
            let (open, close) = match kind {
                '(' | ')' | 'b' => ('(', ')'),
                '[' | ']' => ('[', ']'),
                '<' | '>' => ('<', '>'),
                _ => ('{', '}'),
            };
            // Search backward for an unmatched opener.
            let mut depth = 0;
            let mut q = p;
            let start = loop {
                let c = at(s, q);
                if c == close && q != p {
                    depth += 1;
                } else if c == open {
                    if depth == 0 {
                        break q;
                    }
                    depth -= 1;
                }
                q = prev_pos(s, q)?;
            };
            let end = matching_bracket(s, start)?;
            Some(if inner { (next_pos(s, start)?, end) } else { (start, next_pos(s, end).unwrap_or(Pos::new(end.line, end.col + 1))) })
        }
        'p' => {
            let n = s.line_count();
            let blank = |l: usize| s.line(l).trim().is_empty();
            let kind_blank = blank(p.line);
            let mut a = p.line;
            while a > 0 && blank(a - 1) == kind_blank {
                a -= 1;
            }
            let mut b = p.line;
            while b + 1 < n && blank(b + 1) == kind_blank {
                b += 1;
            }
            if !inner {
                while b + 1 < n && blank(b + 1) {
                    b += 1;
                }
            }
            let end = if b + 1 < n { Pos::new(b + 1, 0) } else { Pos::new(b, s.line_len(b)) };
            Some((Pos::new(a, 0), end))
        }
        _ => None,
    }
}

/// Regex search from `from`, wrapping around. Returns the match start and end.
pub fn search(s: &dyn TextSurface, from: Pos, pattern: &str, forward: bool) -> Option<(Pos, Pos)> {
    let re = regex::RegexBuilder::new(pattern)
        .case_insensitive(!pattern.chars().any(char::is_uppercase))
        .build()
        .or_else(|_| regex::Regex::new(&regex::escape(pattern)))
        .ok()?;
    let n = s.line_count();
    let to_pos = |line: usize, text: &str, b: usize| Pos::new(line, char_len(&text[..b]));
    let matches_in = |line: usize| -> Vec<(Pos, Pos)> {
        let t = s.line(line);
        re.find_iter(&t).map(|m| (to_pos(line, &t, m.start()), to_pos(line, &t, m.end()))).collect()
    };
    if forward {
        for k in 0..=n {
            let line = (from.line + k) % n.max(1);
            for m in matches_in(line) {
                if k == 0 && m.0 <= from {
                    continue;
                }
                return Some(m);
            }
            if k == n {
                break;
            }
        }
    } else {
        for k in 0..=n {
            let line = (from.line + n * 2 - k) % n.max(1);
            for m in matches_in(line).into_iter().rev() {
                if k == 0 && m.0 >= from {
                    continue;
                }
                return Some(m);
            }
        }
    }
    None
}

pub fn line_end(s: &dyn TextSurface, line: usize) -> Pos {
    Pos::new(line, s.line_len(line))
}

pub fn doc_end(s: &dyn TextSurface) -> Pos {
    let l = s.line_count().saturating_sub(1);
    line_end(s, l)
}

pub fn is_blank_line(s: &dyn TextSurface, l: usize) -> bool {
    char_len(&s.line(l)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::TextArea;

    #[test]
    fn words() {
        let t = TextArea::new("foo.bar baz\n\nqux");
        assert_eq!(word_forward(&t, Pos::new(0, 0), false), Pos::new(0, 3));
        assert_eq!(word_forward(&t, Pos::new(0, 0), true), Pos::new(0, 8));
        assert_eq!(word_forward(&t, Pos::new(0, 8), false), Pos::new(1, 0));
        assert_eq!(word_backward(&t, Pos::new(0, 8), false), Pos::new(0, 4));
        assert_eq!(word_end(&t, Pos::new(0, 0), false), Pos::new(0, 2));
        assert_eq!(text_object(&t, Pos::new(0, 1), true, 'w'), Some((Pos::new(0, 0), Pos::new(0, 3))));
    }

    #[test]
    fn brackets_and_search() {
        let t = TextArea::new("f(a, (b))\nnext f");
        assert_eq!(matching_bracket(&t, Pos::new(0, 1)), Some(Pos::new(0, 8)));
        assert_eq!(text_object(&t, Pos::new(0, 6), true, '('), Some((Pos::new(0, 6), Pos::new(0, 7))));
        assert_eq!(search(&t, Pos::new(0, 0), "f", true).map(|m| m.0), Some(Pos::new(1, 5)));
    }
}
