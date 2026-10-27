//! Pairing a phone (on the desktop, where machines are set up): `phone-pair` shows a code the
//! Serpis app scans (the camera opens it as a `serpis://pair#…` link). The code carries the
//! machines shared (address, port, account) with the host keys trusted here, so the phone
//! trusts exactly those, and a pairing key each machine lets in for a few minutes, only to run
//! `serpis-node pair`. With it the phone proposes its own key; the machine's node tells this
//! window, which asks you, and once you say yes adds the phone's key on every machine shared, then
//! ends the pairing everywhere. Nothing secret stays in the code after that.
//!
//! This machine is shared the same way, so the phone drives the chats this window shows (they
//! run in this machine's node, which the phone reaches over ssh like any other machine's). What
//! it can't take from ssh is an address: what this machine calls itself is no use off it, so
//! serpis offers the names it has on Tailscale and on this network (`phone-pair-local-address`
//! keeps the one you pick), reads its host keys from the files sshd offers them from, and says
//! so when nothing answers ssh here.

use std::time::{Duration, Instant};

use base64::Engine;
use egui::{Align2, Color32, FontId, Rect, Vec2};
use serde_json::{Value, json};
use serpis_node::client::Target;
use serpis_proto::wire::Request;

use crate::buffer::{Buffer, BufferId, Modeline};
use crate::editor::Editor;
use crate::impl_any;
use crate::minibuffer::Candidate;
use crate::remote::LOCAL;

/// The address picker's prompt (to recognise it as addresses arrive).
const ADDRESS_PROMPT: &str = "The phone reaches this machine at: ";

/// The phone being paired.
pub struct Active {
    id: String,
    /// The machines that let the pairing key in.
    machines: Vec<String>,
    until: Instant,
    buffer: BufferId,
    /// A phone's key was let in or turned down.
    answered: bool,
}

/// A machine as the phone reaches it.
#[derive(Clone, Debug, PartialEq)]
pub struct Shared {
    /// What it is called here, to ask its node things ([`LOCAL`] for this machine).
    pub machine: String,
    /// What the phone calls it (this machine's hostname, for itself).
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    /// Its host keys, `type base64`.
    pub keys: Vec<String>,
}

/// What `ssh -G` says ssh would do: (host name, port, account, host key alias).
pub fn ssh_g(out: &str) -> (String, u16, String, Option<String>) {
    let (mut host, mut port, mut user, mut alias) = (String::new(), 22, String::new(), None);
    for line in out.lines() {
        let Some((k, v)) = line.split_once(' ') else { continue };
        match k {
            "hostname" => host = v.trim().to_string(),
            "port" => port = v.trim().parse().unwrap_or(22),
            "user" => user = v.trim().to_string(),
            "hostkeyalias" if v.trim() != "none" => alias = Some(v.trim().to_string()),
            _ => {}
        }
    }
    (host, port, user, alias)
}

/// The keys (`type base64`) in `ssh-keygen -F` output.
pub fn found_keys(out: &str) -> Vec<String> {
    let mut keys = Vec::new();
    for line in out.lines().filter(|l| !l.starts_with('#') && !l.starts_with('@')) {
        let f: Vec<&str> = line.split_whitespace().collect();
        if let Some(k) = f.windows(2).find(|w| w[0].starts_with("ssh-") || w[0].starts_with("ecdsa-") || w[0].starts_with("sk-")).map(|w| format!("{} {}", w[0], w[1]))
            && !keys.contains(&k)
        {
            keys.push(k);
        }
    }
    keys
}

