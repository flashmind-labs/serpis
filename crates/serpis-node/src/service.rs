//! The node as a per-user service: started when the machine boots, and again when it crashes,
//! so a chat cut off overnight (a reboot, a crash) continues without anyone connecting. systemd
//! user units on Linux, launchd agents on macOS; installed only when asked
//! (`remote-install-service`).
//!
//! The service runs `~/.serpis/bin/serpis-node-current`, a link to whichever build last started
//! the node: serpis installs each build under its own name, and a node started by a client
//! (`serpis-node agent`, a window) goes through the service once there is one, pointing the link
//! at itself first. `$SERPIS_SERVICE_DIR` and `$SERPIS_SERVICE_CTL` put the service file and the
//! program that runs `systemctl`/`launchctl` elsewhere (tests).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

const UNIT: &str = "serpis-node.service";
const LABEL: &str = "dev.serpis.node";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Manager {
    Systemd,
    Launchd,
}

/// What an install left behind, kept beside the node's settings (`service.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Installed {
    pub manager: Manager,
    /// The unit or agent file.
    pub file: String,
    /// The socket the service's node listens on.
    pub socket: String,
}

fn record() -> PathBuf {
    crate::node_dir().join("service.json")
}

/// The service installed for this node, if any.
pub fn installed() -> Option<Installed> {
    serde_json::from_str(&std::fs::read_to_string(record()).ok()?).ok()
}

/// The link the service runs.
pub fn link() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".serpis/bin/serpis-node-current")
}

fn manager() -> Result<Manager> {
    if cfg!(target_os = "macos") {
        return Ok(Manager::Launchd);
    }
    if std::env::var_os("SERPIS_SERVICE_CTL").is_some() || std::process::Command::new("systemctl").args(["--user", "--version"]).output().is_ok_and(|o| o.status.success()) {
        return Ok(Manager::Systemd);
    }
    bail!("this machine has neither launchd nor a systemd user manager (`systemctl --user`), so serpis-node can't be started at boot here; it still starts whenever serpis connects")
}

fn service_file(m: Manager) -> PathBuf {
    let dir = std::env::var_os("SERPIS_SERVICE_DIR").map(PathBuf::from).unwrap_or_else(|| {
        let home = dirs::home_dir().unwrap_or_default();
        match m {
            // Where systemd looks: $XDG_CONFIG_HOME, else ~/.config.
            Manager::Systemd => std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()).map(PathBuf::from).unwrap_or_else(|| home.join(".config")).join("systemd/user"),
            Manager::Launchd => home.join("Library/LaunchAgents"),
        }
    });
    dir.join(match m {
        Manager::Systemd => UNIT.to_string(),
        Manager::Launchd => format!("{LABEL}.plist"),
    })
}

