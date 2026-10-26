//! Terminals on this machine, held by a process of their own (`serpis-node terminals`) so they
//! outlive every window and a chat node restarting on a new build never takes a shell with it.
//! The chat node forwards its clients' terminal requests and bytes here (over the connection
//! they already have to it), so a client reaches a machine's terminals the way it reaches its
//! chats.
//!
//! It speaks the node's own framing ([`serpis_proto::wire`]): calls (`Terminals`,
//! `NewTerminal`, `AttachTerminal`, `DetachTerminal`, `ResizeTerminal`, `KillTerminal`, `Stop`)
//! answered by replies, and [`tag::TERMINAL`] frames for the bytes. Each terminal keeps what it
//! printed last, raw: a view that attaches gets it (for its scrollback) and its screen, which an
//! emulator works out from it only then. Nothing reads what a terminal prints on its way, so it
//! prints as fast as a terminal can take it.

use std::collections::HashMap;
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use anyhow::{Context, Result, bail};
use serpis_proto::wire::{self, Call, Event, Reply, Request, Response, TerminalInfo, tag};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

/// How much of what a terminal printed is kept for a fresh view's scrollback.
const KEEP: usize = 1 << 20;

/// Where the terminal host listens: `$SERPIS_TERMINAL_SOCKET`, else `terminals.sock` beside
/// the node's.
pub fn socket() -> PathBuf {
    std::env::var_os("SERPIS_TERMINAL_SOCKET").map(PathBuf::from).unwrap_or_else(|| crate::node_dir().join("terminals.sock"))
}

/// One terminal.
struct Term {
    name: String,
    /// The program, as listings show it (the shell's name).
    command: String,
    master: Arc<OwnedFd>,
    child: nix::unistd::Pid,
    started: SystemTime,
    last_output: Instant,
    /// What it printed last, from a line's start.
    kept: Vec<u8>,
    /// The size it is shown at.
    size: (u16, u16),
    /// The views showing it: by connection.
    views: HashMap<u64, mpsc::UnboundedSender<Vec<u8>>>,
}

impl Term {
    /// Take in what it printed.
    fn printed(&mut self, bytes: &[u8]) {
        self.last_output = Instant::now();
        self.kept.extend_from_slice(bytes);
        // Cut back to `KEEP` once it is twice that (cutting every time would move the whole
        // megabyte for every few bytes printed), at a line's start, so what is kept doesn't
        // begin inside an escape sequence.
        if self.kept.len() > 2 * KEEP {
            let over = self.kept.len() - KEEP;
            let cut = self.kept[over..].iter().position(|&b| b == b'\n').map_or(over, |i| over + i + 1);
            self.kept.drain(..cut);
        }
    }

    /// Its screen as it is, as escape sequences that paint it: worked out from what it printed
    /// last.
    fn screen(&self) -> String {
        let mut vt = avt::Vt::builder().size(self.size.0 as usize, self.size.1 as usize).build();
        vt.feed_str(&String::from_utf8_lossy(&self.kept));
        vt.dump()
    }

    /// What a view needs to show it as it is: what it printed before (for scrollback, when
    /// asked), then its screen.
    fn paint(&self, history: bool) -> Vec<u8> {
        let mut out = Vec::new();
        if history {
            out.extend_from_slice(&self.kept);
        }
        // Back to the main screen, cleared, then the screen as it is.
        out.extend_from_slice(b"\x1b[?1049l\x1b[0m\x1b[H\x1b[2J");
        out.extend_from_slice(self.screen().as_bytes());
        out
    }