/// The link the code holds: the machines, the pairing key (its OpenSSH body), when it ends.
pub fn link(id: &str, private: &str, until_unix: u64, machines: &[Shared]) -> String {
    let body: String = private.lines().filter(|l| !l.starts_with("-----")).collect();
    let m: Vec<Value> = machines.iter().map(|s| json!({"n": s.name, "h": s.host, "p": s.port, "u": s.user, "k": s.keys})).collect();
    let payload = json!({"v": 1, "p": id, "k": body, "x": until_unix, "m": m});
    format!("serpis://pair#{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.to_string()))
}

/// How the phone reaches `machine`, and the host keys trusted for it here.
fn resolve(ed: &mut Editor, machine: &str) -> Option<(Vec<String>, Vec<std::path::PathBuf>)> {
    let target = crate::remote::machines(ed).into_iter().find(|m| m.name == machine)?.target;
    let Target::Ssh { host, ssh, ssh_args, .. } = target else { return None };
    let mut argv = ssh;
    argv.extend(ssh_args);
    argv.extend(["-G".into(), host]);
    let mut files = vec![crate::machines::known_hosts(ed)];
    if ed.var("remote-ssh-known-hosts").and_then(Value::as_bool).unwrap_or(true) {
        files.push(crate::expand_home("~/.ssh/known_hosts"));
    }
    Some((argv, files))
}

fn keygen(ed: &Editor) -> String {
    ed.var_str("remote-ssh-keygen-command").filter(|c| !c.is_empty()).unwrap_or_else(|| "ssh-keygen".into())
}

/// Ask `ssh -G` and the known_hosts files, off the window's thread.
fn look_up(argv: Vec<String>, files: Vec<std::path::PathBuf>, keygen: &str, name: String) -> Result<Shared, String> {
    let out = std::process::Command::new(&argv[0]).args(&argv[1..]).stdin(std::process::Stdio::null()).output().map_err(|e| format!("{}: {e}", argv[0]))?;
    let (host, port, user, alias) = ssh_g(&String::from_utf8_lossy(&out.stdout));
    if host.is_empty() {
        return Err(format!("ssh can't say how it reaches {name}"));
    }
    let known_as = alias.unwrap_or_else(|| if port == 22 { host.clone() } else { format!("[{host}]:{port}") });
    let mut keys = Vec::new();
    for f in files.iter().filter(|f| f.is_file()) {
        let out = std::process::Command::new(keygen).arg("-F").arg(&known_as).arg("-f").arg(f).output().map_err(|e| format!("{keygen}: {e}"))?;
        for k in found_keys(&String::from_utf8_lossy(&out.stdout)) {
            if !keys.contains(&k) {
                keys.push(k);
            }
        }
    }
    if keys.is_empty() {
        return Err(format!("no host key is trusted for {name} ({known_as}): connect to it once first"));
    }
    Ok(Shared { machine: name.clone(), name, host, port, user, keys })
}

// ---- This machine

/// What a machine's name shows as here (`local` is this machine).
fn shown(machine: &str) -> &str {
    if machine == LOCAL { "this machine" } else { machine }
}

/// The address the phone reaches this machine at, if one was picked.
fn local_address(ed: &Editor) -> Option<String> {
    ed.var_str("phone-pair-local-address").map(|a| a.trim().to_string()).filter(|a| !a.is_empty())
}

/// An ssh destination in parts: (account, host, port).
pub fn dest_parts(dest: &str) -> (Option<String>, String, Option<u16>) {
    let rest = dest.trim().trim_start_matches("ssh://");
    let (user, rest) = match rest.rsplit_once('@') {
        Some((u, r)) => (Some(u.to_string()).filter(|u| !u.is_empty()), r),
        None => (None, rest),
    };
    // `[::1]:2222` as well as `box:2222`; a trailing part that isn't a port is part of the host.
    let (host, port) = match rest.strip_prefix('[').and_then(|r| r.split_once(']')) {
        Some((h, tail)) => (h.to_string(), tail.trim_start_matches(':').parse().ok()),
        None => match rest.rsplit_once(':') {
            // A bare IPv6 address has colons of its own: only `host:port` carries a port.
            Some((h, p)) if !h.contains(':') && p.parse::<u16>().is_ok() => (h.to_string(), p.parse().ok()),
            _ => (rest.to_string(), None),
        },
    };
    (user, host, port)
}

/// What the phone calls this machine: its hostname, short (`Mac` for `Mac.local`).
fn local_name() -> String {
    let host = crate::machines::hostname();
    let short = host.trim_end_matches(".local").split('.').next().unwrap_or("").trim();
    if short.is_empty() || short.eq_ignore_ascii_case("localhost") { "this machine".into() } else { short.to_string() }
}

/// The account the phone logs in to this machine as.
fn local_user() -> String {
    std::env::var("USER").or_else(|_| std::env::var("LOGNAME")).unwrap_or_default()
}

/// This machine's ssh host keys, read from the files sshd offers them from (`type base64`).
/// Its own keys, unlike another machine's, are there to read: nothing needs to have connected.
fn local_keys(pattern: &str) -> Vec<String> {
    let mut keys = Vec::new();
    for f in crate::machines::glob(&crate::expand_home(pattern)) {
        for line in std::fs::read_to_string(&f).unwrap_or_default().lines() {
            let f: Vec<&str> = line.split_whitespace().take(2).collect();
            if let [t, k] = f[..]
                && (t.starts_with("ssh-") || t.starts_with("ecdsa-") || t.starts_with("sk-"))
            {
                let key = format!("{t} {k}");
                if !keys.contains(&key) {
                    keys.push(key);
                }
            }
        }
    }
    keys
}

/// The port in an sshd_config (its first `Port`), if it says one.
pub fn sshd_port(config: &str) -> Option<u16> {
    config.lines().map(str::trim).find_map(|l| l.strip_prefix("Port ")?.trim().parse().ok())
}

/// How the phone reaches this machine: the address you gave, the port sshd listens on, your
/// account, and this machine's host keys.
fn local_shared(ed: &mut Editor) -> Result<Shared, String> {
    let address = local_address(ed).ok_or("no address for this machine yet")?;
    let (user, host, port) = dest_parts(&address);
    if host.is_empty() {
        return Err(format!("{address} isn't an address the phone can use"));
    }
    let pattern = ed.var_str("phone-pair-local-host-keys").filter(|p| !p.is_empty()).unwrap_or_else(|| "/etc/ssh/ssh_host_*_key.pub".into());
    let keys = local_keys(&pattern);
    if keys.is_empty() {
        return Err(format!("this machine's ssh host keys aren't there to read ({pattern})"));
    }
    let port = port.unwrap_or_else(|| {
        let config = ed.var_str("phone-pair-sshd-config").filter(|p| !p.is_empty()).unwrap_or_else(|| "/etc/ssh/sshd_config".into());
        sshd_port(&std::fs::read_to_string(crate::expand_home(&config)).unwrap_or_default()).unwrap_or(22)
    });
    let user = user.unwrap_or_else(local_user);
    if user.is_empty() {
        return Err("who the phone logs in as here isn't known: give the address as you@host".into());
    }
    Ok(Shared { machine: LOCAL.into(), name: local_name(), host, port, user, keys })
}

/// Nothing answers ssh here: the phone can't log in until something does.
fn sshd_note(port: u16) -> Option<String> {
    let at = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    if std::net::TcpStream::connect_timeout(&at, Duration::from_millis(500)).is_ok() {
        return None;
    }
    let how = if cfg!(target_os = "macos") { "turn Remote Login on (System Settings › General › Sharing)" } else { "start sshd" };
    Some(format!("Nothing answers ssh on port {port} here: {how}, or the phone can't log in."))
}

/// A new pairing key: (private key file text, public key).
fn new_key(keygen: &str) -> Result<(String, String), String> {
    let dir = std::env::temp_dir().join(format!("serpis-pair-{}", uuid::Uuid::now_v7().simple()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = dir.join("key");
    let made = std::process::Command::new(keygen).args(["-q", "-t", "ed25519", "-N", "", "-C", "serpis-pairing", "-f"]).arg(&file).stdin(std::process::Stdio::null()).output();
    let read = (std::fs::read_to_string(&file), std::fs::read_to_string(file.with_extension("pub")));
    let _ = std::fs::remove_dir_all(&dir);
    match (made, read) {
        (Ok(o), (Ok(private), Ok(public))) if o.status.success() => {
            let public = public.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
            Ok((private, public))
        }
        (Ok(o), _) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
        (Err(e), _) => Err(format!("{keygen}: {e}")),
    }
}

/// The addresses `tailscale status --json` gives this machine.
pub fn tailscale_addresses(status: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let me = &status["Self"];
    let dns = me["DNSName"].as_str().unwrap_or("").trim_end_matches('.');
    if !dns.is_empty() {
        out.push((dns.to_string(), "Tailscale · wherever the phone is, with Tailscale on there".into()));
    }
    if let Some(ip) = me["TailscaleIPs"][0].as_str().filter(|ip| !ip.is_empty()) {
        out.push((ip.to_string(), "Tailscale · its address there".into()));
    }
    out
}

/// This machine's address on this network: the one it would talk to the world from (nothing is
/// sent; a UDP socket only picks the interface).
fn lan_address() -> Option<String> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then(|| ip.to_string())
}

/// Addresses this machine has without asking anything: its Bonjour name and its address here.
fn nearby() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let host = crate::machines::hostname();
    let host = host.trim();
    if !host.is_empty() && !host.contains(' ') && !host.eq_ignore_ascii_case("localhost") {
        let name = if host.contains('.') { host.to_string() } else { format!("{host}.local") };
        out.push((name, "this network · while the phone is on the same Wi-Fi".into()));
    }
    if let Some(ip) = lan_address() {
        out.push((ip, "this network · its address right now".into()));
    }
    out
}

fn address_candidates(ed: &mut Editor) -> Vec<Candidate> {
    let mut out = Vec::new();
    if let Some(a) = local_address(ed) {
        out.push(Candidate::new(a).detail("the one you picked before"));
    }
    for (a, detail) in ed.remote.pair_addresses.clone() {
        if !out.iter().any(|c| c.value == a) {
            out.push(Candidate::new(a).detail(detail));
        }
    }
    out
}

/// The open address picker, again with what was found since.
fn address_arrived(ed: &mut Editor, found: Vec<(String, String)>) {
    ed.remote.pair_addresses.extend(found);
    if ed.minibuffer.as_ref().is_none_or(|m| m.prompt != ADDRESS_PROMPT) {
        return;
    }
    let cands = address_candidates(ed);
    ed.minibuffer_set_candidates(cands);
    ed.minibuffer_loading(false);
}

/// Ask what address the phone should use for this machine, then share `machines` with it. Only
/// this machine needs asking: for any other, ssh says.
fn ask_address(ed: &mut Editor, machines: Vec<String>) {
    ed.remote.pair_addresses = nearby();
    let cmd = crate::machines::tailscale_command(ed);
    let main = ed.main_sender();
    ed.rt.spawn(async move {
        let out = tokio::time::timeout(Duration::from_secs(5), tokio::process::Command::new(cmd).args(["status", "--json"]).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output()).await;
        let found = out.ok().and_then(Result::ok).and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok()).map(|s| tailscale_addresses(&s)).unwrap_or_default();
        main.send(move |ed| address_arrived(ed, found));
    });
    let cands = address_candidates(ed);
    ed.completing_read(ADDRESS_PROMPT, cands, false, move |ed, c, typed| {
        let address = c.map(|c| c.value).unwrap_or(typed).trim().to_string();
        if address.is_empty() {
            return;
        }
        ed.set_var("phone-pair-local-address", json!(address));
        ed.persist_state("phone-pair-local-address", json!(address));
        start(ed, machines);
    });
    ed.minibuffer_loading(true);
}