/// Run `systemctl --user ...` or `launchctl ...` (or `$SERPIS_SERVICE_CTL` with the same words).
fn ctl(m: Manager, args: &[&str]) -> Result<String> {
    let (program, mut words): (String, Vec<String>) = match m {
        Manager::Systemd => ("systemctl".into(), vec!["--user".into()]),
        Manager::Launchd => ("launchctl".into(), Vec::new()),
    };
    words.extend(args.iter().map(|a| a.to_string()));
    let program = std::env::var("SERPIS_SERVICE_CTL").unwrap_or(program);
    let mut cmd = std::process::Command::new(&program);
    cmd.args(&words);
    // A login that didn't go through PAM (some ssh servers, rayfish's, `su`) has no
    // XDG_RUNTIME_DIR, and `systemctl --user` can't find the user's manager without it.
    if m == Manager::Systemd && std::env::var_os("XDG_RUNTIME_DIR").is_none() {
        let run = PathBuf::from(format!("/run/user/{}", nix::unistd::getuid()));
        if !run.join("bus").exists() && std::env::var_os("SERPIS_SERVICE_CTL").is_none() {
            let user = nix::unistd::User::from_uid(nix::unistd::getuid()).ok().flatten().map(|u| u.name).unwrap_or_default();
            bail!("no systemd user manager runs for {user} now (nobody is logged in, and lingering is off): run `loginctl enable-linger {user}` there (with sudo, if it asks), then try again");
        }
        cmd.env("XDG_RUNTIME_DIR", run);
    }
    let out = cmd.output().with_context(|| format!("running {program}"))?;
    if !out.status.success() {
        bail!("{program} {}: {}", words.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// What the node gets in the service: where it keeps its things (as it was started with), and
/// the PATH its tools look programs up in (a service's own is short).
fn environment(socket: &Path) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = ["PATH", "SERPIS_NODE_DIR", "SERPIS_AGENT_DIR", "SERPIS_CONFIG_DIR", "SERPIS_TERMINAL_SOCKET"]
        .iter()
        .filter_map(|k| Some((k.to_string(), std::env::var(k).ok()?)))
        .collect();
    env.push(("SERPIS_NODE_SOCKET".into(), socket.to_string_lossy().into_owned()));
    env
}

/// A systemd user unit for the node. (systemd reads `%` as the start of a specifier, `%h` its
/// home: written `%%`, it stays a `%`.)
pub fn unit(exe: &Path, env: &[(String, String)]) -> String {
    let quote = |v: &str| v.replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%");
    let mut s = String::from("[Unit]\nDescription=serpis node: runs serpis chats on this machine\n\n[Service]\n");
    s.push_str(&format!("ExecStart=\"{}\" serve\n", quote(&exe.to_string_lossy())));
    for (k, v) in env {
        s.push_str(&format!("Environment=\"{k}={}\"\n", quote(v)));
    }
    s.push_str("Restart=on-failure\nRestartSec=2\n\n[Install]\nWantedBy=default.target\n");
    s
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// A launchd agent for the node: started at login, and again when it exits with an error.
pub fn plist(exe: &Path, env: &[(String, String)], log: &Path) -> String {
    let vars: String = env.iter().map(|(k, v)| format!("    <key>{}</key><string>{}</string>\n", xml(k), xml(v))).collect();
    format!(
        concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
            "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
            "<plist version=\"1.0\"><dict>\n",
            "  <key>Label</key><string>{label}</string>\n",
            "  <key>ProgramArguments</key><array><string>{exe}</string><string>serve</string></array>\n",
            "  <key>EnvironmentVariables</key><dict>\n{vars}  </dict>\n",
            "  <key>RunAtLoad</key><true/>\n",
            "  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>\n",
            "  <key>StandardOutPath</key><string>{log}</string>\n",
            "  <key>StandardErrorPath</key><string>{log}</string>\n",
            "</dict></plist>\n"
        ),
        label = LABEL,
        exe = xml(&exe.to_string_lossy()),
        vars = vars,
        log = xml(&log.to_string_lossy()),
    )
}

/// Point the service's link at `exe`.
fn relink(exe: &Path) -> Result<()> {
    let l = link();
    std::fs::create_dir_all(l.parent().context("no directory for the link")?)?;
    let tmp = l.with_extension("new");
    let _ = std::fs::remove_file(&tmp);
    std::os::unix::fs::symlink(exe, &tmp)?;
    std::fs::rename(&tmp, &l)?;
    Ok(())
}

/// What `install` did, for the person who asked.
pub struct Report {
    pub installed: Installed,
    /// Something they still have to do (Linux: let the user's services run without a login).
    pub note: Option<String>,
}

/// Install the node as a service running `exe` (this node's build) on `socket`, started at boot
/// and after crashes. It doesn't start one now: the node running keeps running, and the next
/// start (an upgrade, a crash) goes through the service.
pub fn install(exe: &Path, socket: &Path) -> Result<Report> {
    let m = manager()?;
    relink(exe)?;
    let env = environment(socket);
    let file = service_file(m);
    std::fs::create_dir_all(file.parent().context("no directory for the service")?)?;
    let log = socket.parent().unwrap_or(Path::new("/tmp")).join("node.log");
    std::fs::write(&file, match m {
        Manager::Systemd => unit(&link(), &env),
        Manager::Launchd => plist(&link(), &env, &log),
    })?;
    let mut note = None;
    match m {
        Manager::Systemd => {
            ctl(m, &["daemon-reload"])?;
            ctl(m, &["enable", UNIT])?;
            let user = nix::unistd::User::from_uid(nix::unistd::getuid()).ok().flatten().map(|u| u.name).unwrap_or_default();
            let linger = std::process::Command::new("loginctl").args(["show-user", &user, "-p", "Linger"]).output().map(|o| String::from_utf8_lossy(&o.stdout).trim() == "Linger=yes").unwrap_or(false);
            if !linger && std::env::var_os("SERPIS_SERVICE_CTL").is_none() {
                note = Some(format!("for it to start at boot without anyone logged in, run once there: loginctl enable-linger {user} (with sudo, if it asks for authentication)"));
            }
        }
        Manager::Launchd => {
            // Loaded now (it exits at once: a node already runs), and at every login.
            let uid = nix::unistd::getuid();
            let _ = ctl(m, &["bootout", &format!("gui/{uid}/{LABEL}")]);
            ctl(m, &["bootstrap", &format!("gui/{uid}"), &file.to_string_lossy()])
                .context("launchd has no session for this user to start it in (a Mac reached over ssh with nobody logged in at it): log in there once, or run `serpis-node install-service` from a terminal on it")?;
            note = Some("macOS starts it when you log in (a reboot needs a login first, unless the Mac logs in by itself)".into());
        }
    }
    let installed = Installed { manager: m, file: file.to_string_lossy().into_owned(), socket: socket.to_string_lossy().into_owned() };
    std::fs::write(record(), serde_json::to_string(&installed)?)?;
    Ok(Report { installed, note })
}

/// Remove the service (the node running keeps running).
pub fn uninstall() -> Result<Option<Installed>> {
    let Some(i) = installed() else { return Ok(None) };
    match i.manager {
        Manager::Systemd => {
            let _ = ctl(i.manager, &["disable", UNIT]);
        }
        Manager::Launchd => {
            let _ = ctl(i.manager, &["bootout", &format!("gui/{}/{LABEL}", nix::unistd::getuid())]);
        }
    }
    let _ = std::fs::remove_file(&i.file);
    let _ = std::fs::remove_file(record());
    if i.manager == Manager::Systemd {
        let _ = ctl(i.manager, &["daemon-reload"]);
    }
    Ok(Some(i))
}

/// Start the node on `socket` through its service, running `exe`: false when it has none for
/// that socket (the caller starts it itself).
pub fn start(exe: &Path, socket: &Path) -> Result<bool> {
    let Some(i) = installed().filter(|i| Path::new(&i.socket) == socket) else { return Ok(false) };
    relink(exe)?;
    match i.manager {
        Manager::Systemd => ctl(i.manager, &["restart", UNIT])?,
        Manager::Launchd => ctl(i.manager, &["kickstart", "-k", &format!("gui/{}/{LABEL}", nix::unistd::getuid())])?,
    };
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_unit_runs_the_link_and_restarts_on_failure() {
        let env = vec![("PATH".to_string(), "/usr/bin:/a \"b\":/50%".to_string())];
        let u = unit(Path::new("/h/.serpis/bin/serpis-node-current"), &env);
        assert!(u.contains("ExecStart=\"/h/.serpis/bin/serpis-node-current\" serve\n"), "{u}");
        assert!(u.contains("Environment=\"PATH=/usr/bin:/a \\\"b\\\":/50%%\"\n"), "{u}");
        assert!(u.contains("Restart=on-failure") && u.contains("WantedBy=default.target"), "{u}");
        let p = plist(Path::new("/h/x&y"), &env, Path::new("/h/node.log"));
        assert!(p.contains("<string>/h/x&amp;y</string><string>serve</string>"), "{p}");
        assert!(p.contains("<key>SuccessfulExit</key><false/>") && p.contains("<key>RunAtLoad</key><true/>"), "{p}");
    }
}
