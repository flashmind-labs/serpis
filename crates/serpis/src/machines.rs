//! Adding machines (on the desktop, where machines are set up): `machine-add` suggests machines
//! from your ssh config, Tailscale, rayfish, this network and the ones used before, or takes any
//! ssh destination (`box`, `me@10.0.0.5`, `ssh://me@box:2222`). It checks serpis can log in
//! there, and walks through what stands in the way: a host key to trust (kept in serpis's own
//! known_hosts, `remote-known-hosts`; ssh's is only read), a key the machine doesn't accept yet
//! (`ssh-copy-id` in a terminal, where the password is typed once), another account. Machines
//! added are kept in `remote-machines`, and serpis installs its node there as it connects.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use serpis_node::client::Target;

use crate::editor::Editor;
use crate::minibuffer::Candidate;

/// The machine picker's "Add a machine…" entry.
pub const ADD: &str = "\u{1}add-machine";
const PROMPT: &str = "Add a machine: ";

/// A machine to offer.
#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    /// How ssh reaches it.
    pub dest: String,
    pub source: &'static str,
    pub detail: String,
}

type Then = Box<dyn FnOnce(&mut Editor, String)>;

/// The machine being added: where, what it will be called, and what wanted it.
struct Attempt {
    dest: String,
    name: Option<String>,
    then: Option<Then>,
}

/// What `machine-add` is doing.
#[derive(Default)]
pub struct Adding {
    /// Machines found in the background (Tailscale, this network), by source.
    found: Vec<Suggestion>,
    /// Searches still running.
    pending: usize,
    /// A terminal copying a key there, and the machine to check again once it ends.
    copying: Option<(crate::buffer::BufferId, Attempt)>,
    /// Machines being checked in the background, by token (what wanted them needn't be Send).
    checking: std::collections::HashMap<u64, Attempt>,
    next: u64,
}

/// Keep `a` here while the background works on it; the token gets it back.
fn park(ed: &mut Editor, a: Attempt) -> (u64, String) {
    let dest = a.dest.clone();
    ed.remote.adding.next += 1;
    let token = ed.remote.adding.next;
    ed.remote.adding.checking.insert(token, a);
    (token, dest)
}

fn unpark(ed: &mut Editor, token: u64) -> Option<Attempt> {
    ed.remote.adding.checking.remove(&token)
}

// ---- Suggestions

