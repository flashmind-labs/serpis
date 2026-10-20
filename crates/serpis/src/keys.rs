//! Key chords in Emacs `kbd` notation: `C-x`, `M-x`, `s-p` (super/cmd), `S-<tab>`,
//! `<escape>`, `RET`, `SPC`, `C-M-<up>`. Sequences are space separated: `C-x C-f`.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Mods {
    pub ctrl: bool,
    pub meta: bool,
    pub shift: bool,
    /// Cmd on macOS, Super/Windows elsewhere.
    pub sup: bool,
}

impl Mods {
    pub const NONE: Mods = Mods {
        ctrl: false,
        meta: false,
        shift: false,
        sup: false,
    };
    pub fn is_empty(&self) -> bool {
        !(self.ctrl || self.meta || self.shift || self.sup)
    }
    /// Any modifier that turns a printable key into a command.
    pub fn is_command(&self) -> bool {
        self.ctrl || self.meta || self.sup
    }
}

/// The non-modifier part of a chord.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Key {
    /// A printable character. Shift is folded into the character (`A`, `?`), not into `Mods`.
    Char(char),
    /// A named key: `escape`, `return`, `tab`, `backspace`, `delete`, `up`, `f5`, ...
    Named(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeyChord {
    pub mods: Mods,
    pub key: Key,
}

impl KeyChord {
    pub fn ch(c: char) -> Self {
        // Space is the named key `SPC` however it arrives (typed text or a key event), so
        // bindings like `SPC b b` match real input.
        if c == ' ' {
            return KeyChord::named("space");
        }
        KeyChord {
            mods: Mods::NONE,
            key: Key::Char(c),
        }
    }
    pub fn named(name: &str) -> Self {
        KeyChord {
            mods: Mods::NONE,
            key: Key::Named(name.to_string()),
        }
    }
    pub fn with(mut self, mods: Mods) -> Self {
        self.mods = mods;
        self
    }
    /// The character this chord types, if it is plain self-inserting input.
    pub fn printable(&self) -> Option<char> {
        match self.key {
            Key::Char(c) if !self.mods.is_command() => Some(c),
            _ if self.is("space") && !self.mods.is_command() => Some(' '),
            _ => None,
        }
    }
    pub fn is(&self, name: &str) -> bool {
        matches!(&self.key, Key::Named(n) if n == name)
    }
}

fn canonical_named(name: &str) -> String {
    match name.to_lowercase().as_str() {
        "esc" | "escape" => "escape".into(),
        "ret" | "return" | "enter" | "cr" => "return".into(),
        "tab" => "tab".into(),
        "spc" | "space" => "space".into(),
        "del" | "backspace" | "bs" => "backspace".into(),
        "delete" | "deletechar" => "delete".into(),
        "left" => "left".into(),
        "right" => "right".into(),
        "up" => "up".into(),
        "down" => "down".into(),
        "home" => "home".into(),
        "end" => "end".into(),
        "prior" | "pageup" => "pageup".into(),
        "next" | "pagedown" => "pagedown".into(),
        "insert" => "insert".into(),
        other => other.to_string(),
    }
}

impl std::str::FromStr for KeyChord {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let mut mods = Mods::NONE;
        let mut rest = s;
        loop {
            let b = rest.as_bytes();
            if b.len() > 2 && b[1] == b'-' {
                match b[0] {
                    b'C' => mods.ctrl = true,
                    b'M' => mods.meta = true,
                    b'S' => mods.shift = true,
                    b's' => mods.sup = true,
                    _ => break,
                }
                rest = &rest[2..];
            } else {
                break;
            }
        }
        let key = if let Some(name) = rest.strip_prefix('<').and_then(|r| r.strip_suffix('>')) {
            Key::Named(canonical_named(name))
        } else if ["RET", "SPC", "TAB", "ESC", "DEL"].contains(&rest) {
            Key::Named(canonical_named(rest))
        } else {
            let mut chars = rest.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Key::Char(c),
                _ => return Err(format!("invalid key: {s}")),
            }
        };
        // `S-a` means `A`.
        if let (Key::Char(c), true) = (&key, mods.shift) {
            if c.is_alphabetic() {
                mods.shift = false;
                return Ok(KeyChord {
                    mods,
                    key: Key::Char(c.to_uppercase().next().unwrap_or(*c)),
                });
            }
        }
        if key == Key::Char(' ') {
            return Ok(KeyChord {
                mods,
                key: Key::Named("space".into()),
            });
        }
        Ok(KeyChord { mods, key })
    }
}

impl fmt::Display for KeyChord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.mods.ctrl {
            f.write_str("C-")?;
        }
        if self.mods.meta {
            f.write_str("M-")?;
        }
        if self.mods.shift {
            f.write_str("S-")?;
        }
        if self.mods.sup {
            f.write_str("s-")?;
        }
        match &self.key {
            Key::Char(c) => write!(f, "{c}"),
            Key::Named(n) => match n.as_str() {
                "return" => f.write_str("RET"),
                "space" => f.write_str("SPC"),
                "tab" => f.write_str("TAB"),
                "escape" => f.write_str("ESC"),
                "backspace" => f.write_str("DEL"),
                other => write!(f, "<{other}>"),
            },
        }
    }
}

/// Parse a space-separated key sequence.
pub fn parse_seq(s: &str) -> Result<Vec<KeyChord>, String> {
    s.split_whitespace().map(str::parse).collect()
}