/// Pair a phone with `machines` (None: ask which; "local" is this machine).
pub fn pair(ed: &mut Editor, machines: Option<Vec<String>>) {
    if let Some(m) = machines {
        return start(ed, m);
    }
    // Machines the phone can reach by ssh (not rayfish's: the phone isn't on rayfish), and this
    // machine, whose node runs the chats this window shows.
    let mut all = vec![LOCAL.to_string()];
    all.extend(crate::remote::machines(ed).into_iter().filter(|m| m.source != "rayfish" && matches!(m.target, Target::Ssh { .. })).map(|m| m.name));
    let detail = match local_address(ed) {
        Some(a) => format!("the desktop serpis runs on, at {a}"),
        None => "the desktop serpis runs on · serpis asks for an address".into(),
    };
    let mut cands = vec![Candidate::new("this machine").value(LOCAL).detail(detail)];
    if all.len() > 1 {
        cands.push(Candidate::new(format!("All {} machines", all.len())).value("\u{1}all").detail(all.iter().map(|m| shown(m)).collect::<Vec<_>>().join(", ")));
    }
    cands.extend(all.iter().filter(|m| *m != LOCAL).map(|m| Candidate::new(m.clone())));
    ed.completing_read("Share with the phone: ", cands, true, move |ed, c, _| match c.map(|c| c.value) {
        Some(v) if v == "\u{1}all" => start(ed, all),
        Some(m) => start(ed, vec![m]),
        None => {}
    });
}