/// The hosts named in an ssh config (and the files it includes), without patterns.
pub fn ssh_config_hosts(path: &Path, ssh_dir: &Path, depth: usize) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, rest) = line.split_once(|c: char| c.is_whitespace() || c == '=').unwrap_or((line, ""));
        let args = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '=').split_whitespace().map(|a| a.trim_matches('"'));
        match key.to_ascii_lowercase().as_str() {
            "host" => out.extend(args.filter(|h| !h.contains(['*', '?', '!'])).map(str::to_string)),
            "include" if depth < 5 => {
                for a in args {
                    let p = crate::expand_home(a);
                    let p = if p.is_absolute() { p } else { ssh_dir.join(p) };
                    for f in glob(&p) {
                        out.extend(ssh_config_hosts(&f, ssh_dir, depth + 1));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// The files `pattern` names (`*` and `?` in its last part).
pub(crate) fn glob(pattern: &Path) -> Vec<PathBuf> {
    let name = pattern.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if !name.contains(['*', '?']) {
        return vec![pattern.to_path_buf()];
    }
    let dir = pattern.parent().unwrap_or(Path::new("."));
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.file_name().is_some_and(|n| wildcard(&name, &n.to_string_lossy()))).collect();
    out.sort();
    out
}

fn wildcard(pattern: &str, s: &str) -> bool {
    let (p, s): (Vec<char>, Vec<char>) = (pattern.chars().collect(), s.chars().collect());
    fn go(p: &[char], s: &[char]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some('*'), _) => go(&p[1..], s) || (!s.is_empty() && go(p, &s[1..])),
            (Some('?'), Some(_)) => go(&p[1..], &s[1..]),
            (Some(a), Some(b)) if a == b => go(&p[1..], &s[1..]),
            _ => false,
        }
    }
    go(&p, &s)
}

/// The machines `tailscale status --json` knows (not phones, which don't take ssh).
pub fn tailscale_peers(status: &Value) -> Vec<Suggestion> {
    let mut out = Vec::new();
    for p in status["Peer"].as_object().into_iter().flat_map(|m| m.values()) {
        let os = p["OS"].as_str().unwrap_or("");
        if matches!(os, "iOS" | "android" | "tvOS") {
            continue;
        }
        let dns = p["DNSName"].as_str().unwrap_or("").trim_end_matches('.');
        let dest = if dns.is_empty() { p["TailscaleIPs"][0].as_str().unwrap_or("").to_string() } else { dns.to_string() };
        if dest.is_empty() {
            continue;
        }
        let state = if p["Online"].as_bool() == Some(true) { "online" } else { "offline" };
        let detail = if os.is_empty() { state.to_string() } else { format!("{os} · {state}") };
        out.push(Suggestion { dest, source: "Tailscale", detail });
    }
    out.sort_by(|a, b| (a.detail.ends_with("offline"), &a.dest).cmp(&(b.detail.ends_with("offline"), &b.dest)));
    out
}

/// The instance names in `dns-sd -B _ssh._tcp` output.
pub fn dns_sd_instances(out: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in out.lines() {
        // Timestamp, Add/Rmv, flags, interface, domain, type, then the name (spaces and all).
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() >= 7 && fields[1] == "Add" && fields[5].starts_with("_ssh.") {
            let name = fields[6..].join(" ");
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names
}

/// Where `dns-sd -L` says an instance is: `host.local.:22`.
pub fn dns_sd_resolved(out: &str) -> Option<(String, u16)> {
    let at = out.split("can be reached at ").nth(1)?.split_whitespace().next()?;
    let (host, port) = at.rsplit_once(':')?;
    Some((host.trim_end_matches('.').to_string(), port.parse().ok()?))
}

/// The machines in `avahi-browse -rpt _ssh._tcp` output: (host, port).
pub fn avahi_resolved(out: &str) -> Vec<(String, u16)> {
    let mut found = Vec::new();
    for line in out.lines().filter(|l| l.starts_with('=')) {
        let f: Vec<&str> = line.split(';').collect();
        if let (Some(host), Some(Ok(port))) = (f.get(6), f.get(8).map(|p| p.parse::<u16>())) {
            let h = (host.to_string(), port);
            if !found.contains(&h) {
                found.push(h);
            }
        }
    }
    found
}

/// How ssh reaches `host` on `port`.
fn dest_of(host: &str, port: u16) -> String {
    if port == 22 { host.to_string() } else { format!("ssh://{host}:{port}") }
}

/// The machines on this network that offer ssh, asked for `seconds`.
async fn lan(seconds: f64) -> Vec<Suggestion> {
    let wait = Duration::from_secs_f64(seconds.max(0.2));
    let me = hostname();
    let mut hosts = Vec::new();
    if cfg!(target_os = "macos") {
        let names = dns_sd_instances(&run_for(&["dns-sd", "-B", "_ssh._tcp", "local."], wait).await);
        let lookups = names.iter().map(|n| async move { run_for(&["dns-sd", "-L", n, "_ssh._tcp", "local."], wait).await });
        for out in futures::future::join_all(lookups).await {
            hosts.extend(dns_sd_resolved(&out));
        }
    } else {
        let out = tokio::time::timeout(wait * 3, tokio::process::Command::new("avahi-browse").args(["-rpt", "_ssh._tcp"]).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output()).await;
        if let Ok(Ok(o)) = out {
            hosts = avahi_resolved(&String::from_utf8_lossy(&o.stdout));
        }
    }
    let mut out: Vec<Suggestion> = Vec::new();
    for (h, port) in hosts {
        let short = h.trim_end_matches(".local");
        if short.eq_ignore_ascii_case(&me) || short.eq_ignore_ascii_case(me.trim_end_matches(".local")) {
            continue;
        }
        let dest = dest_of(&h, port);
        if !out.iter().any(|s| s.dest == dest) {
            out.push(Suggestion { dest, source: "this network", detail: String::new() });
        }
    }
    out
}

/// What a program that runs until stopped printed in `wait`.
async fn run_for(argv: &[&str], wait: Duration) -> String {
    let Ok(mut child) = tokio::process::Command::new(argv[0]).args(&argv[1..]).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).kill_on_drop(true).spawn() else { return String::new() };
    let mut out = Vec::new();
    let mut stdout = child.stdout.take().unwrap();
    let _ = tokio::time::timeout(wait, tokio::io::AsyncReadExt::read_to_end(&mut stdout, &mut out)).await;
    let _ = child.kill().await;
    String::from_utf8_lossy(&out).into_owned()
}

pub(crate) fn hostname() -> String {
    nix::unistd::gethostname().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The `tailscale` command: `remote-tailscale-command`, else `tailscale` on PATH, else (macOS)
/// the one inside Tailscale.app.
pub(crate) fn tailscale_command(ed: &Editor) -> String {
    if let Some(c) = ed.var_str("remote-tailscale-command").filter(|c| !c.is_empty()) {
        return c;
    }
    let on_path = std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("tailscale").is_file()));
    let bundled = "/Applications/Tailscale.app/Contents/MacOS/Tailscale";
    if !on_path && cfg!(target_os = "macos") && Path::new(bundled).is_file() { bundled.into() } else { "tailscale".into() }
}

/// Serpis's own known_hosts.
pub fn known_hosts(ed: &Editor) -> PathBuf {
    crate::expand_home(&ed.var_str("remote-known-hosts").filter(|p| !p.is_empty()).unwrap_or_else(|| "~/.serpis/known_hosts".into()))
}

/// The hosts in a known_hosts file (not hashed ones), as ssh destinations.
pub fn known_hosts_hosts(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(first) = line.split_whitespace().next().filter(|f| !f.starts_with('#') && !f.starts_with('|') && !f.starts_with('@')) else { continue };
        let host = first.split(',').next().unwrap_or(first);
        let dest = match host.strip_prefix('[').and_then(|h| h.split_once("]:")) {
            Some((h, port)) => dest_of(h, port.parse().unwrap_or(22)),
            None => host.to_string(),
        };
        if !out.contains(&dest) {
            out.push(dest);
        }
    }
    out
}