    fn info(&self) -> TerminalInfo {
        let front = nix::unistd::tcgetpgrp(self.master.as_ref()).ok().filter(|pg| *pg != self.child);
        TerminalInfo {
            name: self.name.clone(),
            title: self.command.clone(),
            command: self.command.clone(),
            cwd: process_cwd(front.unwrap_or(self.child).as_raw()),
            foreground: front.and_then(|pg| process_name(pg.as_raw())).into_iter().collect(),
            attached: self.views.len(),
            idle: self.last_output.elapsed().as_secs(),
            started: self.started.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0),
        }
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        let ws = nix::pty::Winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
        // SAFETY: TIOCSWINSZ reads a Winsize, the size the call expects.
        unsafe {
            nix::libc::ioctl(self.master.as_raw_fd(), nix::libc::TIOCSWINSZ, &ws);
        }
        self.size = (cols, rows);
    }

    /// Hang up its programs.
    fn hang_up(&self) {
        let _ = nix::sys::signal::killpg(self.child, nix::sys::signal::Signal::SIGHUP);
        let _ = nix::sys::signal::kill(self.child, nix::sys::signal::Signal::SIGHUP);
    }
}

type Terms = Arc<Mutex<HashMap<String, Term>>>;

/// The name of process `pid`'s program.
fn process_name(pid: i32) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let mut buf = [0u8; 256];
        // SAFETY: proc_name writes at most `buf.len()` bytes into `buf`.
        let n = unsafe { nix::libc::proc_name(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
        (n > 0).then(|| String::from_utf8_lossy(&buf[..n as usize]).into_owned())
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::fs::read_to_string(format!("/proc/{pid}/comm")).ok().map(|s| s.trim().to_string())
    }
}

/// Where process `pid` is, when the machine can say.
fn process_cwd(pid: i32) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let mut info: nix::libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<nix::libc::proc_vnodepathinfo>() as i32;
        // SAFETY: proc_pidinfo writes at most `size` bytes into `info`.
        let n = unsafe { nix::libc::proc_pidinfo(pid, nix::libc::PROC_PIDVNODEPATHINFO, 0, (&mut info as *mut nix::libc::proc_vnodepathinfo).cast(), size) };
        if n != size {
            return None;
        }
        let bytes: Vec<u8> = info.pvi_cdir.vip_path.iter().flatten().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
        (!bytes.is_empty()).then(|| String::from_utf8_lossy(&bytes).into_owned())
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::fs::read_link(format!("/proc/{pid}/cwd")).ok().map(|p| p.to_string_lossy().into_owned())
    }
}

/// The scripts serpis's shell integration is made of (zsh: prompt marks, the directory).
pub const SHELL_FILES: [(&str, &str); 2] = [("zsh/.zshenv", include_str!("../shell/zsh/.zshenv")), ("serpis.zsh", include_str!("../shell/serpis.zsh"))];

/// Write the shell integration to a directory of this user's, and return it.
pub fn install_shell_integration() -> Option<PathBuf> {
    let dir = std::env::temp_dir().join(format!("serpis-shell-{}", nix::unistd::getuid()));
    std::fs::create_dir_all(dir.join("zsh")).ok()?;
    for (path, body) in SHELL_FILES {
        let p = dir.join(path);
        if std::fs::read_to_string(&p).ok().as_deref() != Some(body) {
            std::fs::write(&p, body).ok()?;
        }
    }
    Some(dir)
}

/// The user's shell: `$SHELL` (sshd sets it to the login shell), else the account's.
pub fn login_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| nix::unistd::User::from_uid(nix::unistd::getuid()).ok().flatten().map(|u| u.shell.to_string_lossy().into_owned()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/sh".into())
}