fn start(ed: &mut Editor, machines: Vec<String>) {
    // This machine can only be shared once it has an address the phone can use.
    if machines.iter().any(|m| m == LOCAL) && local_address(ed).is_none() {
        return ask_address(ed, machines);
    }
    if let Some(a) = ed.remote.pairing.take() {
        end(ed, a, "Ended: another pairing started");
    }
    let minutes = ed.var("phone-pair-minutes").and_then(Value::as_u64).unwrap_or(10).clamp(1, 60) as u32;
    let keygen = keygen(ed);
    let here: Vec<Result<Shared, String>> = machines.iter().filter(|m| *m == LOCAL).map(|_| local_shared(ed).map_err(|e| format!("this machine: {e}"))).collect();
    let lookups: Vec<_> = machines.iter().filter(|m| *m != LOCAL).filter_map(|m| resolve(ed, m).map(|(argv, files)| (m.clone(), argv, files))).collect();
    ed.busy_start("phone-pair", "Getting the code ready…");
    let main = ed.main_sender();
    ed.rt.spawn_blocking(move || {
        let mut found = here;
        found.extend(lookups.into_iter().map(|(name, argv, files)| look_up(argv, files, &keygen, name)));
        // Sharing this machine is no use while its sshd is off: the code says so.
        let advice: Vec<String> = found.iter().flatten().filter(|s| s.machine == LOCAL).filter_map(|s| sshd_note(s.port)).collect();
        let key = new_key(&keygen);
        main.send(move |ed| started(ed, found, advice, key, minutes));
    });
}