/// The machines added or set up already, by name and destination.
fn known(ed: &mut Editor) -> Vec<(String, String)> {
    crate::remote::machines(ed).into_iter().map(|m| (m.name.clone(), crate::remote::ssh_login(&m.target).unwrap_or(m.name))).collect()
}

/// Everything to offer now, best first: ssh config, used before, Tailscale, rayfish, this network.
fn suggestions(ed: &mut Editor) -> Vec<Suggestion> {
    let mut all = Vec::new();
    let on = |ed: &Editor, v: &str| ed.var(v).and_then(Value::as_bool).unwrap_or(true);
    if on(ed, "machine-suggest-ssh-config") {
        let ssh = crate::expand_home("~/.ssh");
        all.extend(ssh_config_hosts(&ssh.join("config"), &ssh, 0).into_iter().map(|dest| Suggestion { dest, source: "ssh config", detail: String::new() }));
    }
    if let Some(Value::Object(users)) = ed.var("remote-users") {
        all.extend(users.iter().filter_map(|(h, a)| Some(Suggestion { dest: format!("{}@{h}", a.as_str()?), source: "used before", detail: String::new() })));
    }
    let trusted = std::fs::read_to_string(known_hosts(ed)).unwrap_or_default();
    all.extend(known_hosts_hosts(&trusted).into_iter().map(|dest| Suggestion { dest, source: "used before", detail: String::new() }));
    all.extend(ed.remote.adding.found.iter().filter(|s| s.source == "Tailscale").cloned());
    all.extend(crate::remote::machines(ed).into_iter().filter(|m| m.source == "rayfish").map(|m| Suggestion { dest: m.name, source: "rayfish", detail: m.detail }));
    all.extend(ed.remote.adding.found.iter().filter(|s| s.source != "Tailscale").cloned());
    let mut out: Vec<Suggestion> = Vec::new();
    for s in all {
        if !out.iter().any(|o| o.dest == s.dest) {
            out.push(s);
        }
    }
    out
}

fn candidates(ed: &mut Editor) -> Vec<Candidate> {
    let known = known(ed);
    suggestions(ed)
        .into_iter()
        .map(|s| {
            let mut detail = if s.detail.is_empty() { s.source.to_string() } else { format!("{} · {}", s.source, s.detail) };
            if let Some((name, _)) = known.iter().find(|(n, d)| *d == s.dest || *n == s.dest) {
                detail = format!("added as {name} · {detail}");
            }
            Candidate::new(s.dest).detail(detail)
        })
        .collect()
}

/// The open `machine-add` picker, again with what was found since.
pub fn refresh(ed: &mut Editor) {
    if ed.minibuffer.as_ref().is_none_or(|m| m.prompt != PROMPT) {
        return;
    }
    let cands = candidates(ed);
    ed.minibuffer_set_candidates(cands);
    let loading = ed.remote.adding.pending > 0;
    ed.minibuffer_loading(loading);
}

/// Look for machines in the background: Tailscale's, and this network's.
fn search(ed: &mut Editor) {
    if ed.remote.adding.pending > 0 {
        return;
    }
    ed.remote.adding.found.clear();
    let main = ed.main_sender();
    if ed.var("machine-suggest-tailscale").and_then(Value::as_bool).unwrap_or(true) {
        ed.remote.adding.pending += 1;
        let cmd = tailscale_command(ed);
        let main = main.clone();
        ed.rt.spawn(async move {
            let out = tokio::time::timeout(Duration::from_secs(5), tokio::process::Command::new(cmd).args(["status", "--json"]).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output()).await;
            let found = out.ok().and_then(Result::ok).and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok()).map(|s| tailscale_peers(&s)).unwrap_or_default();
            main.send(move |ed| arrived(ed, found));
        });
    }
    if ed.var("machine-suggest-lan").and_then(Value::as_bool).unwrap_or(true) {
        ed.remote.adding.pending += 1;
        let seconds = ed.var("machine-lan-seconds").and_then(Value::as_f64).unwrap_or(1.5);
        ed.rt.spawn(async move {
            let found = lan(seconds).await;
            main.send(move |ed| arrived(ed, found));
        });
    }
}

fn arrived(ed: &mut Editor, found: Vec<Suggestion>) {
    ed.remote.adding.pending = ed.remote.adding.pending.saturating_sub(1);
    ed.remote.adding.found.extend(found);
    refresh(ed);
}

// ---- Adding