pub fn format_seq(seq: &[KeyChord]) -> String {
    seq.iter().map(ToString::to_string).collect::<Vec<_>>().join(" ")
}

/// Convert an egui key + modifiers into a chord. Printable keys without command modifiers
/// return `None`: they arrive as `Event::Text` instead (which respects keyboard layouts).
pub fn chord_from_egui(key: egui::Key, m: egui::Modifiers) -> Option<KeyChord> {
    use egui::Key as K;
    let mods = Mods {
        ctrl: m.ctrl,
        meta: m.alt,
        shift: m.shift,
        sup: if cfg!(target_os = "macos") { m.mac_cmd } else { false },
    };
    let named = |n: &str| Some(KeyChord::named(n).with(mods));
    let printable = |c: char| -> Option<KeyChord> {
        if !mods.is_command() {
            return None;
        }
        let c = if mods.shift && c.is_ascii_alphabetic() { c.to_ascii_uppercase() } else { c };
        // Shift is folded into the character: `A` for letters, the shifted symbol otherwise.
        Some(KeyChord {
            mods: Mods { shift: false, ..mods },
            key: Key::Char(c),
        })
    };
    match key {
        K::ArrowDown => named("down"),
        K::ArrowUp => named("up"),
        K::ArrowLeft => named("left"),
        K::ArrowRight => named("right"),
        K::Escape => named("escape"),
        K::Tab => named("tab"),
        K::Backspace => named("backspace"),
        K::Enter => named("return"),
        K::Space => {
            if mods.is_command() || mods.shift {
                named("space")
            } else {
                None
            }
        }
        K::Insert => named("insert"),
        K::Delete => named("delete"),
        K::Home => named("home"),
        K::End => named("end"),
        K::PageUp => named("pageup"),
        K::PageDown => named("pagedown"),
        K::F1 => named("f1"),
        K::F2 => named("f2"),
        K::F3 => named("f3"),
        K::F4 => named("f4"),
        K::F5 => named("f5"),
        K::F6 => named("f6"),
        K::F7 => named("f7"),
        K::F8 => named("f8"),
        K::F9 => named("f9"),
        K::F10 => named("f10"),
        K::F11 => named("f11"),
        K::F12 => named("f12"),
        // egui names this key with the typographic minus sign (U+2212).
        K::Minus => printable('-'),
        other => {
            let name = other.symbol_or_name();
            let mut it = name.chars();
            match (it.next(), it.next()) {
                (Some(c), None) => printable(c.to_ascii_lowercase()),
                _ => None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a keyboard delivers for a bound chord: plain characters (and space) arrive as
    /// typed text, everything else as a key event.
    fn delivered(c: &KeyChord) -> Option<KeyChord> {
        let mods = egui::Modifiers { alt: c.mods.meta, ctrl: c.mods.ctrl, shift: c.mods.shift, mac_cmd: c.mods.sup, command: c.mods.sup, };
        match &c.key {
            Key::Char(ch) if !c.mods.is_command() => Some(KeyChord::ch(*ch)),
            Key::Named(n) if n == "space" && !c.mods.is_command() && !c.mods.shift => Some(KeyChord::ch(' ')),
            Key::Char(ch) => {
                let shift = ch.is_ascii_uppercase();
                let key = egui::Key::from_name(&ch.to_ascii_uppercase().to_string())?;
                chord_from_egui(key, egui::Modifiers { shift: mods.shift || shift, ..mods })
            }
            Key::Named(n) => {
                let name = match n.as_str() {
                    "return" => "Enter",
                    "escape" => "Escape",
                    "tab" => "Tab",
                    "backspace" => "Backspace",
                    "space" => "Space",
                    "up" => "ArrowUp",
                    "down" => "ArrowDown",
                    "left" => "ArrowLeft",
                    "right" => "ArrowRight",
                    other => return egui::Key::from_name(&other.to_uppercase()).and_then(|k| chord_from_egui(k, mods)),
                };
                chord_from_egui(egui::Key::from_name(name)?, mods)
            }
        }
    }

    /// Every kind of binding must be reachable from real input (`SPC` once wasn't: typed
    /// space arrived as a character while bindings named it).
    #[test]
    fn bindings_match_real_input() {
        for s in ["SPC", "C-SPC", "a", "A", ":", "s-p", "s-P", "s--", "s-=", "s-+", "s-0", "C-s-3", "C-x", "M-x", "RET", "ESC", "TAB", "DEL", "<up>", "C-w H", "<f5>"] {
            for chord in parse_seq(s).unwrap() {
                if cfg!(not(target_os = "macos")) && chord.mods.sup {
                    continue;
                }
                assert_eq!(delivered(&chord).as_ref(), Some(&chord), "{s}");
            }
        }
    }

    #[test]
    fn roundtrip() {
        for s in ["C-x", "M-x", "s-p", "C-M-<up>", "RET", "SPC", "<f5>", "A", "C-S-TAB"] {
            let c: KeyChord = s.parse().unwrap();
            assert_eq!(c.to_string(), s, "{s}");
        }
        assert_eq!("S-a".parse::<KeyChord>().unwrap(), KeyChord::ch('A'));
        assert_eq!(parse_seq("C-x C-f").unwrap().len(), 2);
    }
}
