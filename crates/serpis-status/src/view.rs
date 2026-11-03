//! What the indicator says, from what `serpis-node status` printed. No AppKit here.

use std::path::Path;

use serde_json::Value;

/// The app a helper at `exe` is inside: Serpis.app/Contents/Helpers/SerpisStatus.app/Contents/MacOS/serpis-status.
pub fn app_of(exe: &Path) -> Option<&Path> {
    exe.ancestors().nth(6)
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    pub node: bool,
    pub terminal_host: bool,
    pub chats: u64,
    pub working: u64,
    pub terminals: u64,
    pub indicator: bool,
}

impl Status {
    /// None for anything that isn't the node's answer.
    pub fn parse(json: &str) -> Option<Status> {
        let v: Value = serde_json::from_str(json).ok()?;
        let flag = |k: &str| v.get(k).and_then(Value::as_bool);
        let count = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
        Some(Status { node: flag("node")?, terminal_host: flag("terminal_host")?, chats: count("chats"), working: count("working"), terminals: count("terminals"), indicator: flag("indicator")? })
    }

    pub fn running(&self) -> bool {
        self.node || self.terminal_host
    }

    /// Quitting would cut something off, so it asks first.
    pub fn busy(&self) -> bool {
        self.working > 0 || self.terminals > 0
    }

    /// Beside the icon: how many chats are working, when any is.
    pub fn title(&self) -> String {
        if self.working > 0 { format!(" {}", self.working) } else { String::new() }
    }

    /// The menu's first line.
    pub fn headline(&self) -> &'static str {
        if self.node { "Serpis node is running" } else { "Serpis terminals are running" }
    }

    /// The menu's second line.
    pub fn summary(&self) -> String {
        let count = |n: u64, one: &str| format!("{n} {one}{}", if n == 1 { "" } else { "s" });
        let mut parts = Vec::new();
        if self.node {
            parts.push(if self.working > 0 { format!("{}, {} working", count(self.chats, "chat"), self.working) } else { count(self.chats, "chat") });
        }
        if self.terminal_host {
            parts.push(count(self.terminals, "terminal"));
        }
        parts.join(" · ")
    }

    /// What the quit dialog warns about.
    pub fn quit_warning(&self) -> String {
        let mut said = Vec::new();
        if self.working > 0 {
            said.push("Working chats stop, and carry on when the node next starts.".to_string());
        }
        if self.terminals > 0 {
            said.push(format!("{} and whatever runs in {} will end.", if self.terminals == 1 { "1 open terminal".to_string() } else { format!("{} open terminals", self.terminals) }, if self.terminals == 1 { "it" } else { "them" }));
        }
        said.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(node: bool, host: bool, chats: u64, working: u64, terminals: u64) -> Status {
        Status { node, terminal_host: host, chats, working, terminals, indicator: true }
    }

    #[test]
    fn the_helper_finds_the_app_it_is_inside() {
        let exe = Path::new("/Applications/Serpis.app/Contents/Helpers/SerpisStatus.app/Contents/MacOS/serpis-status");
        assert_eq!(app_of(exe), Some(Path::new("/Applications/Serpis.app")));
    }

    #[test]
    fn it_reads_the_nodes_answer() {
        let s = Status::parse(r#"{"node":true,"terminal_host":false,"chats":2,"working":1,"terminals":0,"indicator":true}"#).unwrap();
        assert_eq!(s, status(true, false, 2, 1, 0));
        assert_eq!(Status::parse("not json"), None);
        assert_eq!(Status::parse(r#"{"node":true}"#), None);
    }

    #[test]
    fn it_says_what_runs() {
        let idle = status(true, false, 1, 0, 0);
        assert_eq!((idle.title().as_str(), idle.summary().as_str(), idle.busy()), ("", "1 chat", false));
        let working = status(true, true, 3, 2, 1);
        assert_eq!((working.title().as_str(), working.summary().as_str(), working.busy()), (" 2", "3 chats, 2 working · 1 terminal", true));
        let terminals = status(false, true, 0, 0, 2);
        assert_eq!((terminals.headline(), terminals.summary().as_str(), terminals.busy()), ("Serpis terminals are running", "2 terminals", true));
        assert_eq!(status(true, false, 0, 0, 0).summary(), "0 chats");
        assert!(!status(false, false, 0, 0, 0).running());
    }

    #[test]
    fn quitting_warns_about_what_it_cuts_off() {
        assert_eq!(status(true, false, 1, 1, 0).quit_warning(), "Working chats stop, and carry on when the node next starts.");
        assert_eq!(status(true, true, 0, 0, 1).quit_warning(), "1 open terminal and whatever runs in it will end.");
        assert_eq!(status(true, true, 0, 0, 2).quit_warning(), "2 open terminals and whatever runs in them will end.");
    }
}