/// Every machine shared lets the pairing key in; then the code shows.
fn started(ed: &mut Editor, found: Vec<Result<Shared, String>>, advice: Vec<String>, key: Result<(String, String), String>, minutes: u32) {
    let (private, public) = match key {
        Ok(k) => k,
        Err(e) => {
            ed.busy_stop("phone-pair");
            return ed.warn(format!("Can't make a pairing key: {e}"));
        }
    };
    let problems: Vec<String> = found.iter().filter_map(|f| f.as_ref().err().cloned()).collect();
    let shared: Vec<Shared> = found.into_iter().filter_map(Result::ok).collect();
    if shared.is_empty() {
        ed.busy_stop("phone-pair");
        return ed.warn(format!("Nothing to share with the phone: {}", problems.join("; ")));
    }
    let id = uuid::Uuid::now_v7().simple().to_string();
    // (Machines still to answer, those that let the key in, what wasn't shared and why.)
    let left = std::rc::Rc::new(std::cell::RefCell::new((shared.len(), Vec::<Shared>::new(), problems)));
    for s in shared {
        let (left, id2, private, advice) = (left.clone(), id.clone(), private.clone(), advice.clone());
        let machine = s.machine.clone();
        crate::remote::request(ed, &machine, Request::StartPairing { pairing: id.clone(), key: public.clone(), minutes }, move |ed, r| {
            let mut l = left.borrow_mut();
            l.0 -= 1;
            match r {
                Ok(_) => l.1.push(s),
                Err(e) => l.2.push(format!("{}: {e}", shown(&s.machine))),
            }
            if l.0 > 0 {
                return;
            }
            let (ready, problems) = (std::mem::take(&mut l.1), std::mem::take(&mut l.2));
            drop(l);
            show(ed, id2, private, ready, minutes, problems, advice);
        });
    }
}

fn show(ed: &mut Editor, id: String, private: String, shared: Vec<Shared>, minutes: u32, problems: Vec<String>, advice: Vec<String>) {
    ed.busy_stop("phone-pair");
    if shared.is_empty() {
        return ed.warn(format!("No machine let the pairing in, nothing to share with the phone: {}", problems.join("; ")));
    }
    let until_unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() + 60 * minutes as u64;
    let url = link(&id, &private, until_unix, &shared);
    let machines: Vec<String> = shared.iter().map(|s| s.machine.clone()).collect();
    // This machine shows as what it is, with the address the phone was given for it.
    let labels: Vec<String> = shared.iter().map(|s| if s.machine == LOCAL { format!("this machine ({})", s.host) } else { s.name.clone() }).collect();
    let mut b = PairBuffer::new(url, labels, Instant::now() + Duration::from_secs(60 * minutes as u64));
    if !problems.is_empty() {
        b.notes = problems;
    }
    b.advice = advice;
    let buffer = ed.add_buffer("*phone pairing*", Box::new(b));
    ed.show_buffer(buffer);
    ed.remote.pairing = Some(Active { id, machines, until: Instant::now() + Duration::from_secs(60 * minutes as u64), buffer, answered: false });
}

fn set_status(ed: &mut Editor, buffer: BufferId, status: String) {
    ed.with_buffer_as::<PairBuffer, _>(buffer, |b, _| b.status = Some(status));
}

/// The pairing is over: no machine lets its key in any more, and the code says why.
fn end(ed: &mut Editor, a: Active, why: &str) {
    for m in &a.machines {
        crate::remote::send(ed, m, Request::EndPairing { pairing: a.id.clone() });
    }
    set_status(ed, a.buffer, why.to_string());
    ed.with_buffer_as::<PairBuffer, _>(a.buffer, |b, _| b.over = true);
}