/// Add a machine: pick or type where (`dest`), check it, name it (`name`), keep it. `then` gets
/// its name once it is added (the machine picker's "Add a machine…" continues with it).
pub fn add(ed: &mut Editor, dest: Option<String>, name: Option<String>, then: Option<Then>) {
    if let Some(dest) = dest {
        return check(ed, Attempt { dest, name, then });
    }
    search(ed);
    let cands = candidates(ed);
    ed.completing_read(PROMPT, cands, false, move |ed, c, typed| {
        let dest = c.map(|c| c.value).unwrap_or(typed).trim().to_string();
        if !dest.is_empty() {
            check(ed, Attempt { dest, name, then });
        }
    });
    let loading = ed.remote.adding.pending > 0;
    ed.minibuffer_loading(loading);
}

/// What stands between serpis and a login there.
#[derive(Debug, PartialEq)]
pub enum Reach {
    Ok,
    /// Its host key isn't trusted yet.
    UnknownKey,
    /// Its host key isn't the one trusted.
    ChangedKey,
    /// It doesn't take this machine's key (or the account).
    Denied,
    Other(String),
}

/// How an ssh login went, from its exit and what it said.
pub fn classify(ok: bool, stderr: &str) -> Reach {
    if ok {
        return Reach::Ok;
    }
    if stderr.contains("REMOTE HOST IDENTIFICATION HAS CHANGED") || stderr.contains("Host key for") && stderr.contains("has changed") {
        Reach::ChangedKey
    } else if stderr.contains("Host key verification failed") || stderr.contains("host key is known for") {
        Reach::UnknownKey
    } else if stderr.contains("Permission denied") || stderr.contains("Too many authentication failures") {
        Reach::Denied
    } else {
        let why = stderr.lines().map(str::trim).rfind(|l| !l.is_empty() && !l.starts_with("Warning: Permanently added")).unwrap_or("ssh failed").to_string();
        Reach::Other(why.trim_start_matches("ssh: ").to_string())
    }
}

/// The ssh command line serpis reaches `dest` with, running `command` there, with `first`
/// options ahead of serpis's (ssh keeps an option's first value).
fn ssh_argv(ed: &mut Editor, dest: &str, first: &[String], command: &str) -> Vec<String> {
    let Target::Ssh { host, ssh, ssh_args, .. } = crate::remote::ssh_target(ed, dest, None) else { unreachable!() };
    let mut argv = ssh;
    argv.extend(first.iter().cloned());
    argv.extend(ssh_args);
    argv.extend([host, "--".into(), command.into()]);
    argv
}

async fn run(argv: Vec<String>) -> (bool, String) {
    let out = tokio::time::timeout(Duration::from_secs(30), tokio::process::Command::new(&argv[0]).args(&argv[1..]).stdin(std::process::Stdio::null()).output()).await;
    match out {
        Ok(Ok(o)) => (o.status.success(), String::from_utf8_lossy(&o.stderr).into_owned()),
        Ok(Err(e)) => (false, format!("{}: {e}", argv[0])),
        Err(_) => (false, "no answer in 30 seconds".into()),
    }
}

fn busy_key(dest: &str) -> String {
    format!("machine-add:{dest}")
}

/// Log in there, and deal with what stands in the way.
fn check(ed: &mut Editor, a: Attempt) {
    let argv = ssh_argv(ed, &a.dest, &[], "exit 0");
    ed.busy_start(&busy_key(&a.dest), &format!("Reaching {}…", a.dest));
    let (token, dest) = park(ed, a);
    let main = ed.main_sender();
    ed.rt.spawn(async move {
        let (ok, stderr) = run(argv).await;
        main.send(move |ed| {
            ed.busy_stop(&busy_key(&dest));
            let Some(a) = unpark(ed, token) else { return };
            match classify(ok, &stderr) {
                Reach::Ok => named(ed, a),
                Reach::UnknownKey => fetch_key(ed, a),
                Reach::ChangedKey => ed.warn(format!(
                    "{}'s host key isn't the one trusted: it may have been reinstalled, or something is in the way. If you know why, machine-forget-host-key {} and add it again.",
                    a.dest, a.dest
                )),
                Reach::Denied => denied(ed, a),
                Reach::Other(why) => ed.warn(format!("Can't reach {}: {why}", a.dest)),
            }
        });
    });
}

