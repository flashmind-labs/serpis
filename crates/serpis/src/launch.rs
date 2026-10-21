//! Started from the Dock or Finder, an app gets launchd's bare environment: no PATH from the
//! shell (Homebrew's git, yazi, cargo for the agent's tools), no exported API keys, and `/` as its
//! directory. Like VS Code and Emacs' exec-path-from-shell, serpis then reads the login shell's
//! environment once at startup and starts in the home folder. Started from a terminal (`TERM`
//! set) it keeps what it was given. `SERPIS_NO_SHELL_ENV=1` turns the import off.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Duration;

const MARKER: &str = "__SERPIS_ENV__";

/// How long the login shell may take to print its environment.
const TIMEOUT: Duration = Duration::from_secs(5);

/// Variables that describe the shell process itself, not the user's environment.
const SKIP: &[&str] = &["PWD", "OLDPWD", "SHLVL", "_", "TERM", "TERM_PROGRAM", "TERM_PROGRAM_VERSION", "TERM_SESSION_ID", "COLUMNS", "LINES"];

/// Prepare the process environment before anything reads it (call first, single-threaded).
pub fn prepare() {
    if std::env::var_os("TERM").is_some() {
        return;
    }
    if std::env::var_os("SERPIS_NO_SHELL_ENV").is_none() {
        match login_env() {
            Some(vars) => {
                for (k, v) in vars {
                    // SAFETY: called at startup, before serpis starts any thread.
                    unsafe { std::env::set_var(k, v) };
                }
            }
            None => eprintln!("serpis: couldn't read the login shell's environment"),
        }
    }
    if std::env::current_dir().is_ok_and(|d| d == std::path::Path::new("/"))
        && let Some(home) = std::env::var_os("HOME")
    {
        let _ = std::env::set_current_dir(home);
    }
}

/// The login shell's environment (interactive too, so `.zshrc` exports count).
fn login_env() -> Option<Vec<(String, String)>> {
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/zsh".into());
    let mut child = Command::new(&shell)
        .args(["-l", "-i", "-c", &format!("printf '{MARKER}'; env -0")])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut out = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    let buf = match rx.recv_timeout(TIMEOUT) {
        Ok(b) => b,
        Err(_) => {
            let _ = child.kill();
            return None;
        }
    };
    let _ = child.wait();
    parse(&String::from_utf8_lossy(&buf))
}

/// `KEY=value` entries after the marker, NUL-separated (whatever the shell printed before it,
/// like a greeting from `.zshrc`, is ignored).
fn parse(out: &str) -> Option<Vec<(String, String)>> {
    let (_, env) = out.split_once(MARKER)?;
    Some(
        env.split('\0')
            .filter_map(|e| e.split_once('='))
            .filter(|(k, _)| !k.is_empty() && !SKIP.contains(k))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_after_the_marker() {
        let out = format!("Welcome!\n{MARKER}PATH=/opt/homebrew/bin:/usr/bin\0KEY=a=b\0SHLVL=2\0MULTI=x\ny\0");
        let v = parse(&out).unwrap();
        assert_eq!(v, [("PATH".into(), "/opt/homebrew/bin:/usr/bin".into()), ("KEY".into(), "a=b".into()), ("MULTI".into(), "x\ny".into())]);
        assert!(parse("no marker").is_none());
    }
}
