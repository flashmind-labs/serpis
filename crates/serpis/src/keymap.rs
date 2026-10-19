//! Keymaps: prefix trees from key chords to commands, with per-binding ownership so a
//! reloaded package removes exactly the bindings it made.

use std::collections::{BTreeMap, HashMap};

use crate::keys::{KeyChord, format_seq};

#[derive(Debug, Clone, PartialEq)]
pub enum Binding {
    /// `shadowed` is the binding this one replaced; it comes back when this owner is removed.
    Command { name: String, owner: String, shadowed: Option<Box<Binding>> },
    Prefix(Keymap),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Keymap {
    pub bindings: BTreeMap<KeyChord, Binding>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Lookup {
    Command(String),
    Prefix,
    None,
}

impl Keymap {
    pub fn bind(&mut self, seq: &[KeyChord], command: &str, owner: &str) {
        let Some((last, prefix)) = seq.split_last() else { return };
        let mut map = self;
        for chord in prefix {
            let entry = map.bindings.entry(chord.clone()).or_insert_with(|| Binding::Prefix(Keymap::default()));
            if !matches!(entry, Binding::Prefix(_)) {
                *entry = Binding::Prefix(Keymap::default());
            }
            let Binding::Prefix(next) = entry else { unreachable!() };
            map = next;
        }
        let shadowed = match map.bindings.remove(last) {
            // Rebinding within one owner replaces; across owners it shadows.
            Some(Binding::Command { owner: o, shadowed, .. }) if o == owner => shadowed,
            Some(prev) => Some(Box::new(prev)),
            None => None,
        };
        map.bindings.insert(
            last.clone(),
            Binding::Command {
                name: command.to_string(),
                owner: owner.to_string(),
                shadowed,
            },
        );
    }

    pub fn unbind(&mut self, seq: &[KeyChord]) {
        let Some((last, prefix)) = seq.split_last() else { return };
        let mut map = self;
        for chord in prefix {
            match map.bindings.get_mut(chord) {
                Some(Binding::Prefix(next)) => map = next,
                _ => return,
            }
        }
        map.bindings.remove(last);
    }

    pub fn lookup(&self, seq: &[KeyChord]) -> Lookup {
        let mut map = self;
        for (i, chord) in seq.iter().enumerate() {
            match map.bindings.get(chord) {
                Some(Binding::Command { name, .. }) if i == seq.len() - 1 => return Lookup::Command(name.clone()),
                Some(Binding::Command { .. }) => return Lookup::None,
                Some(Binding::Prefix(next)) => map = next,
                None => return Lookup::None,
            }
        }
        if seq.is_empty() { Lookup::None } else { Lookup::Prefix }
    }

    /// The sub-map reached by a prefix (for which-key).
    pub fn submap(&self, seq: &[KeyChord]) -> Option<&Keymap> {
        let mut map = self;
        for chord in seq {
            match map.bindings.get(chord) {
                Some(Binding::Prefix(next)) => map = next,
                _ => return None,
            }
        }
        Some(map)
    }

    /// Remove every binding made by `owner`, restoring what it shadowed and pruning empty prefixes.
    pub fn remove_owner(&mut self, owner: &str) {
        let keys: Vec<KeyChord> = self.bindings.keys().cloned().collect();
        for k in keys {
            let b = self.bindings.remove(&k).unwrap();
            if let Some(b) = strip_owner(b, owner) {
                self.bindings.insert(k, b);
            }
        }
    }

    /// Flatten to (sequence, command) pairs.
    pub fn flatten(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        fn walk(map: &Keymap, prefix: &mut Vec<KeyChord>, out: &mut Vec<(String, String)>) {
            for (chord, b) in &map.bindings {
                prefix.push(chord.clone());
                match b {
                    Binding::Command { name, .. } => out.push((format_seq(prefix), name.clone())),
                    Binding::Prefix(m) => walk(m, prefix, out),
                }
                prefix.pop();
            }
        }
        walk(self, &mut Vec::new(), &mut out);
        out
    }
}

fn strip_owner(b: Binding, owner: &str) -> Option<Binding> {
    match b {
        Binding::Command { name, owner: o, shadowed } => {
            let shadowed = shadowed.and_then(|s| strip_owner(*s, owner)).map(Box::new);
            if o == owner {
                shadowed.map(|s| *s)
            } else {
                Some(Binding::Command { name, owner: o, shadowed })
            }
        }
        Binding::Prefix(mut m) => {
            m.remove_owner(owner);
            (!m.bindings.is_empty()).then_some(Binding::Prefix(m))
        }
    }
}

/// All named keymaps. Names follow the lookup convention in `Editor::active_keymaps`:
/// `global`, `<major>` (e.g. `chat`), `<minor>` (e.g. `vim-normal`), `<minor>:<major>`.
#[derive(Default)]
pub struct Keymaps {
    pub maps: HashMap<String, Keymap>,
}

impl Keymaps {
    pub fn get_mut(&mut self, name: &str) -> &mut Keymap {
        self.maps.entry(name.to_string()).or_default()
    }
    pub fn get(&self, name: &str) -> Option<&Keymap> {
        self.maps.get(name)
    }
    pub fn remove_owner(&mut self, owner: &str) {
        for m in self.maps.values_mut() {
            m.remove_owner(owner);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::parse_seq;

    #[test]
    fn prefixes_and_owners() {
        let mut m = Keymap::default();
        m.bind(&parse_seq("C-x C-f").unwrap(), "find-file", "a");
        m.bind(&parse_seq("C-x o").unwrap(), "other-window", "b");
        assert_eq!(m.lookup(&parse_seq("C-x").unwrap()), Lookup::Prefix);
        assert_eq!(m.lookup(&parse_seq("C-x o").unwrap()), Lookup::Command("other-window".into()));
        m.remove_owner("b");
        assert_eq!(m.lookup(&parse_seq("C-x o").unwrap()), Lookup::None);
        m.remove_owner("a");
        assert!(m.bindings.is_empty());
    }

    #[test]
    fn shadowed_bindings_come_back() {
        let mut m = Keymap::default();
        let k = parse_seq("s-1").unwrap();
        m.bind(&k, "select-window-1", "base", );
        m.bind(&k, "workspace-1", "lua");
        assert_eq!(m.lookup(&k), Lookup::Command("workspace-1".into()));
        m.remove_owner("lua");
        assert_eq!(m.lookup(&k), Lookup::Command("select-window-1".into()));
    }
}