/// Fetch its host key (as ssh does on first contact, through whatever the ssh config says), show
/// its fingerprint, and trust it if you say so.
fn fetch_key(ed: &mut Editor, a: Attempt) {
    let file = std::env::temp_dir().join(format!("serpis-hostkey-{}", uuid::Uuid::now_v7().simple()));
    let first = ["-o", "StrictHostKeyChecking=accept-new", "-o"].map(String::from).into_iter().chain([format!("UserKnownHostsFile=\"{}\"", file.display())]).collect::<Vec<_>>();
    let argv = ssh_argv(ed, &a.dest, &first, "exit 0");
    let keygen = ed.var_str("remote-ssh-keygen-command").filter(|c| !c.is_empty()).unwrap_or_else(|| "ssh-keygen".into());
    ed.busy_start(&busy_key(&a.dest), &format!("Asking {} for its host key…", a.dest));
    let (token, dest) = park(ed, a);
    let main = ed.main_sender();
    ed.rt.spawn(async move {
        let (_, stderr) = run(argv).await;
        let lines = std::fs::read_to_string(&file).unwrap_or_default();
        let prints = tokio::process::Command::new(keygen).arg("-lf").arg(&file).output().await.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
        let _ = std::fs::remove_file(&file);
        main.send(move |ed| {
            ed.busy_stop(&busy_key(&dest));
            let Some(a) = unpark(ed, token) else { return };
            if lines.trim().is_empty() {
                let why = match classify(false, &stderr) {
                    Reach::Other(w) => w,
                    _ => "it sent no host key".into(),
                };
                return ed.warn(format!("Can't reach {}: {why}", a.dest));
            }
            // "256 SHA256:… host (ED25519)": the fingerprint and the key's type.
            let print = prints.lines().next().map(|l| {
                let f: Vec<&str> = l.split_whitespace().collect();
                format!("{} {}", f.get(1).unwrap_or(&""), f.last().unwrap_or(&""))
            });
            let Some(print) = print else { return ed.warn(format!("Can't read {}'s host key", a.dest)) };
            ed.y_or_n(&format!("{}'s host key is {print}. Trust it?", a.dest), move |ed, yes| {
                if !yes {
                    return ed.message(format!("{} isn't added: its host key wasn't trusted", a.dest));
                }
                match trust(&known_hosts(ed), &lines) {
                    Ok(()) => check(ed, a),
                    Err(e) => ed.warn(format!("Can't keep {}'s host key: {e}", a.dest)),
                }
            });
        });
    });
}

/// Add host key lines to serpis's known_hosts.
fn trust(file: &Path, lines: &str) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(file)?;
    for l in lines.lines().filter(|l| !l.trim().is_empty()) {
        writeln!(f, "{l}")?;
    }
    Ok(())
}

/// This machine's public keys, as ssh offers them.
fn public_keys() -> Vec<PathBuf> {
    let ssh = crate::expand_home("~/.ssh");
    ["id_ed25519.pub", "id_ecdsa.pub", "id_rsa.pub"].iter().map(|k| ssh.join(k)).filter(|p| p.is_file()).collect()
}

/// It doesn't take this machine's key: copy it there, or try another account.
fn denied(ed: &mut Editor, a: Attempt) {
    let has_key = !public_keys().is_empty();
    let mut cands = vec![
        Candidate::new(if has_key { "Copy my key there" } else { "Make a key and copy it there" }).value("copy").detail("opens a terminal running ssh-copy-id: type the password there once"),
        Candidate::new("Log in as another account").value("account"),
    ];
    if has_key {
        cands.push(Candidate::new("Copy my public key to the clipboard").value("clipboard").detail("to add to ~/.ssh/authorized_keys there yourself"));
    }
    let prompt = format!("{} doesn't take your key: ", a.dest);
    ed.completing_read(&prompt, cands, true, move |ed, c, _| match c.map(|c| c.value).as_deref() {
        Some("copy") => copy_key(ed, a, has_key),
        Some("account") => {
            let host = a.dest.rsplit('@').next().unwrap_or(&a.dest).to_string();
            let Attempt { name, then, .. } = a;
            ed.read_string(&format!("Account on {host}: "), "", move |ed, account| {
                let account = account.trim();
                if !account.is_empty() {
                    check(ed, Attempt { dest: format!("{account}@{host}"), name, then });
                }
            });
        }
        Some("clipboard") => {
            let key = public_keys().first().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
            ed.copy_to_clipboard(key.trim());
            ed.message(format!("Your public key is on the clipboard: add it to ~/.ssh/authorized_keys on {}, then machine-add it again", a.dest));
        }
        _ => {}
    });
}

/// Copy this machine's key there with `ssh-copy-id`, in a terminal (it asks for the password),
/// making one first if there is none; check again when it ends.
fn copy_key(ed: &mut Editor, a: Attempt, has_key: bool) {
    let copy = ed.var_str("remote-ssh-copy-id-command").filter(|c| !c.is_empty()).unwrap_or_else(|| "ssh-copy-id".into());
    let file = known_hosts(ed);
    let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    let login = match crate::remote::ssh_target(ed, &a.dest, None) {
        Target::Ssh { host, .. } => host,
        _ => a.dest.clone(),
    };
    let mut cmd = format!("{copy} -o {} -o StrictHostKeyChecking=yes {}", quote(&format!("UserKnownHostsFile={}", file.display())), quote(&login));
    if !has_key {
        cmd = format!("ssh-keygen -t ed25519 -f ~/.ssh/id_ed25519 && {cmd}");
    }
    match ed.create_buffer("terminal", Some(&format!("ssh-copy-id {}", a.dest)), &json!({ "command": cmd })) {
        Ok(id) => {
            ed.show_buffer(id);
            ed.remote.adding.copying = Some((id, a));
        }
        Err(e) => ed.warn(format!("Can't open a terminal for ssh-copy-id: {e}")),
    }
}

