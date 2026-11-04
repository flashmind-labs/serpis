//! A shell command as the steps it runs in order, so a chain (`cd x && make; echo done`) can be
//! read a step at a time. Only for showing: steps are byte ranges of the command as written, and
//! what it runs never changes. Pipelines, loops, subshells, quotes and heredocs stay whole, and
//! anything not understood is no split at all (`None`): it then shows as written.

use std::ops::Range;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// The operator joining it to the step before: `&&`, `||`, `;`, `&`. None: the first step,
    /// or one on a line of its own.
    pub op: Option<&'static str>,
    pub range: Range<usize>,
}

/// What is open at a point in the command; steps only split with nothing open.
#[derive(PartialEq)]
enum Open {
    Paren,
    Brace,
    Param,
    DoubleQuote,
    DoubleBracket,
    /// A compound command, by the word that closes it (`fi`, `done`, `esac`).
    Compound(&'static [u8]),
}

/// The steps of `cmd`, when it has at least two and all of it was understood.
pub fn split(cmd: &str) -> Option<Vec<Step>> {
    let b = cmd.as_bytes();
    let at = |i: usize| b.get(i).copied().unwrap_or(0);
    let mut stack: Vec<Open> = Vec::new();
    let mut heredocs: Vec<(Vec<u8>, bool)> = Vec::new();
    let mut steps: Vec<Step> = Vec::new();
    let (mut start, mut op, mut at_command, mut i) = (0, None, true, 0);
    while i < b.len() {
        let c = b[i];
        match (c, at(i + 1)) {
            (b'\\', next) => {
                at_command &= next == b'\n';
                i += 2;
                continue;
            }
            (b'`', _) => {
                i = closing(b, i + 1, b'`', true)? + 1;
                at_command = false;
                continue;
            }
            (b'$', b'(') => {
                stack.push(Open::Paren);
                at_command = true;
                i += 2;
                continue;
            }
            (b'$', b'{') => {
                stack.push(Open::Param);
                i += 2;
                continue;
            }
            (b'$', b'\'') if stack.last() != Some(&Open::DoubleQuote) => {
                i = closing(b, i + 2, b'\'', true)? + 1;
                at_command = false;
                continue;
            }
            _ => {}
        }
        match stack.last() {
            Some(Open::DoubleQuote) => {
                if c == b'"' {
                    stack.pop();
                }
                i += 1;
                continue;
            }
            Some(Open::Param) => {
                i = match c {
                    b'}' => {
                        stack.pop();
                        i + 1
                    }
                    b'\'' => closing(b, i + 1, b'\'', false)? + 1,
                    b'"' => {
                        stack.push(Open::DoubleQuote);
                        i + 1
                    }
                    _ => i + 1,
                };
                continue;
            }
            _ => {}
        }
        // Ends the step here and starts the next after the operator.
        let mut operator = |name: &'static str, len: usize, i: &mut usize, op: &mut Option<&'static str>, start: &mut usize| -> Option<()> {
            if stack.is_empty() {
                if !heredocs.is_empty() {
                    return None; // The heredoc's text would land inside the next step.
                }
                steps.push(Step { op: *op, range: nonempty(trimmed(b, *start..*i))? });
                *op = Some(name);
                *start = *i + len;
            }
            *i += len;
            Some(())
        };
        match c {
            b'\'' => {
                i = closing(b, i + 1, b'\'', false)? + 1;
                at_command = false;
            }
            b'"' => {
                stack.push(Open::DoubleQuote);
                at_command = false;
                i += 1;
            }
            b'#' if i == 0 || matches!(b[i - 1], b' ' | b'\t' | b'\n' | b';' | b'&' | b'|' | b'(' | b')') => {
                i = b[i..].iter().position(|&x| x == b'\n').map_or(b.len(), |n| i + n);
            }
            b'\n' => {
                i += 1;
                let bodies = !heredocs.is_empty();
                for (delimiter, strip_tabs) in heredocs.drain(..) {
                    i = past_heredoc(b, i, &delimiter, strip_tabs)?;
                }
                if stack.is_empty() && (!at_command || bodies) {
                    steps.push(Step { op, range: nonempty(trimmed(b, start..i))? });
                    op = None;
                    start = i;
                }
                at_command = true;
            }
            b'(' if at(i + 1) == b')' => {
                i += 2; // A function's definition: its body follows.
                at_command = true;
            }
            b'(' => {
                if !matches!(stack.last(), Some(Open::Compound(b"esac"))) {
                    stack.push(Open::Paren);
                    at_command = true;
                }
                i += 1;
            }
            b')' => {
                match stack.last() {
                    Some(Open::Paren) => {
                        stack.pop();
                        at_command = false;
                    }
                    Some(Open::Compound(b"esac")) => at_command = true,
                    _ => return None,
                }
                i += 1;
            }
            b';' => {
                let len = if matches!(at(i + 1), b';' | b'&') { 2 } else { 1 };
                if len == 2 && stack.is_empty() {
                    return None;
                }
                operator(";", len, &mut i, &mut op, &mut start)?;
                at_command = true;
            }
            b'&' if at(i + 1) == b'&' => {
                operator("&&", 2, &mut i, &mut op, &mut start)?;
                at_command = true;
            }
            // Redirections, not a command put in the background: 2>&1, <&3, &>file.
            b'&' if i > 0 && matches!(b[i - 1], b'>' | b'<') || at(i + 1) == b'>' => i += 1,
            b'&' => {
                operator("&", 1, &mut i, &mut op, &mut start)?;
                at_command = true;
            }
            b'|' if at(i + 1) == b'|' => {
                operator("||", 2, &mut i, &mut op, &mut start)?;
                at_command = true;
            }
            b'|' => {
                // A pipeline is one step. (`>|` is a redirection: what follows is a file.)
                at_command = i == 0 || b[i - 1] != b'>';
                i += if at(i + 1) == b'&' { 2 } else { 1 };
            }
            b'<' if at(i + 1) == b'<' && at(i + 2) == b'<' => i += 3,
            b'<' if at(i + 1) == b'<' => {
                let strip_tabs = at(i + 2) == b'-';
                let (delimiter, next) = heredoc_delimiter(b, i + 2 + usize::from(strip_tabs))?;
                heredocs.push((delimiter, strip_tabs));
                i = next;
            }
            b'<' | b'>' | b' ' | b'\t' | b'\r' => i += 1,
            _ => {
                let end = (i..b.len()).find(|&j| is_metachar(b[j])).unwrap_or(b.len()).max(i + 1);
                let word = &b[i..end];
                let bracketed = stack.last() == Some(&Open::DoubleBracket);
                if word == b"]]" && bracketed {
                    stack.pop();
                    at_command = false;
                } else if at_command && !bracketed {
                    at_command = match word {
                        b"if" => open(&mut stack, Open::Compound(b"fi"), true),
                        b"while" | b"until" => open(&mut stack, Open::Compound(b"done"), true),
                        b"for" | b"select" => open(&mut stack, Open::Compound(b"done"), false),
                        b"case" => open(&mut stack, Open::Compound(b"esac"), false),
                        b"{" => open(&mut stack, Open::Brace, true),
                        b"[[" => open(&mut stack, Open::DoubleBracket, false),
                        b"then" | b"do" | b"else" | b"elif" | b"!" | b"time" => true,
                        b"function" => return None,
                        b"fi" | b"done" | b"esac" => {
                            match stack.pop() {
                                Some(Open::Compound(w)) if w == word => {}
                                _ => return None,
                            }
                            false
                        }
                        b"}" => {
                            if stack.pop() != Some(Open::Brace) {
                                return None;
                            }
                            false
                        }
                        _ => false,
                    };
                }
                i = end;
            }
        }
    }
    if !stack.is_empty() || !heredocs.is_empty() {
        return None;
    }
    let last = trimmed(b, start.min(b.len())..b.len());
    if !last.is_empty() {
        steps.push(Step { op, range: last });
    } else if matches!(op, Some(";" | "&")) {
        // Ends in an operator: it stays with the last step (`sleep 5 &` runs in the background).
        steps.last_mut()?.range.end = start;
    } else if op.is_some() {
        return None;
    }
    (steps.len() > 1).then_some(steps)
}

fn open(stack: &mut Vec<Open>, what: Open, at_command: bool) -> bool {
    stack.push(what);
    at_command
}

fn is_metachar(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\r' | b'\n' | b';' | b'&' | b'|' | b'(' | b')' | b'<' | b'>' | b'`' | b'"' | b'\'' | b'$' | b'\\')
}

fn nonempty(r: Range<usize>) -> Option<Range<usize>> {
    (!r.is_empty()).then_some(r)
}

/// `r` without the space around it, and without line continuations at its ends.
fn trimmed(b: &[u8], mut r: Range<usize>) -> Range<usize> {
    r.end = r.end.min(b.len());
    loop {
        while r.start < r.end && b[r.start].is_ascii_whitespace() {
            r.start += 1;
        }
        if r.start + 1 < r.end && b[r.start] == b'\\' && b[r.start + 1] == b'\n' {
            r.start += 2;
        } else {
            break;
        }
    }
    loop {
        let before = r.end;
        while r.start < r.end && b[r.end - 1].is_ascii_whitespace() {
            r.end -= 1;
        }
        if r.start < r.end && r.end < before && b[r.end - 1] == b'\\' && b[r.end] == b'\n' {
            r.end -= 1;
        } else {
            break;
        }
    }
    r
}

/// Where `close` is, from `i` on (`escapes`: a backslash hides the next byte).
fn closing(b: &[u8], mut i: usize, close: u8, escapes: bool) -> Option<usize> {
    while i < b.len() {
        match b[i] {
            b'\\' if escapes => i += 1,
            c if c == close => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

/// A heredoc's closing word (without its quotes), from `i` on, and where it ends.
fn heredoc_delimiter(b: &[u8], mut i: usize) -> Option<(Vec<u8>, usize)> {
    while matches!(b.get(i), Some(b' ' | b'\t')) {
        i += 1;
    }
    let mut word = Vec::new();
    while let Some(&c) = b.get(i) {
        match c {
            b'\'' | b'"' => {
                let end = closing(b, i + 1, c, false)?;
                word.extend_from_slice(&b[i + 1..end]);
                i = end + 1;
            }
            b'\\' => {
                word.push(*b.get(i + 1)?);
                i += 2;
            }
            _ if is_metachar(c) => break,
            _ => {
                word.push(c);
                i += 1;
            }
        }
    }
    (!word.is_empty()).then_some((word, i))
}

/// Past the heredoc text starting at `i` and the line that closes it.
fn past_heredoc(b: &[u8], mut i: usize, delimiter: &[u8], strip_tabs: bool) -> Option<usize> {
    while i < b.len() {
        let end = b[i..].iter().position(|&c| c == b'\n').map_or(b.len(), |n| i + n);
        let mut line = &b[i..end];
        if strip_tabs {
            while let [b'\t', rest @ ..] = line {
                line = rest;
            }
        }
        i = (end + 1).min(b.len());
        if line == delimiter {
            return Some(i);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (operator, text) of each step.
    fn parts(cmd: &str) -> Option<Vec<(&'static str, &str)>> {
        Some(split(cmd)?.into_iter().map(|s| (s.op.unwrap_or(""), &cmd[s.range])).collect())
    }

    #[test]
    fn chains_split_at_their_operators_keeping_them() {
        assert_eq!(
            parts("cd x && cargo build --release 2>&1 | tail -20 || echo failed; echo done").unwrap(),
            [("", "cd x"), ("&&", "cargo build --release 2>&1 | tail -20"), ("||", "echo failed"), (";", "echo done")]
        );
        assert_eq!(parts("sleep 5 & echo waiting").unwrap(), [("", "sleep 5"), ("&", "echo waiting")]);
        assert_eq!(parts("one\ntwo\n\nthree\n").unwrap(), [("", "one"), ("", "two"), ("", "three")]);
    }

    #[test]
    fn one_command_is_no_split() {
        for cmd in ["ls -la", "a | b | c", "a |& b", "cargo test 2>&1", "x &>log", "a >| b", "sleep 5 &", "echo hi;", "", "a \\\n  --flag \\\n  | b", "a |\n  b"] {
            assert_eq!(split(cmd), None, "{cmd}");
        }
    }

    #[test]
    fn operators_inside_something_stay_there() {
        for cmd in [
            "echo 'a && b'",
            "echo \"a && b; c\"",
            "echo $'it\\'s && b'",
            "echo a\\&\\&b",
            "x=$(a && b; c)",
            "echo \"$(a && echo \"b; c\")\"",
            "echo `a && b`",
            "echo ${x:-a;b}",
            "(a && b; c)",
            "{ a && b; }",
            "[[ a && b ]]",
            "[[ $x =~ (a|b) && -n y ]]",
            "if a && b; then c; else d; fi",
            "for x in 1 2; do a; b; done",
            "while a; do b && c; done",
            "case $x in a) b;; (c|d) e; f;; esac",
            "foo() { a; b; }",
            "echo a # b && c",
            "cat <<EOF\na && b; c\nEOF",
            "cat <<-'EOF' | tail\n\ta && b\n\tEOF",
            "diff <(a; b) <(c)",
            "echo $# && ",
        ] {
            assert_eq!(split(cmd), None, "{cmd}");
        }
    }

    #[test]
    fn compound_steps_are_whole_steps_of_a_chain() {
        assert_eq!(
            parts("NAME=\"a b\" && echo \"hi, $NAME\" # say it\nfor n in 1 2; do\n  printf '%s\\n' \"${n}\"\ndone; echo $(echo nested) ok").unwrap(),
            [("", "NAME=\"a b\""), ("&&", "echo \"hi, $NAME\" # say it"), ("", "for n in 1 2; do\n  printf '%s\\n' \"${n}\"\ndone"), (";", "echo $(echo nested) ok")]
        );
        assert_eq!(parts("(a; b) && { c; d; } > f").unwrap(), [("", "(a; b)"), ("&&", "{ c; d; } > f")]);
        assert_eq!(parts("if a; then b; fi && c").unwrap(), [("", "if a; then b; fi"), ("&&", "c")]);
        assert_eq!(parts("f() { a; }; f").unwrap(), [("", "f() { a; }"), (";", "f")]);
    }

    #[test]
    fn heredocs_stay_with_their_step() {
        assert_eq!(
            parts("cd x && python3 - <<'EOF' | tail\nprint('a && b; c')\nEOF\necho done").unwrap(),
            [("", "cd x"), ("&&", "python3 - <<'EOF' | tail\nprint('a && b; c')\nEOF"), ("", "echo done")]
        );
        assert_eq!(parts("a <<<x && b").unwrap(), [("", "a <<<x"), ("&&", "b")]);
    }

    #[test]
    fn line_ends_and_comments_belong_to_the_chain() {
        assert_eq!(parts("a &&\n  b").unwrap(), [("", "a"), ("&&", "b")]);
        assert_eq!(parts("a \\\n  && \\\n  b").unwrap(), [("", "a"), ("&&", "b")]);
        assert_eq!(parts("# first this\na\n# then that\nb").unwrap(), [("", "# first this\na"), ("", "# then that\nb")]);
        assert_eq!(parts("a && b &").unwrap(), [("", "a"), ("&&", "b &")]);
        assert_eq!(parts("a; b;").unwrap(), [("", "a"), (";", "b;")]);
    }

    #[test]
    fn what_is_not_understood_is_no_split() {
        for cmd in ["a && 'b", "a && \"b", "a && (b", "a && b)", "a && $(b", "a && `b", "a && ${b", "if a; then b", "a && fi", "a && cat <<EOF\nb", "a <<EOF && b\nc\nEOF", "function f { a; }; f", "a && && b", "a &&", "; a", "a;; b"] {
            assert_eq!(split(cmd), None, "{cmd}");
        }
    }