/// A login shell in `cwd` on a new PTY, with this process's environment and serpis's terminal
/// variables (and, for zsh, its integration): the PTY's master side and the shell's pid.
pub fn spawn_shell(cwd: &Path, cols: u16, rows: u16, integration: bool) -> Result<(OwnedFd, nix::unistd::Pid)> {
    use nix::pty::{ForkptyResult, Winsize, forkpty};
    use std::ffi::CString;
    let ws = Winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
    let shell = login_shell();
    // Everything the child needs is made before forking: nothing allocates after.
    let prog = CString::new(shell.clone())?;
    let base = Path::new(&shell).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| "sh".into());
    let arg0 = CString::new(format!("-{base}"))?;
    let cwd_c = CString::new(cwd.as_os_str().as_encoded_bytes())?;
    let mut env: Vec<CString> = std::env::vars()
        .filter(|(k, _)| !matches!(k.as_str(), "TERM" | "COLORTERM" | "TERM_PROGRAM"))
        .filter_map(|(k, v)| CString::new(format!("{k}={v}")).ok())
        .collect();
    env.push(CString::new("TERM=xterm-256color")?);
    env.push(CString::new("COLORTERM=truecolor")?);
    env.push(CString::new("TERM_PROGRAM=serpis")?);
    // zsh reads $ZDOTDIR/.zshenv first; serpis's puts ZDOTDIR back and adds the prompt marks.
    if let Some(dir) = (integration && base == "zsh").then(install_shell_integration).flatten() {
        env.retain(|e| !e.as_bytes().starts_with(b"ZDOTDIR="));
        if let Some(z) = std::env::var_os("ZDOTDIR") {
            env.push(CString::new(format!("SERPIS_ZDOTDIR={}", z.to_string_lossy()))?);
        }
        env.push(CString::new(format!("ZDOTDIR={}", dir.join("zsh").display()))?);
        env.push(CString::new(format!("SERPIS_SHELL_DIR={}", dir.display()))?);
    }
    // SAFETY: the child calls only async-signal-safe functions (chdir, execve, _exit).
    match unsafe { forkpty(&ws, None)? } {
        ForkptyResult::Child => {
            unsafe {
                nix::libc::chdir(cwd_c.as_ptr());
            }
            let _ = nix::unistd::execve(&prog, &[arg0.as_c_str()], &env);
            unsafe { nix::libc::_exit(127) }
        }
        ForkptyResult::Parent { child, master } => Ok((master, child)),
    }
}

/// Run the terminal host at `socket` until it is stopped (`Stop`, or a signal): every terminal
/// in it is hung up on the way out.
pub async fn serve(socket: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let dir = socket.parent().context("the socket path has no directory")?;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    if socket.exists() {
        if tokio::net::UnixStream::connect(socket).await.is_ok() {
            bail!("a terminal host is already running on {}", socket.display());
        }
        std::fs::remove_file(socket)?;
    }
    let listener = tokio::net::UnixListener::bind(socket).with_context(|| format!("listening on {}", socket.display()))?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    let terms: Terms = Default::default();
    let (stop_tx, mut stop_rx) = mpsc::unbounded_channel::<()>();
    {
        let stop = stop_tx.clone();
        tokio::spawn(async move {
            use tokio::signal::unix::{SignalKind, signal};
            let (Ok(mut term), Ok(mut int)) = (signal(SignalKind::terminate()), signal(SignalKind::interrupt())) else { return };
            tokio::select! {
                _ = term.recv() => {}
                _ = int.recv() => {}
            }
            let _ = stop.send(());
        });
    }
    let me = nix::unistd::getuid().as_raw();
    let mut next = 0u64;
    loop {
        tokio::select! {
            _ = stop_rx.recv() => break,
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                // The directory keeps others out already; this holds even if its mode changes.
                if stream.peer_cred().map(|c| c.uid()).ok() != Some(me) {
                    continue;
                }
                next += 1;
                tokio::spawn(connection(stream, next, terms.clone(), stop_tx.clone()));
            }
        }
    }
    // Hung up gently, so shells write their history; then the socket goes.
    for t in terms.lock().unwrap().values() {
        t.hang_up();
    }
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let _ = std::fs::remove_file(socket);
    Ok(())
}