/// Every tick: a key copied there (its terminal ended) is checked again.
pub fn tick(ed: &mut Editor) {
    let Some((id, _)) = &ed.remote.adding.copying else { return };
    let id = *id;
    match ed.with_buffer_as::<crate::buffers::terminal::TerminalBuffer, _>(id, |t, _| t.exited) {
        Some(false) => {}
        Some(true) => {
            let (_, a) = ed.remote.adding.copying.take().unwrap();
            check(ed, a);
        }
        // Closed before it ended: given up.
        None => ed.remote.adding.copying = None,
    }
}

/// Its name here (from the host: `box` for `me@box.tail1234.ts.net`).
pub fn default_name(dest: &str) -> String {
    let host = dest.trim_start_matches("ssh://");
    let host = host.rsplit('@').next().unwrap_or(host);
    let host = match host.strip_prefix('[') {
        Some(h) => h.split(']').next().unwrap_or(h),
        None => host.rsplit_once(':').filter(|(_, p)| p.parse::<u16>().is_ok()).map_or(host, |(h, _)| h),
    };
    let ip = host.parse::<std::net::IpAddr>().is_ok();
    if ip { host.to_string() } else { host.split('.').next().unwrap_or(host).to_string() }
}

/// Logged in: name it (unless named already), keep it, and connect (which installs the node).
fn named(ed: &mut Editor, a: Attempt) {
    let Attempt { dest, name, then } = a;
    let default = default_name(&dest);
    let keep = move |ed: &mut Editor, name: String| {
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        if name != dest && crate::remote::machines(ed).iter().any(|m| m.name == name && m.source != "rayfish") && !added(ed).iter().any(|(n, d)| *n == name && *d == dest) {
            return ed.warn(format!("There is a machine called {name} already: machine-add again with another name"));
        }
        save(ed, &name, &dest);
        ed.run_hook("machine-added", &json!({"name": name, "host": dest}));
        ed.message(format!("Added {name} ({dest}): serpis sets up its node there"));
        match then {
            Some(f) => f(ed, name),
            None => crate::remote::with_client(ed, &name, |_, _| {}),
        }
    };
    match name {
        Some(n) => keep(ed, n),
        None => ed.read_string("Call it: ", &default, keep),
    }
}

/// The machines added: (name, destination).
pub fn added(ed: &Editor) -> Vec<(String, String)> {
    let Some(Value::Array(list)) = ed.var("remote-machines") else { return Vec::new() };
    list.iter().filter_map(|m| Some((m["name"].as_str()?.to_string(), m["host"].as_str()?.to_string()))).collect()
}

fn set_added(ed: &mut Editor, list: Vec<(String, String)>) {
    let v = Value::Array(list.into_iter().map(|(name, host)| json!({"name": name, "host": host})).collect());
    ed.set_var("remote-machines", v.clone());
    ed.persist_state("remote-machines", v);
}

fn save(ed: &mut Editor, name: &str, dest: &str) {
    let mut list: Vec<(String, String)> = added(ed).into_iter().filter(|(n, _)| n != name).collect();
    list.push((name.to_string(), dest.to_string()));
    set_added(ed, list);
}

/// Pick one of the machines added, then call `f` with its name.
fn pick_added(ed: &mut Editor, prompt: &str, f: impl FnOnce(&mut Editor, String) + 'static) {
    let list = added(ed);
    if list.is_empty() {
        return ed.message("No machines added (machine-add adds one)");
    }
    let cands = list.into_iter().map(|(n, d)| Candidate::new(n).detail(d)).collect();
    ed.completing_read(prompt, cands, true, move |ed, c, _| {
        if let Some(c) = c {
            f(ed, c.value);
        }
    });
}