/// A phone asks `machine` to let it in: ask, and let it in everywhere shared if you say so.
pub fn requested(ed: &mut Editor, machine: &str, pairing: String, key: String, label: String) {
    // Another window's pairing, or one that is over: not ours to answer.
    let Some(a) = ed.remote.pairing.as_ref().filter(|a| a.id == pairing && !a.answered) else { return };
    let (buffer, machines) = (a.buffer, a.machines.clone());
    let print = fingerprint(&keygen(ed), &key).unwrap_or_else(|| "a key".into());
    set_status(ed, buffer, format!("{label} wants in ({print})…"));
    let asked_on = machine.to_string();
    let into = machines.iter().map(|m| shown(m)).collect::<Vec<_>>().join(", ");
    ed.y_or_n(&format!("Let {label} ({print}) into {into}?"), move |ed, yes| {
        let Some(a) = ed.remote.pairing.as_mut().filter(|a| a.id == pairing) else { return };
        a.answered = true;
        if !yes {
            crate::remote::send(ed, &asked_on, Request::PairAnswer { pairing: pairing.clone(), approved: false });
            let a = ed.remote.pairing.take().unwrap();
            return end(ed, a, &format!("Turned down: {label} wasn't let in"));
        }
        let left = std::rc::Rc::new(std::cell::RefCell::new((machines.len(), Vec::<String>::new(), Vec::<String>::new())));
        for m in &machines {
            let (left, pairing, asked_on, label, m2) = (left.clone(), pairing.clone(), asked_on.clone(), label.clone(), m.clone());
            crate::remote::request(ed, m, Request::AuthorizeKey { key: key.clone(), comment: format!("serpis phone {label}") }, move |ed, r| {
                let mut l = left.borrow_mut();
                l.0 -= 1;
                match r {
                    Ok(_) => l.1.push(m2),
                    Err(e) => l.2.push(format!("{}: {e}", shown(&m2))),
                }
                if l.0 > 0 {
                    return;
                }
                let (ok, failed) = (std::mem::take(&mut l.1), std::mem::take(&mut l.2));
                drop(l);
                let approved = !ok.is_empty();
                crate::remote::send(ed, &asked_on, Request::PairAnswer { pairing, approved });
                let Some(a) = ed.remote.pairing.take() else { return };
                let reached = ok.iter().map(|m| shown(m)).collect::<Vec<_>>().join(", ");
                let mut status = if approved { format!("Paired: {label} can reach {reached}") } else { format!("{label} couldn't be let in") };
                if !failed.is_empty() {
                    status.push_str(&format!(" (not {})", failed.join("; ")));
                }
                ed.message(status.clone());
                ed.run_hook("phone-paired", &json!({"label": label, "machines": ok, "failed": failed}));
                end(ed, a, &status);
            });
        }
    });
}

/// A key's SHA256 fingerprint, as ssh shows it.
fn fingerprint(keygen: &str, key: &str) -> Option<String> {
    use std::io::Write;
    let mut child = std::process::Command::new(keygen).args(["-lf", "-"]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn().ok()?;
    writeln!(child.stdin.take()?, "{key}").ok()?;
    let out = child.wait_with_output().ok()?;
    String::from_utf8_lossy(&out.stdout).split_whitespace().nth(1).map(str::to_string)
}

/// Every tick: a pairing past its time ends.
pub fn tick(ed: &mut Editor) {
    let over = ed.remote.pairing.as_ref().is_some_and(|a| Instant::now() > a.until && !a.answered);
    if over {
        let a = ed.remote.pairing.take().unwrap();
        end(ed, a, "Expired: phone-pair shows a new code");
    }
}

/// The code and what is happening.
pub struct PairBuffer {
    url: String,
    code: Option<qrcode::QrCode>,
    machines: Vec<String>,
    until: Instant,
    status: Option<String>,
    notes: Vec<String>,
    /// What stands between the phone and a machine shared (sshd off here).
    advice: Vec<String>,
    over: bool,
}

impl PairBuffer {
    fn new(url: String, machines: Vec<String>, until: Instant) -> Self {
        let code = qrcode::QrCode::with_error_correction_level(url.as_bytes(), qrcode::EcLevel::L).ok();
        PairBuffer { url, code, machines, until, status: None, notes: Vec::new(), advice: Vec::new(), over: false }
    }

    fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.over {
            out.push(self.status.clone().unwrap_or_default());
        } else {
            out.push("Scan this with your iPhone's camera to pair it (the Serpis app opens).".into());
            let left = self.until.saturating_duration_since(Instant::now()).as_secs();
            out.push(format!("Shares {} · ends in {}:{:02}", self.machines.join(", "), left / 60, left % 60));
            if let Some(s) = &self.status {
                out.push(s.clone());
            }
        }
        out.extend(self.notes.iter().map(|n| format!("Not shared: {n}")));
        if !self.over {
            out.extend(self.advice.iter().cloned());
        }
        out
    }
}