/// Serve one connection (a chat node, for its clients) until it closes.
async fn connection(stream: tokio::net::UnixStream, id: u64, terms: Terms, stop: mpsc::UnboundedSender<()>) {
    let (mut rd, mut wr) = stream.into_split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let writer = tokio::spawn(async move {
        while let Some(bytes) = rx.recv().await {
            if wr.write_all(&bytes).await.is_err() {
                break;
            }
        }
    });
    let mut decoder = wire::Decoder::default();
    let mut buf = vec![0u8; 64 * 1024];
    'read: loop {
        let n = match rd.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        decoder.push(&buf[..n]);
        loop {
            match decoder.next() {
                Ok(Some((tag::CALL, body))) => {
                    let Ok(call) = serde_json::from_slice::<Call>(&body) else { continue };
                    let stopping = matches!(call.request, Request::Stop);
                    let response = answer(&terms, id, &tx, call.request).unwrap_or_else(|e| Response::Error { message: format!("{e:#}") });
                    let _ = tx.send(wire::encode(tag::REPLY, &Reply { id: call.id, response }));
                    if stopping {
                        let _ = stop.send(());
                    }
                }
                Ok(Some((tag::TERMINAL, body))) => {
                    if let Some((name, bytes)) = wire::parse_terminal(&body) {
                        let master = terms.lock().unwrap().get(&name).map(|t| t.master.clone());
                        if let Some(m) = master {
                            let _ = nix::unistd::write(m.as_ref(), bytes);
                        }
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => break 'read,
            }
        }
    }
    // Gone: it shows nothing any more.
    for t in terms.lock().unwrap().values_mut() {
        t.views.remove(&id);
    }
    drop(tx);
    let _ = writer.await;
}

fn answer(terms: &Terms, conn: u64, tx: &mpsc::UnboundedSender<Vec<u8>>, request: Request) -> Result<Response> {
    Ok(match request {
        Request::Terminals => {
            let mut list: Vec<TerminalInfo> = terms.lock().unwrap().values().map(Term::info).collect();
            list.sort_by_key(|t| t.started);
            Response::Terminals { terminals: list }
        }
        Request::NewTerminal { cwd, cols, rows, integration } => {
            let cwd = PathBuf::from(cwd);
            if !cwd.is_dir() {
                bail!("{} isn't a folder here", cwd.display());
            }
            let (master, child) = spawn_shell(&cwd, cols, rows, integration)?;
            let shell = login_shell();
            let base = shell.rsplit('/').next().unwrap_or(&shell).to_string();
            let mut all = terms.lock().unwrap();
            let name = (1..).map(|n| if n == 1 { base.clone() } else { format!("{base}-{n}") }).find(|n| !all.contains_key(n)).unwrap_or_default();
            let master = Arc::new(master);
            all.insert(name.clone(), Term { name: name.clone(), command: base, master: master.clone(), child, started: SystemTime::now(), last_output: Instant::now(), kept: Vec::new(), size: (cols, rows), views: HashMap::new() });
            drop(all);
            read_from(terms.clone(), name.clone(), master, child);
            Response::Terminal { name }
        }
        Request::AttachTerminal { name, cols, rows, history } => {
            let mut all = terms.lock().unwrap();
            let t = all.get_mut(&name).with_context(|| format!("no terminal {name} here"))?;
            t.resize(cols, rows);
            t.views.insert(conn, tx.clone());
            // Its screen as it is, then whatever it prints from now on.
            let _ = tx.send(wire::terminal_frame(&name, &t.paint(history)));
            Response::Ok
        }
        Request::DetachTerminal { name } => {
            if let Some(t) = terms.lock().unwrap().get_mut(&name) {
                t.views.remove(&conn);
            }
            Response::Ok
        }
        Request::RepaintTerminal { name } => {
            if let Some(t) = terms.lock().unwrap().get(&name) {
                let _ = tx.send(wire::terminal_frame(&name, &t.paint(false)));
            }
            Response::Ok
        }
        Request::ResizeTerminal { name, cols, rows } => {
            if let Some(t) = terms.lock().unwrap().get_mut(&name) {
                t.resize(cols, rows);
            }
            Response::Ok
        }
        Request::KillTerminal { name } => {
            terms.lock().unwrap().get(&name).with_context(|| format!("no terminal {name} here"))?.hang_up();
            Response::Ok
        }
        Request::Stop => Response::Ok,
        other => bail!("the terminal host doesn't answer {other:?}"),
    })
}

/// Read what `name` prints, on a thread of its own, until it ends: kept, and sent to its views.
fn read_from(terms: Terms, name: String, master: Arc<OwnedFd>, child: nix::unistd::Pid) {
    std::thread::spawn(move || {
        let Ok(reader) = nix::unistd::dup(master.as_raw_fd()) else { return };
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match nix::unistd::read(reader, &mut buf) {
                Ok(0) | Err(nix::errno::Errno::EIO) => break,
                Ok(n) => {
                    let mut all = terms.lock().unwrap();
                    let Some(t) = all.get_mut(&name) else { break };
                    t.printed(&buf[..n]);
                    let frame = wire::terminal_frame(&name, &buf[..n]);
                    t.views.retain(|_, v| v.send(frame.clone()).is_ok());
                }
                Err(nix::errno::Errno::EINTR) => continue,
                Err(_) => break,
            }
        }
        let _ = nix::unistd::close(reader);
        let code = match nix::sys::wait::waitpid(child, None) {
            Ok(nix::sys::wait::WaitStatus::Exited(_, c)) => Some(c),
            _ => None,
        };
        if let Some(t) = terms.lock().unwrap().remove(&name) {
            let ended = wire::encode(tag::EVENT, &Event::TerminalExited { name: name.clone(), code });
            for v in t.views.values() {
                let _ = v.send(ended.clone());
            }
        }
    });
}