pub fn install(ed: &mut Editor) {
    let o = "machines";
    ed.defvar("remote-machines", Editor::persisted_state("remote-machines").unwrap_or_else(|| json!([])), "Machines added with machine-add: {name, host} (host: an ssh destination). Kept between runs; remote-hosts in your config works beside them.");
    ed.defvar("remote-known-hosts", json!("~/.serpis/known_hosts"), "The host keys serpis trusts (machine-add asks before adding one). ssh's own known_hosts is read too, never written (remote-ssh-known-hosts).");
    ed.defvar("remote-ssh-known-hosts", json!(true), "Also trust the host keys ssh trusts (~/.ssh/known_hosts), without ever writing there.");
    ed.defvar("remote-tailscale-command", json!(""), "The tailscale command machine-add asks for peers (\"\": tailscale on PATH, else the one in /Applications/Tailscale.app).");
    ed.defvar("remote-ssh-copy-id-command", json!("ssh-copy-id"), "What machine-add copies your key to a machine with (in a terminal, where the password is typed).");
    ed.defvar("remote-ssh-keygen-command", json!("ssh-keygen"), "What machine-add reads host key fingerprints with.");
    ed.defvar("machine-suggest-ssh-config", json!(true), "machine-add suggests the hosts in ~/.ssh/config (and the files it includes).");
    ed.defvar("machine-suggest-tailscale", json!(true), "machine-add suggests your Tailscale peers (not phones).");
    ed.defvar("machine-suggest-lan", json!(true), "machine-add suggests machines on this network that offer ssh (Bonjour: dns-sd on macOS, avahi-browse on Linux).");
    ed.defvar("machine-lan-seconds", json!(1.5), "How long machine-add listens for machines on this network.");

    ed.define_command("machine-add", "Add a machine to run chats and terminals on: pick one serpis found (ssh config, Tailscale, rayfish, this network, used before) or type an ssh destination (box, me@10.0.0.5, ssh://me@box:2222). serpis checks it can log in, asks to trust its host key, offers to copy your key there, then names it and installs its node (params: destination, name). Runs the machine-added hook.", o, |ed, a| {
        add(ed, a.str(0).map(str::to_string), a.str(1).map(str::to_string), None);
        Ok(())
    });
    ed.define_command("machine-remove", "Forget a machine added with machine-add (param: its name): it leaves this desktop's list, and this window's connection to it closes. Nothing on the machine changes: its node keeps running, with its chats, terminals and files, for anyone else using it. Its host key stays trusted (machine-forget-host-key).", o, |ed, a| {
        let remove = |ed: &mut Editor, name: String| {
            let list: Vec<_> = added(ed).into_iter().filter(|(n, _)| *n != name).collect();
            set_added(ed, list);
            if crate::remote::disconnect(ed, &name) {
                ed.message(format!("Removed {name} (it keeps running there; nothing on it changed)"));
            } else {
                ed.message(format!("Removed {name}: its chats and terminals shown here stay connected until you close them (nothing on it changed)"));
            }
        };
        match a.str(0) {
            Some(n) => remove(ed, n.to_string()),
            None => pick_added(ed, "Remove machine: ", remove),
        }
        Ok(())
    });
    ed.define_command("machine-rename", "Call a machine added with machine-add something else (params: name, new name).", o, |ed, a| {
        let rename = |ed: &mut Editor, old: String, new: String| {
            let new = new.trim().to_string();
            if new.is_empty() || new == old {
                return;
            }
            if crate::remote::machines(ed).iter().any(|m| m.name == new) {
                return ed.warn(format!("There is a machine called {new} already"));
            }
            let list = added(ed).into_iter().map(|(n, d)| if n == old { (new.clone(), d) } else { (n, d) }).collect();
            set_added(ed, list);
            ed.message(format!("{old} is {new} now"));
        };
        match (a.str(0).map(str::to_string), a.str(1).map(str::to_string)) {
            (Some(old), Some(new)) => rename(ed, old, new),
            (Some(old), None) => ed.read_string(&format!("Call {old}: "), &old.clone(), move |ed, new| rename(ed, old, new)),
            _ => pick_added(ed, "Rename machine: ", move |ed, old| ed.read_string(&format!("Call {old}: "), &old.clone(), move |ed, new| rename(ed, old, new))),
        }
        Ok(())
    });
    ed.define_command("machine-forget-host-key", "Stop trusting a machine's host key (in remote-known-hosts; ssh's own is untouched), e.g. after it was reinstalled: machine-add asks again (param: the machine or host).", o, |ed, a| {
        let forget = |ed: &mut Editor, which: String| {
            let dest = added(ed).into_iter().find(|(n, _)| *n == which).map_or(which.clone(), |(_, d)| d);
            let host = dest.trim_start_matches("ssh://");
            let host = host.rsplit('@').next().unwrap_or(host).to_string();
            let host = match host.rsplit_once(':').filter(|(_, p)| p.parse::<u16>().is_ok()) {
                Some((h, p)) => format!("[{h}]:{p}"),
                None => host,
            };
            let file = known_hosts(ed);
            let keygen = ed.var_str("remote-ssh-keygen-command").filter(|c| !c.is_empty()).unwrap_or_else(|| "ssh-keygen".into());
            let out = std::process::Command::new(keygen).arg("-R").arg(&host).arg("-f").arg(&file).output();
            let _ = std::fs::remove_file(file.with_extension("old"));
            match out {
                Ok(o) if o.status.success() => ed.message(format!("{host}'s host key isn't trusted any more")),
                Ok(o) => ed.warn(format!("Can't forget {host}'s host key: {}", String::from_utf8_lossy(&o.stderr).trim())),
                Err(e) => ed.warn(format!("Can't forget {host}'s host key: {e}")),
            }
        };
        match a.str(0) {
            Some(w) => forget(ed, w.to_string()),
            None => {
                let cands = added(ed).into_iter().map(|(n, d)| Candidate::new(n).detail(d)).collect();
                ed.completing_read("Forget the host key of: ", cands, false, move |ed, c, typed| {
                    let w = c.map(|c| c.value).unwrap_or(typed);
                    if !w.trim().is_empty() {
                        forget(ed, w.trim().to_string());
                    }
                });
            }
        }
        Ok(())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_config_hosts_and_includes() {
        let dir = std::env::temp_dir().join(format!("sc-{}", uuid::Uuid::now_v7().simple()));
        std::fs::create_dir_all(dir.join("conf.d")).unwrap();
        std::fs::write(dir.join("config"), "# mine\nHost gpu-box lab\n  HostName 10.0.0.5\nHost *.internal !bad\nHost=eq-box\nInclude conf.d/*.conf\nMatch host x\n").unwrap();
        std::fs::write(dir.join("conf.d/work.conf"), "Host \"work\"\n").unwrap();
        std::fs::write(dir.join("conf.d/skip.txt"), "Host nope\n").unwrap();
        assert_eq!(ssh_config_hosts(&dir.join("config"), &dir, 0), ["gpu-box", "lab", "eq-box", "work"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tailscale_peers_but_not_phones() {
        let s = json!({"Self": {"DNSName": "me.ts.net."}, "Peer": {
            "a": {"DNSName": "box.tail1.ts.net.", "OS": "linux", "Online": true, "TailscaleIPs": ["100.1.1.1"]},
            "b": {"DNSName": "", "OS": "linux", "Online": false, "TailscaleIPs": ["100.1.1.2"]},
            "c": {"DNSName": "phone.tail1.ts.net.", "OS": "iOS", "Online": true}}});
        let p = tailscale_peers(&s);
        assert_eq!(p.iter().map(|s| (s.dest.as_str(), s.detail.as_str())).collect::<Vec<_>>(), [("box.tail1.ts.net", "linux · online"), ("100.1.1.2", "linux · offline")]);
    }

    #[test]
    fn bonjour_output() {
        let browse = "Browsing for _ssh._tcp.local.\nDATE: ---Mon 26 Oct 2026---\n12:00:00.000  ...STARTING...\nTimestamp     A/R    Flags  if Domain               Service Type         Instance Name\n12:00:00.100  Add        3  14 local.               _ssh._tcp.           box\n12:00:00.100  Add        2  14 local.               _ssh._tcp.           Alice’s Mac mini\n";
        assert_eq!(dns_sd_instances(browse), ["box", "Alice’s Mac mini"]);
        let look = "Lookup box._ssh._tcp.local.\n12:00:00.200  box._ssh._tcp.local. can be reached at box.local.:22 (interface 14)\n";
        assert_eq!(dns_sd_resolved(look), Some(("box.local".into(), 22)));
        // (As avahi-browse prints them: escaped names, the service type in words.)
        let avahi = "+;wlan0;IPv4;my\\032Mac;SSH Remote Terminal;local\n=;wlan0;IPv4;my\\032Mac;SSH Remote Terminal;local;my-Mac.local;192.168.1.128;22;\n=;wlan0;IPv4;box;SSH Remote Terminal;local;box.local;192.168.1.9;2222;\n";
        assert_eq!(avahi_resolved(avahi), [("my-Mac.local".to_string(), 22), ("box.local".to_string(), 2222)]);
        assert_eq!(dest_of("box.local", 2222), "ssh://box.local:2222");
    }

    #[test]
    fn known_hosts_lists_plain_hosts() {
        let text = "box.local ssh-ed25519 AAAA\n[10.0.0.5]:2222,10.0.0.6 ssh-ed25519 AAAA\n|1|hash= ssh-ed25519 AAAA\n@revoked x ssh-ed25519 AAAA\n";
        assert_eq!(known_hosts_hosts(text), ["box.local", "ssh://10.0.0.5:2222"]);
    }

    #[test]
    fn what_ssh_said() {
        assert_eq!(classify(true, ""), Reach::Ok);
        assert_eq!(classify(false, "No ED25519 host key is known for box and you have requested strict checking.\nHost key verification failed.\n"), Reach::UnknownKey);
        assert_eq!(classify(false, "@@@@\n@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\nHost key verification failed.\n"), Reach::ChangedKey);
        assert_eq!(classify(false, "me@box: Permission denied (publickey,password).\n"), Reach::Denied);
        assert_eq!(classify(false, "ssh: Could not resolve hostname nobox: nodename nor servname provided, or not known\n"), Reach::Other("Could not resolve hostname nobox: nodename nor servname provided, or not known".into()));
    }

    #[test]
    fn names_from_destinations() {
        assert_eq!(default_name("me@lab.tail1.ts.net"), "lab");
        assert_eq!(default_name("ssh://me@box.local:2222"), "box");
        assert_eq!(default_name("10.0.0.5"), "10.0.0.5");
        assert_eq!(default_name("gpu-box"), "gpu-box");
        assert_eq!(default_name("ec2-1-2-3-4.compute.amazonaws.com"), "ec2-1-2-3-4");
    }
}