    /// Whatever is thrown at it: no panic, and steps are in order with only an operator and
    /// space between them, so nothing the command does can go unshown.
    #[test]
    fn steps_account_for_every_byte() {
        let pieces = ["a", " ", "\n", "&&", "&", "||", "|", ";", ";;", "'", "\"", "\\", "`", "$(", "(", ")", "{ ", " }", "${", "}", "[[ ", " ]]", "if ", "fi", "then ", "for ", "do ", "done", "case ", "esac", "<<E", "\nE\n", "<<-", "#", "é", "2>&1", ">"];
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut splits = 0;
        for _ in 0..200_000 {
            let mut cmd = String::new();
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let mut s = seed;
            for _ in 0..(s % 12) {
                s >>= 5;
                cmd.push_str(pieces[(s % pieces.len() as u64) as usize]);
            }
            let Some(steps) = split(&cmd) else { continue };
            splits += 1;
            let mut at = 0;
            for step in &steps {
                assert!(step.range.start >= at && step.range.start < step.range.end, "{cmd:?}: {steps:?}");
                let gap = cmd[at..step.range.start].replace("\\\n", "");
                let gap = gap.trim();
                assert!(gap.is_empty() || Some(gap) == step.op || Some(gap.trim_end_matches([';', '&'])) == step.op, "{cmd:?}: {gap:?} in {steps:?}");
                at = step.range.end;
            }
            assert!(cmd[at..].replace("\\\n", "").trim().is_empty(), "{cmd:?}: {steps:?}");
        }
        assert!(splits > 1000, "{splits}");
    }
}