impl Buffer for PairBuffer {
    fn kind(&self) -> &str {
        "pairing"
    }

    fn ui(&mut self, ui: &mut egui::Ui, rect: Rect, _focused: bool, ed: &mut Editor) {
        let theme = ed.theme.clone();
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 0.0, theme.bg);
        let font = FontId::proportional(theme.font_size * 1.1);
        let lines = self.lines();
        let text_h = (lines.len() as f32 + 1.0) * theme.font_size * 1.6;
        if let (Some(code), false) = (&self.code, self.over) {
            let n = code.width();
            let side = (rect.width() - 32.0).min(rect.height() - text_h - 32.0).max(80.0);
            // Dark on white, as cameras read best, in either theme; a quiet border of 3 modules.
            let module = (side / (n + 6) as f32).floor().max(1.0);
            let full = module * (n + 6) as f32;
            let top_left = egui::pos2(rect.center().x - full / 2.0, rect.top() + 16.0);
            p.rect_filled(Rect::from_min_size(top_left, Vec2::splat(full)), 6.0, Color32::WHITE);
            let colors = code.to_colors();
            for (i, c) in colors.iter().enumerate() {
                if *c == qrcode::Color::Dark {
                    let (x, y) = ((i % n + 3) as f32, (i / n + 3) as f32);
                    p.rect_filled(Rect::from_min_size(top_left + Vec2::new(x * module, y * module), Vec2::splat(module)), 0.0, Color32::BLACK);
                }
            }
            let mut y = top_left.y + full + 16.0;
            for l in &lines {
                p.text(egui::pos2(rect.center().x, y), Align2::CENTER_TOP, l, font.clone(), theme.fg);
                y += theme.font_size * 1.6;
            }
            // The countdown moves.
            ui.ctx().request_repaint_after(Duration::from_millis(500));
        } else {
            let mut y = rect.center().y - text_h / 2.0;
            for l in &lines {
                p.text(egui::pos2(rect.center().x, y), Align2::CENTER_TOP, l, font.clone(), theme.fg);
                y += theme.font_size * 1.6;
            }
        }
    }

    fn full_text(&mut self) -> Option<String> {
        let mut t = self.lines().join("\n");
        if !self.over {
            t.push_str(&format!("\n{}", self.url));
        }
        Some(t)
    }

    fn modeline(&self) -> Modeline {
        Modeline { title: "Pair a phone".into(), ..Default::default() }
    }

    fn on_kill(&mut self, ed: &mut Editor) {
        if let Some(a) = ed.remote.pairing.take() {
            for m in &a.machines {
                crate::remote::send(ed, m, Request::EndPairing { pairing: a.id.clone() });
            }
        }
    }

    impl_any!();
}