/// Make sure a terminal host listens at `socket`, starting `exe terminals` (in a session of its
/// own, so it outlives whatever started it) when none does.
pub async fn ensure(exe: &Path, socket: &Path) -> Result<()> {
    if tokio::net::UnixStream::connect(socket).await.is_ok() {
        return Ok(());
    }
    use std::os::unix::process::CommandExt;
    let dir = socket.parent().context("the socket path has no directory")?;
    std::fs::create_dir_all(dir)?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("terminals.log"))?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("terminals").env("SERPIS_TERMINAL_SOCKET", socket).stdin(std::process::Stdio::null()).stdout(log.try_clone()?).stderr(log);
    // SAFETY: setsid is async-signal-safe, and nothing else runs between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            nix::unistd::setsid().map_err(std::io::Error::from)?;
            Ok(())
        });
    }
    cmd.spawn().context("starting the terminal host")?;
    crate::bridge::wait_for(socket).await.map(|_| ())
}

/// Stop the terminal host at `socket`, if one runs: every terminal in it ends.
pub async fn stop(socket: &Path) -> Result<()> {
    let Ok(stream) = tokio::net::UnixStream::connect(socket).await else { return Ok(()) };
    let (mut rd, mut wr) = stream.into_split();
    wr.write_all(&wire::encode(tag::CALL, &Call { id: 1, request: Request::Stop })).await?;
    // Gone once the socket closes.
    let mut buf = [0u8; 1024];
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while rd.read(&mut buf).await.is_ok_and(|n| n > 0) {}
    })
    .await;
    for _ in 0..50 {
        if tokio::net::UnixStream::connect(socket).await.is_err() {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    bail!("the terminal host at {} didn't stop", socket.display())
}

#[cfg(test)]
mod tests {
    #[test]
    fn what_is_kept_starts_at_a_line_and_the_screen_comes_from_it() {
        let pty = nix::pty::openpty(None, None).unwrap();
        let mut t = super::Term {
            name: "t".into(),
            command: "sh".into(),
            master: std::sync::Arc::new(pty.master),
            child: nix::unistd::Pid::from_raw(1),
            started: std::time::SystemTime::now(),
            last_output: std::time::Instant::now(),
            kept: Vec::new(),
            size: (20, 5),
            views: Default::default(),
        };
        let line = b"\x1b[31mred\x1b[0m line\n";
        for _ in 0..(super::KEEP / line.len() + 10) {
            t.printed(line);
        }
        assert!(t.kept.len() <= 2 * super::KEEP);
        assert!(t.kept.starts_with(line), "kept from a line's start");
        // A character split across two reads is whole on the screen.
        let e = "é".as_bytes();
        t.printed(&e[..1]);
        t.printed(&e[1..]);
        assert!(t.screen().contains('é'), "{}", t.screen());
    }
}