pub fn install(ed: &mut Editor) {
    ed.defvar("phone-pair-minutes", json!(10), "How long a phone-pair code works.");
    ed.defvar(
        "phone-pair-local-address",
        Editor::persisted_state("phone-pair-local-address").unwrap_or_else(|| json!("")),
        "The address a phone reaches this machine at when you share it ([you@]host[:port]): a Tailscale name, a .local name on your network, an address. Kept between runs; \"\": phone-pair asks, offering the names this machine has.",
    );
    ed.defvar("phone-pair-local-host-keys", json!("/etc/ssh/ssh_host_*_key.pub"), "Where phone-pair reads this machine's ssh host keys from, to tell a phone which keys to trust for it.");
    ed.defvar("phone-pair-sshd-config", json!("/etc/ssh/sshd_config"), "The sshd config phone-pair reads the port this machine takes ssh on from (an address with a port of its own wins).");
    ed.define_command(
        "phone-pair",
        "Pair your iPhone: shows a code its camera scans (the Serpis app opens). The phone gets the machines you share, trusting only the host keys trusted here, and sends its key back through one of them; you're asked before it is let in, then it can reach every machine shared (params: machine names, \"local\" for this machine). Sharing this machine lets the phone drive the chats this window shows; it needs ssh on here (Remote Login on macOS) and an address the phone can use (phone-pair-local-address). Runs the phone-paired hook.",
        "machines",
        |ed, a| {
            let names: Vec<String> = (0..8).filter_map(|i| a.str(i).map(str::to_string)).collect();
            pair(ed, (!names.is_empty()).then_some(names));
            Ok(())
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_ssh_g_says() {
        let out = "user me\nhostname box.tail1.ts.net\nport 2222\nhostkeyalias none\nidentityfile ~/.ssh/id_ed25519\n";
        assert_eq!(ssh_g(out), ("box.tail1.ts.net".into(), 2222, "me".into(), None));
        assert_eq!(ssh_g("hostname 10.0.0.5\nhostkeyalias lab\n").3, Some("lab".into()));
    }

    #[test]
    fn keys_found() {
        let out = "# Host box found: line 3\nbox ssh-ed25519 AAAAone\n# Host box found: line 9\n|1|abc=|def= ecdsa-sha2-nistp256 AAAAtwo\n@cert-authority * ssh-ed25519 AAAAca\n";
        assert_eq!(found_keys(out), ["ssh-ed25519 AAAAone", "ecdsa-sha2-nistp256 AAAAtwo"]);
    }

    #[test]
    fn the_link_holds_everything_the_phone_needs() {
        let private = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaA\nAAAA\n-----END OPENSSH PRIVATE KEY-----\n";
        let s = Shared { machine: "box".into(), name: "box".into(), host: "box.local".into(), port: 22, user: "me".into(), keys: vec!["ssh-ed25519 AAAA".into()] };
        let url = link("abc", private, 1_800_000_000, &[s]);
        let data = url.strip_prefix("serpis://pair#").unwrap();
        let v: Value = serde_json::from_slice(&base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(data).unwrap()).unwrap();
        assert_eq!(v, json!({"v": 1, "p": "abc", "k": "b3BlbnNzaAAAAA", "x": 1_800_000_000u64, "m": [{"n": "box", "h": "box.local", "p": 22, "u": "me", "k": ["ssh-ed25519 AAAA"]}]}));
    }

    #[test]
    fn addresses_in_parts() {
        assert_eq!(dest_parts("box.local"), (None, "box.local".into(), None));
        assert_eq!(dest_parts(" me@box.tail1.ts.net:2222 "), (Some("me".into()), "box.tail1.ts.net".into(), Some(2222)));
        assert_eq!(dest_parts("ssh://me@10.0.0.5"), (Some("me".into()), "10.0.0.5".into(), None));
        assert_eq!(dest_parts("[fd7a::1]:22"), (None, "fd7a::1".into(), Some(22)));
        // Not a port: part of the host (an IPv6 address written bare).
        assert_eq!(dest_parts("fd7a::1"), (None, "fd7a::1".into(), None));
    }

    #[test]
    fn this_machines_host_keys_and_port() {
        let dir = std::env::temp_dir().join(format!("serpis-pair-test-{}", uuid::Uuid::now_v7().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ssh_host_ed25519_key.pub"), "ssh-ed25519 AAAAone root@mac\n").unwrap();
        std::fs::write(dir.join("ssh_host_rsa_key.pub"), "ssh-rsa AAAAtwo root@mac\n").unwrap();
        // Not a public key file: left alone.
        std::fs::write(dir.join("ssh_host_ed25519_key"), "-----BEGIN OPENSSH PRIVATE KEY-----\n").unwrap();
        let keys = local_keys(&format!("{}/ssh_host_*_key.pub", dir.display()));
        assert_eq!(keys, ["ssh-ed25519 AAAAone", "ssh-rsa AAAAtwo"]);
        assert!(local_keys(&format!("{}/nothing_*", dir.display())).is_empty());
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(sshd_port("#Port 22\nPermitRootLogin no\nPort 2222\n"), Some(2222));
        assert_eq!(sshd_port("PasswordAuthentication no\n"), None);
    }

    #[test]
    fn tailscale_says_what_this_machine_is_called_there() {
        let status = json!({"Self": {"DNSName": "mac.tail1234.ts.net.", "TailscaleIPs": ["100.64.0.1", "fd7a::1"]}, "Peer": {}});
        assert_eq!(tailscale_addresses(&status).iter().map(|(a, _)| a.as_str()).collect::<Vec<_>>(), ["mac.tail1234.ts.net", "100.64.0.1"]);
        assert!(tailscale_addresses(&json!({})).is_empty());
    }
}
