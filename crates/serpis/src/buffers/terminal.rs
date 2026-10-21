//! Terminal buffer backed by libghostty-vt.
//!
//! The PTY is read on a background thread; bytes are fed to the VT engine on the UI thread.
//! Unbound keys are encoded by libghostty's key encoder (honouring application cursor mode,
//! the Kitty keyboard protocol, ...). The scrollback is exposed as a [`TextSurface`]
//! ("copy mode"), so vim normal state, search and yank work on terminal output.
//!
//! On the live input line (the shell's command line, or a full-screen app's prompt row such as
//! Claude Code's) the surface is editable: motions move the copy cursor locally, and edits are replayed to the
//! program as cursor keys, backspaces and pastes, each step confirmed by the echoed cursor.

use std::ops::Range;
use std::os::fd::{AsRawFd, OwnedFd};
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Pos2, Rect, Stroke, Vec2};
use libghostty_vt::key::{self, Key as GKey};
use libghostty_vt::render::{CellIterator, CursorVisualStyle, RenderState, RowIterator};
use libghostty_vt::screen::{CellSemanticContent, CellWide, Screen};
use libghostty_vt::style::{RgbColor, StyleColor};
use libghostty_vt::terminal::{Mode, Point, PointCoordinate, ScrollViewport, Terminal};

use crate::buffer::{Activity, ActivityState, Buffer, Modeline, ProcessStatus};
use crate::editor::Editor;
use crate::impl_any;
use crate::keys::{Key, KeyChord};
use crate::text::{CursorShape, Pos, TextSurface, char_len, ordered};

const TERM: &str = "xterm-256color";
const PAD: f32 = 6.0;
const SCROLLBACK_LINES: usize = 20_000;
/// How long to wait for the program to echo an editing key before giving up on it.
const ECHO_TIMEOUT: Duration = Duration::from_millis(400);
/// Fallback prompt detection when the shell doesn't mark prompts with OSC 133.
pub const DEFAULT_PROMPT_REGEXP: &str = "^[^#$%>λ❯→➜]{0,60}[#$%>λ❯→➜]+[ \\x{a0}]*";

enum PtyMsg {
    Data(Vec<u8>),
    Exit,
}

pub struct TerminalBuffer {
    term: Terminal<'static, 'static>,
    render: RenderState<'static>,
    rows_it: RowIterator<'static>,
    cells_it: CellIterator<'static>,
    key_enc: key::Encoder<'static>,
    key_ev: key::Event<'static>,
    master: Arc<OwnedFd>,
    child: nix::unistd::Pid,
    rx: mpsc::Receiver<PtyMsg>,
    grid: Arc<AtomicU32>,
    /// A cell's size in pixels as programs are told (CSI 16 t; image sizes), width << 16 | height.
    cell_px: Arc<AtomicU32>,
    pub images: super::term_images::Images,
    cell: Vec2,
    cols: u16,
    rows: u16,
    pub exited: bool,
    /// Copy mode: a cursor and selection over the scrollback, driven by motions.
    pub copy_mode: bool,
    cursor: Pos,
    anchor: Option<Pos>,
    /// Row text, filled on demand. Rows that scrolled off stay valid as output arrives.
    row_cache: RefCell<Vec<Option<String>>>,
    /// Scrollback length when `row_cache` was last invalidated.
    cached_scrollback: usize,
    /// Bumped on every chunk of output.
    output_gen: u64,
    origin: Pos2,
    out: Vec<u8>,
    title: String,
    cwd: std::path::PathBuf,
    /// Theme generation whose colors were applied to the VT engine.
    theme_gen: Option<u64>,
    /// The PTY closed while an edit was waiting for echo; `poll` reports it.
    exit_seen: bool,
    prompt_re: Option<regex::Regex>,
    /// Last output that wasn't the echo of our own input (activity detection).
    last_output: Option<Instant>,
    last_input: Cell<Option<Instant>>,
    /// Foreground program (sampled in `poll`) and whether it is a coding agent.
    fg: Option<String>,
    fg_agent: bool,
    fg_checked: Option<Instant>,
    busy_title_re: Option<regex::Regex>,
    /// Seconds of quiet after which an agent counts as waiting for input.
    quiet_after: f64,
}

/// The editable input on the terminal cursor's logical line (soft-wrapped rows included).
#[derive(Debug, Clone, Copy)]
struct Input {
    /// First input character, just past the prompt.
    start: Pos,
    /// Just past the last typed character (excludes a trailing autosuggestion).
    end: Pos,
    first_line: usize,
    last_line: usize,
}

fn rgb(c: RgbColor) -> Color32 {
    Color32::from_rgb(c.r, c.g, c.b)
}

/// Write the shell integration scripts to a per-user temp dir; returns the dir.
fn install_shell_integration() -> Option<std::path::PathBuf> {
    let dir = std::env::temp_dir().join(format!("serpis-shell-{}", unsafe { nix::libc::getuid() }));
    std::fs::create_dir_all(dir.join("zsh")).ok()?;
    for (path, body) in [
        ("zsh/.zshenv", include_str!("../../shell/zsh/.zshenv")),
        ("serpis.zsh", include_str!("../../shell/serpis.zsh")),
    ] {
        let p = dir.join(path);
        if std::fs::read_to_string(&p).ok().as_deref() != Some(body) {
            std::fs::write(&p, body).ok()?;
        }
    }
    Some(dir)
}

fn spawn_shell(cwd: &std::path::Path, cols: u16, rows: u16, integration: bool) -> anyhow::Result<(OwnedFd, nix::unistd::Pid)> {
    use nix::pty::{ForkptyResult, Winsize, forkpty};
    let ws = Winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into());
    // Build everything the child needs before forking (no allocation after fork).
    let prog = std::ffi::CString::new(shell.clone())?;
    let base = std::path::Path::new(&shell).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| "sh".into());
    let arg0 = std::ffi::CString::new(format!("-{base}"))?;
    let cwd_c = std::ffi::CString::new(cwd.as_os_str().as_encoded_bytes())?;
    let mut env: Vec<std::ffi::CString> = std::env::vars()
        .filter(|(k, _)| !matches!(k.as_str(), "TERM" | "COLORTERM" | "TERM_PROGRAM"))
        .filter_map(|(k, v)| std::ffi::CString::new(format!("{k}={v}")).ok())
        .collect();
    env.push(std::ffi::CString::new(format!("TERM={TERM}"))?);
    env.push(std::ffi::CString::new("COLORTERM=truecolor")?);
    env.push(std::ffi::CString::new("TERM_PROGRAM=serpis")?);
    // zsh reads $ZDOTDIR/.zshenv first; ours restores ZDOTDIR and loads the prompt marks.
    if let Some(dir) = (integration && base == "zsh").then(install_shell_integration).flatten() {
        env.retain(|e| !e.as_bytes().starts_with(b"ZDOTDIR="));
        if let Some(z) = std::env::var_os("ZDOTDIR") {
            env.push(std::ffi::CString::new(format!("SERPIS_ZDOTDIR={}", z.to_string_lossy()))?);
        }
        env.push(std::ffi::CString::new(format!("ZDOTDIR={}", dir.join("zsh").display()))?);
        env.push(std::ffi::CString::new(format!("SERPIS_SHELL_DIR={}", dir.display()))?);
    }
    // SAFETY: the child only calls async-signal-safe functions (chdir, execve, _exit).
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

impl TerminalBuffer {
    /// A shell in `params.cwd`, else in the editor's directory.
    pub fn new(ed: &mut Editor, params: &serde_json::Value) -> anyhow::Result<Self> {
        let (cols, rows) = (100u16, 30u16);
        let integration = ed.var("terminal-shell-integration").and_then(serde_json::Value::as_bool).unwrap_or(true);
        let cwd = params.get("cwd").and_then(serde_json::Value::as_str).map(crate::expand_home).filter(|d| d.is_dir()).unwrap_or_else(|| ed.cwd.clone());
        let (master, child) = spawn_shell(&cwd, cols, rows, integration)?;
        let master = Arc::new(master);
        let reader = nix::unistd::dup(master.as_raw_fd())?;
        let (tx, rx) = mpsc::channel();
        let repaint = ed.main_sender();
        std::thread::Builder::new().name("serpis-pty".into()).spawn(move || {
            let mut buf = vec![0u8; 65536];
            loop {
                match nix::unistd::read(reader, &mut buf) {
                    Ok(0) | Err(nix::errno::Errno::EIO) => break,
                    Ok(n) => {
                        if tx.send(PtyMsg::Data(buf[..n].to_vec())).is_err() {
                            break;
                        }
                        repaint.repaint();
                    }
                    Err(nix::errno::Errno::EINTR) => continue,
                    Err(_) => break,
                }
            }
            let _ = nix::unistd::close(reader);
            let _ = tx.send(PtyMsg::Exit);
            repaint.repaint();
        })?;

        let grid = Arc::new(AtomicU32::new(((cols as u32) << 16) | rows as u32));
        let cell_px = Arc::new(AtomicU32::new((8 << 16) | 16));
        let mut term = Terminal::new(cols, rows).map_err(|e| anyhow::anyhow!("libghostty: {e:?}"))?;
        term.set_scrollback_max_lines(Some(SCROLLBACK_LINES)).map_err(|e| anyhow::anyhow!("{e:?}"))?;
        let _ = term.set_terminfo_name(TERM);
        // Images (Kitty graphics protocol), sent inline, from files or from shared memory as
        // local programs do (kitten icat, yazi).
        let mb = ed.var("terminal-image-storage-mb").and_then(serde_json::Value::as_u64).unwrap_or(320);
        if mb > 0 {
            super::term_images::init();
            let _ = term.set_kitty_image_storage_limit(mb * 1024 * 1024);
            let _ = term.set_kitty_image_from_file_allowed(true);
            let _ = term.set_kitty_image_from_shared_mem_allowed(true);
            let _ = term.set_kitty_image_temp_file_dir(Some(&std::env::temp_dir()));
        }
        {
            let w = master.clone();
            let g = grid.clone();
            let px = cell_px.clone();
            term.on_pty_write(move |_t, data| {
                let _ = nix::unistd::write(w.as_ref(), data);
            })
            .map_err(|e| anyhow::anyhow!("{e:?}"))?
            .on_size(move |_t| {
                let v = g.load(Ordering::Relaxed);
                let c = px.load(Ordering::Relaxed);
                Some(libghostty_vt::terminal::SizeReportSize {
                    rows: (v & 0xffff) as u16,
                    columns: (v >> 16) as u16,
                    cell_width: c >> 16,
                    cell_height: c & 0xffff,
                })
            })
            .map_err(|e| anyhow::anyhow!("{e:?}"))?
            .on_xtversion(|_t| Some("serpis"))
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;
        }
        Ok(TerminalBuffer {
            term,
            render: RenderState::new().map_err(|e| anyhow::anyhow!("{e:?}"))?,
            rows_it: RowIterator::new().map_err(|e| anyhow::anyhow!("{e:?}"))?,
            cells_it: CellIterator::new().map_err(|e| anyhow::anyhow!("{e:?}"))?,
            key_enc: key::Encoder::new().map_err(|e| anyhow::anyhow!("{e:?}"))?,
            key_ev: key::Event::new().map_err(|e| anyhow::anyhow!("{e:?}"))?,
            master,
            child,
            rx,
            grid,
            cell_px,
            images: Default::default(),
            cell: Vec2::new(8.0, 16.0),
            cols,
            rows,
            exited: false,
            copy_mode: false,
            cursor: Pos::ZERO,
            anchor: None,
            row_cache: RefCell::new(Vec::new()),
            cached_scrollback: 0,
            output_gen: 0,
            origin: Pos2::ZERO,
            out: Vec::new(),
            title: String::new(),
            cwd,
            theme_gen: None,
            exit_seen: false,
            prompt_re: regex::Regex::new(&ed.var_str("terminal-prompt-regexp").unwrap_or_else(|| DEFAULT_PROMPT_REGEXP.into())).ok(),
            last_output: None,
            last_input: Cell::new(None),
            fg: None,
            fg_agent: false,
            fg_checked: None,
            busy_title_re: None,
            quiet_after: 1.5,
        })
    }

    /// A cell's size in points.
    pub fn cell_size(&self) -> Vec2 {
        self.cell
    }

    pub fn write(&self, data: &[u8]) {
        if !self.exited {
            self.last_input.set(Some(Instant::now()));
            let _ = nix::unistd::write(self.master.as_ref(), data);
        }
    }

    pub fn send_text(&mut self, text: &str) {
        self.exit_copy_mode();
        for c in text.chars() {
            let chord = KeyChord::ch(c);
            self.encode(&chord, Some(c.to_string()));
        }
        self.flush();
    }

    pub fn paste(&mut self, text: &str) {
        self.exit_copy_mode();
        self.paste_raw(text);
    }

    fn paste_raw(&self, text: &str) {
        let bracketed = self.term.mode(Mode::BRACKETED_PASTE).unwrap_or(false);
        let mut data = Vec::new();
        if bracketed {
            data.extend_from_slice(b"\x1b[200~");
        }
        data.extend_from_slice(text.replace('\n', "\r").as_bytes());
        if bracketed {
            data.extend_from_slice(b"\x1b[201~");
        }
        self.write(&data);
    }

    fn flush(&mut self) {
        if !self.out.is_empty() {
            let out = std::mem::take(&mut self.out);
            self.write(&out);
        }
    }

    /// Encode one chord through libghostty's key encoder.
    fn encode(&mut self, chord: &KeyChord, text: Option<String>) -> bool {
        let (k, ucp) = map_key(chord);
        if k == GKey::Unidentified && text.is_none() && chord.printable().is_none() {
            return false;
        }
        let mut mods = key::Mods::empty();
        if chord.mods.ctrl {
            mods |= key::Mods::CTRL;
        }
        if chord.mods.meta {
            mods |= key::Mods::ALT;
        }
        if chord.mods.shift {
            mods |= key::Mods::SHIFT;
        }
        if chord.mods.sup {
            mods |= key::Mods::SUPER;
        }
        let mut consumed = key::Mods::empty();
        let text = text.or_else(|| chord.printable().map(|c| c.to_string()));
        if let Some(t) = &text {
            if t.chars().any(char::is_uppercase) {
                mods |= key::Mods::SHIFT;
                consumed |= key::Mods::SHIFT;
            }
        }
        let utf8 = if chord.mods.ctrl || chord.mods.meta { None } else { text.as_deref() };
        self.key_ev
            .set_action(key::Action::Press)
            .set_key(k)
            .set_mods(mods)
            .set_consumed_mods(consumed)
            .set_unshifted_codepoint(ucp)
            .set_utf8(utf8);
        let before = self.out.len();
        // libghostty's retry on a full buffer under-reserves and drops the key; leave room.
        self.out.reserve(64);
        let _ = self.key_enc.set_options_from_terminal(&self.term).encode_to_vec(&self.key_ev, &mut self.out);
        if self.out.len() == before {
            if let Some(t) = utf8 {
                self.out.extend_from_slice(t.as_bytes());
            }
        }
        true
    }

    /// Note the foreground program and re-read the activity settings.
    fn sample_foreground(&mut self, ed: &Editor) {
        self.fg_checked = Some(Instant::now());
        let fg = match self.process_status() {
            ProcessStatus::Running(p) => Some(p),
            _ => None,
        };
        self.fg = fg;
        self.fg_agent = self.fg.as_ref().is_some_and(|p| crate::sidebar::is_agent_program(ed, p));
        let re = ed.var_str("terminal-busy-title-regexp").unwrap_or_default();
        if self.busy_title_re.as_ref().map(regex::Regex::as_str) != Some(re.as_str()) {
            self.busy_title_re = (!re.is_empty()).then(|| regex::Regex::new(&re).ok()).flatten();
        }
        self.quiet_after = ed.var("terminal-quiet-seconds").and_then(serde_json::Value::as_f64).unwrap_or(1.5);
    }

    /// Whether a full-screen program (vim, less, htop, Claude Code) has the alternate screen.
    pub fn alt_screen(&self) -> bool {
        self.term.active_screen().is_ok_and(|s| s == Screen::Alternate)
    }

    pub fn exit_copy_mode(&mut self) {
        if self.copy_mode {
            // Leaving normal state on the input line: insert where the copy cursor is.
            if let Some(inp) = self.input() {
                if (inp.first_line..=inp.last_line).contains(&self.cursor.line) {
                    let target = self.cursor.clamp(inp.start, inp.end);
                    self.drive_to(target);
                }
            }
            self.copy_mode = false;
            self.anchor = None;
            self.term.scroll_viewport(ScrollViewport::Bottom);
        }
    }

    pub fn enter_copy_mode(&mut self) {
        if !self.copy_mode {
            self.copy_mode = true;
            let sb = self.term.scrollback_rows().unwrap_or(0);
            let y = self.term.cursor_y().unwrap_or(0) as usize;
            let x = self.term.cursor_x().unwrap_or(0) as usize;
            self.cursor = Pos::new(sb + y, x);
        }
    }

    fn feed(&mut self, data: &[u8]) {
        // Output right after a key is mostly its echo, not the program working.
        let echo = self.last_input.get().is_some_and(|t| t.elapsed() < Duration::from_millis(300));
        if !echo {
            self.last_output = Some(Instant::now());
        }
        self.term.vt_write(data);
        self.output_gen += 1;
        self.invalidate_rows(false);
    }

    /// Drop cached rows that output may have changed: the active screen, or everything after a
    /// resize, a cleared scrollback, or once the scrollback is full and old rows shift out.
    fn invalidate_rows(&mut self, all: bool) {
        let sb = self.term.scrollback_rows().unwrap_or(0);
        let total = self.term.total_rows().unwrap_or(0);
        let full = sb + self.rows as usize >= SCROLLBACK_LINES;
        let keep = if all || full || sb < self.cached_scrollback { 0 } else { self.cached_scrollback };
        let rows = self.row_cache.get_mut();
        rows.truncate(keep);
        rows.resize(total, None);
        self.cached_scrollback = sb;
    }

    /// One screen row as text, one char per cell (blank and spacer cells are spaces).
    fn row_text(&self, y: usize) -> String {
        let mut buf = ['\0'; 16];
        let mut s = String::new();
        for x in 0..self.cols {
            match self.term.grid_ref(Self::point(Pos::new(y, x as usize))) {
                Ok(r) => match r.graphemes(&mut buf).unwrap_or(0) {
                    0 => s.push(' '),
                    n => s.extend(&buf[..n]),
                },
                Err(_) => s.push(' '),
            }
        }
        s
    }

    fn point(p: Pos) -> Point {
        Point::Screen(PointCoordinate { x: p.col as u16, y: p.line as u32 })
    }

    /// The terminal cursor in surface coordinates.
    fn term_cursor(&self) -> Pos {
        let sb = self.term.scrollback_rows().unwrap_or(0);
        Pos::new(sb + self.term.cursor_y().unwrap_or(0) as usize, self.term.cursor_x().unwrap_or(0) as usize)
    }

    /// Feed PTY output until `done` holds or `timeout` passes; returns whether `done` held.
    fn wait_until(&mut self, timeout: Duration, done: impl Fn(&Self) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        while !done(self) {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else { return false };
            match self.rx.recv_timeout(left) {
                Ok(PtyMsg::Data(d)) => self.feed(&d),
                Ok(PtyMsg::Exit) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.exit_seen = true;
                    return false;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => return false,
            }
        }
        true
    }

    fn send_key(&mut self, name: &str, times: usize) {
        for _ in 0..times {
            self.encode(&KeyChord::named(name), None);
        }
        self.flush();
    }

    fn row_wraps(&self, y: usize) -> (bool, bool) {
        let row = self.term.grid_ref(Self::point(Pos::new(y, 0))).and_then(|r| r.row());
        row.map(|r| (r.is_wrap_continuation().unwrap_or(false), r.is_wrapped().unwrap_or(false))).unwrap_or((false, false))
    }

    /// Cells between `a` and `b` (a <= b) across soft-wrapped rows, and whether they are all ASCII
    /// (then one cell is one character, and one cursor key moves over exactly one cell).
    fn cell_distance(&self, a: Pos, b: Pos) -> (usize, bool) {
        let cols = self.cols as usize;
        let n = (b.line * cols + b.col).saturating_sub(a.line * cols + a.col);
        let mut ascii = true;
        let mut buf = ['\0'; 16];
        let mut p = a;
        for _ in 0..n {
            if let Ok(r) = self.term.grid_ref(Self::point(p)) {
                let k = r.graphemes(&mut buf).unwrap_or(0);
                let wide = r.cell().and_then(|c| c.wide()).is_ok_and(|w| w != CellWide::Narrow);
                if wide || k > 1 || (k == 1 && !buf[0].is_ascii()) {
                    ascii = false;
                    break;
                }
            }
            p = if p.col + 1 >= cols { Pos::new(p.line + 1, 0) } else { Pos::new(p.line, p.col + 1) };
        }
        (n, ascii)
    }

    /// The live input region on the cursor's line, while the process runs.
    fn input(&self) -> Option<Input> {
        if self.exited {
            return None;
        }
        // Full-screen apps edit only on a recognized prompt row (Claude Code's `❯`), so vim or
        // less never get keys replayed at them.
        let alt = self.term.active_screen().ok()? == Screen::Alternate;
        if alt && !self.term.is_cursor_visible().unwrap_or(false) {
            return None;
        }
        let cur = self.term_cursor();
        let total = self.term.total_rows().ok()?;
        let mut first = cur.line;
        while first > 0 && self.row_wraps(first).0 {
            first -= 1;
        }
        let mut last = cur.line;
        while last + 1 < total && self.row_wraps(last).1 {
            last += 1;
        }
        // Prompt end: past the last OSC 133 prompt cell before the cursor, else the regexp.
        let mut start = None;
        let mut p = Pos::new(first, 0);
        while p < cur {
            let prompt = self.term.grid_ref(Self::point(p)).and_then(|r| r.cell()).and_then(|c| c.semantic_content());
            if prompt.is_ok_and(|c| c == CellSemanticContent::Prompt) {
                start = Some(p);
            }
            p = if p.col + 1 >= self.cols as usize { Pos::new(p.line + 1, 0) } else { Pos::new(p.line, p.col + 1) };
        }
        let start = match start {
            Some(p) if p.col + 1 >= self.cols as usize => Pos::new(p.line + 1, 0),
            Some(p) => Pos::new(p.line, p.col + 1),
            None => {
                let text = self.line(first);
                let m = self.prompt_re.as_ref().and_then(|re| re.find(&text)).map(|m| char_len(&text[..m.end()]));
                if alt && m.is_none() {
                    return None;
                }
                Pos::new(first, m.unwrap_or(0))
            }
        };
        let start = start.min(cur);
        let mut end = Pos::new(last, self.line_len(last)).max(cur);
        if end > cur && self.is_suggestion(cur, end) {
            end = cur;
        }
        Some(Input { start, end, first_line: first, last_line: last })
    }

    /// Whether `from..to` is a greyed-out autosuggestion (zsh-autosuggestions, fish): one dim,
    /// unsaturated color run that differs from the typed text before it. Moving right into it
    /// would accept it.
    fn is_suggestion(&self, from: Pos, to: Pos) -> bool {
        let style = |p: Pos| self.term.grid_ref(Self::point(p)).and_then(|r| r.style()).ok();
        let grey = |st: &libghostty_vt::style::Style| {
            st.faint
                || match st.fg_color {
                    StyleColor::Palette(i) => i.0 == 8,
                    StyleColor::Rgb(c) => {
                        let (mx, mn) = (c.r.max(c.g).max(c.b), c.r.min(c.g).min(c.b));
                        mx < 192 && mx - mn < 51
                    }
                    StyleColor::None => false,
                }
        };
        let Some(first) = style(from) else { return false };
        if !grey(&first) || from.col == 0 || style(Pos::new(from.line, from.col - 1)).is_some_and(|b| b.fg_color == first.fg_color && b.faint == first.faint) {
            return false;
        }
        let mut p = from;
        while p < to {
            if style(p).is_none_or(|st| st.fg_color != first.fg_color || st.faint != first.faint) && self.term.grid_ref(Self::point(p)).and_then(|r| r.cell()).and_then(|c| c.has_text()).unwrap_or(false) {
                return false;
            }
            p = if p.col + 1 >= self.cols as usize { Pos::new(p.line + 1, 0) } else { Pos::new(p.line, p.col + 1) };
        }
        true
    }

    /// Move the program's cursor to `target` with arrow keys, confirming each step by its echo.
    /// Runs of ASCII cells go in one batch; anything else one key at a time.
    fn drive_to(&mut self, target: Pos) -> bool {
        loop {
            let cur = self.term_cursor();
            if cur == target {
                return true;
            }
            let fwd = target > cur;
            let (a, b) = ordered(cur, target);
            let (n, ascii) = self.cell_distance(a, b);
            let n = if ascii { n.max(1) } else { 1 };
            self.send_key(if fwd { "right" } else { "left" }, n);
            self.wait_until(ECHO_TIMEOUT, |t| t.term_cursor() == target);
            let now = self.term_cursor();
            let progressed = if fwd { now > cur && now <= target } else { now < cur && now >= target };
            if !progressed {
                return now == target;
            }
        }
    }

    fn viewport_top(&self) -> usize {
        self.term.scrollbar().map(|s| s.offset as usize).unwrap_or(0)
    }

    fn scroll_cursor_into_view(&mut self) {
        let top = self.viewport_top();
        let rows = self.rows as usize;
        let l = self.cursor.line;
        if l < top {
            self.term.scroll_viewport(ScrollViewport::Row(l));
        } else if l >= top + rows {
            self.term.scroll_viewport(ScrollViewport::Row(l + 1 - rows));
        }
    }
}

/// The executable name of process `pid`.
fn process_name(pid: i32) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let mut buf = [0u8; 256];
        let n = unsafe { nix::libc::proc_name(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
        (n > 0).then(|| String::from_utf8_lossy(&buf[..n as usize]).into_owned())
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::fs::read_to_string(format!("/proc/{pid}/comm")).ok().map(|s| s.trim().to_string())
    }
}

/// Draw a cursor in cell `r`; a block repaints the character under it in the background color.
fn paint_cursor(painter: &egui::Painter, r: Rect, shape: CursorShape, color: Color32, ch: Option<char>, font: &FontId, bg: Color32) {
    match shape {
        CursorShape::Block => {
            painter.rect_filled(r, 1.0, color);
            if let Some(c) = ch {
                painter.text(r.left_top(), egui::Align2::LEFT_TOP, c, font.clone(), bg);
            }
        }
        CursorShape::Bar => {
            painter.rect_filled(Rect::from_min_size(r.min, egui::vec2(2.0, r.height())), 0.0, color);
        }
        CursorShape::Underline => {
            painter.rect_filled(Rect::from_min_max(egui::pos2(r.min.x, r.max.y - 2.0), r.max), 0.0, color);
        }
        CursorShape::Hollow => {
            painter.rect_stroke(r, 1.0, Stroke::new(1.0, color), egui::StrokeKind::Inside);
        }
    }
}

/// Map a chord to a libghostty key and its unshifted codepoint.
fn map_key(chord: &KeyChord) -> (GKey, char) {
    match &chord.key {
        Key::Named(n) => (
            match n.as_str() {
                "return" => GKey::Enter,
                "tab" => GKey::Tab,
                "backspace" => GKey::Backspace,
                "escape" => GKey::Escape,
                "space" => GKey::Space,
                "delete" => GKey::Delete,
                "insert" => GKey::Insert,
                "home" => GKey::Home,
                "end" => GKey::End,
                "pageup" => GKey::PageUp,
                "pagedown" => GKey::PageDown,
                "up" => GKey::ArrowUp,
                "down" => GKey::ArrowDown,
                "left" => GKey::ArrowLeft,
                "right" => GKey::ArrowRight,
                "f1" => GKey::F1,
                "f2" => GKey::F2,
                "f3" => GKey::F3,
                "f4" => GKey::F4,
                "f5" => GKey::F5,
                "f6" => GKey::F6,
                "f7" => GKey::F7,
                "f8" => GKey::F8,
                "f9" => GKey::F9,
                "f10" => GKey::F10,
                "f11" => GKey::F11,
                "f12" => GKey::F12,
                _ => GKey::Unidentified,
            },
            if n == "space" { ' ' } else { '\0' },
        ),
        Key::Char(c) => {
            let l = c.to_ascii_lowercase();
            let k = match l {
                'a' => GKey::A,
                'b' => GKey::B,
                'c' => GKey::C,
                'd' => GKey::D,
                'e' => GKey::E,
                'f' => GKey::F,
                'g' => GKey::G,
                'h' => GKey::H,
                'i' => GKey::I,
                'j' => GKey::J,
                'k' => GKey::K,
                'l' => GKey::L,
                'm' => GKey::M,
                'n' => GKey::N,
                'o' => GKey::O,
                'p' => GKey::P,
                'q' => GKey::Q,
                'r' => GKey::R,
                's' => GKey::S,
                't' => GKey::T,
                'u' => GKey::U,
                'v' => GKey::V,
                'w' => GKey::W,
                'x' => GKey::X,
                'y' => GKey::Y,
                'z' => GKey::Z,
                '0' => GKey::Digit0,
                '1' => GKey::Digit1,
                '2' => GKey::Digit2,
                '3' => GKey::Digit3,
                '4' => GKey::Digit4,
                '5' => GKey::Digit5,
                '6' => GKey::Digit6,
                '7' => GKey::Digit7,
                '8' => GKey::Digit8,
                '9' => GKey::Digit9,
                '-' => GKey::Minus,
                '=' => GKey::Equal,
                '[' => GKey::BracketLeft,
                ']' => GKey::BracketRight,
                '\\' => GKey::Backslash,
                ';' => GKey::Semicolon,
                '\'' => GKey::Quote,
                ',' => GKey::Comma,
                '.' => GKey::Period,
                '/' => GKey::Slash,
                '`' => GKey::Backquote,
                _ => GKey::Unidentified,
            };
            (k, if k == GKey::Unidentified { '\0' } else { l })
        }
    }
}

impl Buffer for TerminalBuffer {
    fn kind(&self) -> &str {
        "terminal"
    }

    fn poll(&mut self, ed: &mut Editor) {
        let mut got = false;
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                PtyMsg::Data(d) => {
                    self.feed(&d);
                    got = true;
                }
                PtyMsg::Exit => self.exit_seen = true,
            }
        }
        if self.exit_seen && !self.exited {
            self.exited = true;
            let pid = self.child;
            std::thread::spawn(move || {
                let _ = nix::sys::wait::waitpid(pid, None);
            });
            ed.run_hook("terminal-exited", &serde_json::Value::Null);
        }
        if got {
            if let Ok(t) = self.term.title() {
                self.title = t.to_string();
            }
        }
        if self.fg_checked.is_none_or(|t| t.elapsed() > Duration::from_millis(400)) {
            self.sample_foreground(ed);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, rect: Rect, focused: bool, ed: &mut Editor) {
        let theme = ed.theme.clone();
        let font = FontId::monospace(theme.mono_size);
        let (cw, ch) = ui.fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
        self.cell = Vec2::new(cw.max(1.0), ch.max(1.0));
        let cols = (((rect.width() - 2.0 * PAD) / self.cell.x).floor() as u16).max(2);
        let rows = (((rect.height() - 2.0 * PAD) / self.cell.y).floor() as u16).max(2);
        let ppp = ui.ctx().pixels_per_point();
        let px = (((self.cell.x * ppp).round() as u32) << 16) | (self.cell.y * ppp).round() as u32;
        // Font size changes resize cells without changing the grid: images need the new size.
        if (cols, rows) != (self.cols, self.rows) || px != self.cell_px.load(Ordering::Relaxed) {
            self.cols = cols;
            self.rows = rows;
            self.cell_px.store(px, Ordering::Relaxed);
            let _ = self.term.resize(cols, rows, px >> 16, px & 0xffff);
            self.grid.store(((cols as u32) << 16) | rows as u32, Ordering::Relaxed);
            let ws = nix::pty::Winsize {
                ws_row: rows,
                ws_col: cols,
                ws_xpixel: (cols as f32 * self.cell.x * ppp) as u16,
                ws_ypixel: (rows as f32 * self.cell.y * ppp) as u16,
            };
            unsafe {
                nix::libc::ioctl(self.master.as_raw_fd(), nix::libc::TIOCSWINSZ, &ws);
            }
            self.invalidate_rows(true);
        }

        // Mouse wheel scrolls the scrollback (or is sent to apps that track the mouse).
        let resp = ui.interact(rect, ui.scope_id().with(("term", rect.min.x as i32, rect.min.y as i32)), egui::Sense::click());
        if resp.hovered() {
            let dy = ui.input(|i| i.smooth_scroll_delta.y);
            if dy.abs() > 0.5 {
                let lines = (-dy / self.cell.y).round() as isize;
                if self.term.is_mouse_tracking().unwrap_or(false) && !self.copy_mode {
                    let seq: &[u8] = if lines < 0 { b"\x1b[A" } else { b"\x1b[B" };
                    if self.term.mode(Mode::new(1007, libghostty_vt::terminal::ModeKind::Dec)).unwrap_or(false) {
                        for _ in 0..lines.unsigned_abs().min(10) {
                            self.write(seq);
                        }
                    }
                } else if lines != 0 {
                    self.term.scroll_viewport(ScrollViewport::Delta(lines));
                }
            }
        }

        // Theme colors become the terminal's defaults (apps can still override via OSC).
        if self.theme_gen != Some(ed.theme_generation) {
            self.theme_gen = Some(ed.theme_generation);
            let c = |c: egui::Color32| RgbColor { r: c.r(), g: c.g(), b: c.b() };
            if let Ok(mut pal) = self.term.default_color_palette() {
                for (i, a) in theme.ansi.iter().enumerate() {
                    pal.0[i] = c(*a);
                }
                let _ = self.term.set_default_color_palette(Some(pal));
            }
            let _ = self.term.set_default_fg_color(Some(c(theme.fg)));
            let _ = self.term.set_default_bg_color(Some(c(theme.bg_alt)));
            let _ = self.term.set_default_cursor_color(Some(c(theme.cursor)));
        }
        let painter = ui.painter_at(rect);
        let origin = rect.min + Vec2::splat(PAD);
        self.origin = origin;
        // Vim's state picks the shape (bar in insert); full-screen apps keep the one they ask for.
        let vim_shape = if focused { ed.cursor_shape(ed.current_buffer()) } else { None };
        let alt = self.term.active_screen().is_ok_and(|s| s == Screen::Alternate);
        let top = self.viewport_top();
        let Ok(snapshot) = self.render.update(&self.term) else { return };
        let colors = snapshot.colors().ok();
        let default_bg = colors.as_ref().map(|c| rgb(c.background)).unwrap_or(theme.bg);
        let default_fg = colors.as_ref().map(|c| rgb(c.foreground)).unwrap_or(theme.fg);
        let bg = default_bg;
        painter.rect_filled(rect, 0.0, bg);
        let px = self.cell_px.load(Ordering::Relaxed);
        let image_grid = super::term_images::Grid { origin, cell: self.cell, cell_px: (px >> 16, px & 0xffff), clip: rect };
        self.images.begin(&self.term);
        for layer in [libghostty_vt::kitty::graphics::Layer::BelowBg, libghostty_vt::kitty::graphics::Layer::BelowText] {
            self.images.paint(&self.term, layer, &painter, &image_grid);
        }
        // The copy-mode (vim visual) selection is a cell background, so its text stays readable.
        let lines = focused && crate::packages::vim::selection_kind(ed, ed.current_buffer()) == crate::text::SelectionKind::Lines;
        let sel = if self.copy_mode { self.anchor.map(|a| ordered(a, self.cursor)) } else { None };
        // Visual-line selects whole rows.
        let sel = sel.map(|(a, b)| if lines { (Pos::new(a.line, 0), Pos::new(b.line, usize::MAX)) } else { (a, b) });
        let Ok(mut row_iter) = self.rows_it.update(&snapshot) else { return };
        let mut y = origin.y;
        let mut text = String::new();
        let mut line = top;
        let mut placeholders = Vec::new();
        let mut row_cells = Vec::new();
        // Text is painted after every cell background, so glyphs that overhang their cell
        // (icons, italics) aren't cut by the next cell's background.
        let mut glyphs: Vec<egui::Shape> = Vec::new();
        // Nerd Font icons wait for the end of their row: they may spread into a blank next cell.
        let mut icons: Vec<(usize, f32, char, Color32)> = Vec::new();
        let mut blank: Vec<bool> = Vec::new();
        while let Some(row) = row_iter.next() {
            row_cells.clear();
            icons.clear();
            blank.clear();
            let Ok(mut cell_iter) = self.cells_it.update(row) else { break };
            let mut x = origin.x;
            let mut col = 0;
            let mut job = LayoutJob::default();
            let mut run_start_x = x;
            let flush = |job: &mut LayoutJob, run_x: f32, glyphs: &mut Vec<egui::Shape>| {
                if !job.text.is_empty() {
                    let g = painter.layout_job(std::mem::take(job));
                    glyphs.push(egui::Shape::galley(egui::pos2(run_x, y), g, default_fg));
                }
            };
            while let Some(cell) = cell_iter.next() {
                let n = cell.graphemes_len().unwrap_or(0);
                let mut fg = cell.fg_color().ok().flatten().map(rgb).unwrap_or(default_fg);
                let mut cbg = cell.bg_color().ok().flatten().map(rgb);
                let mut bold = false;
                let mut italic = false;
                let mut underline = false;
                if cell.has_styling().unwrap_or(false) {
                    if let Ok(s) = cell.style() {
                        if s.inverse {
                            let b = cbg.unwrap_or(bg);
                            cbg = Some(fg);
                            fg = b;
                        }
                        if s.faint {
                            fg = fg.gamma_multiply(0.6);
                        }
                        bold = s.bold;
                        italic = s.italic;
                        underline = !matches!(s.underline, libghostty_vt::style::Underline::None);
                    }
                }
                let p = Pos::new(line, col);
                if cell.is_selected().unwrap_or(false) || sel.is_some_and(|(a, b)| a <= p && p <= b) {
                    cbg = Some(theme.selection);
                }
                if let Some(b) = cbg {
                    painter.rect_filled(Rect::from_min_size(egui::pos2(x, y), self.cell), 0.0, b);
                }
                if n > 0 {
                    let _ = cell.graphemes_utf8(&mut text);
                }
                blank.push(n == 0 || text.trim().is_empty());
                if n > 0 {
                    // Kitty image placeholders show part of an image instead of a glyph.
                    if text.starts_with(super::term_images::PLACEHOLDER) {
                        if let Ok(s) = cell.style()
                            && let Some(c) = super::term_images::PlaceholderCell::parse(col as u32, (line - top) as i32, &text, &s.fg_color, &s.underline_color)
                        {
                            row_cells.push(c);
                        }
                        flush(&mut job, run_start_x, &mut glyphs);
                        x += self.cell.x;
                        col += 1;
                        continue;
                    }
                    // Powerline separators are shapes filling the cell; icons are fitted later.
                    let mut chars = text.chars();
                    if let (Some(ch), None) = (chars.next(), chars.next()) {
                        let cell_rect = Rect::from_min_size(egui::pos2(x, y), self.cell);
                        if super::term_glyphs::powerline(ch, cell_rect, fg, &mut glyphs) {
                            flush(&mut job, run_start_x, &mut glyphs);
                            x += self.cell.x;
                            col += 1;
                            continue;
                        }
                        if super::term_glyphs::is_icon(ch) {
                            flush(&mut job, run_start_x, &mut glyphs);
                            icons.push((col, x, ch, fg));
                            x += self.cell.x;
                            col += 1;
                            continue;
                        }
                    }
                    let fmt = TextFormat {
                        font_id: font.clone(),
                        color: if bold { fg.gamma_multiply(1.0) } else { fg },
                        italics: italic,
                        underline: if underline { Stroke::new(1.0, fg) } else { Stroke::NONE },
                        ..Default::default()
                    };
                    if text.is_ascii() && text.len() == 1 {
                        if job.text.is_empty() {
                            run_start_x = x;
                        }
                        job.append(&text, 0.0, fmt);
                    } else {
                        // Wide or complex graphemes are placed at their exact cell.
                        flush(&mut job, run_start_x, &mut glyphs);
                        let mut single = LayoutJob::default();
                        single.append(&text, 0.0, fmt);
                        glyphs.push(egui::Shape::galley(egui::pos2(x, y), painter.layout_job(single), fg));
                    }
                    if bold {
                        // Fake bold: a second pass offset by a fraction of a pixel.
                    }
                } else {
                    flush(&mut job, run_start_x, &mut glyphs);
                }
                x += self.cell.x;
                col += 1;
            }
            flush(&mut job, run_start_x, &mut glyphs);
            for &(c, ix, ch, color) in &icons {
                let cells = if blank.get(c + 1).copied().unwrap_or(false) { 2 } else { 1 };
                if !super::term_glyphs::icon(ui.ctx(), ch, egui::pos2(ix, y), self.cell, cells, color, &mut glyphs) {
                    let g = painter.layout_no_wrap(ch.to_string(), font.clone(), color);
                    glyphs.push(egui::Shape::galley(egui::pos2(ix, y), g, color));
                }
            }
            placeholders.extend(super::term_images::runs(&row_cells));
            y += self.cell.y;
            line += 1;
        }

        painter.extend(glyphs);
        self.images.paint_virtual(&self.term, &placeholders, &painter, &image_grid);
        self.images.paint(&self.term, libghostty_vt::kitty::graphics::Layer::AboveText, &painter, &image_grid);

        // Terminal cursor (hidden in copy mode), painted after the snapshot is released.
        let mut live = None;
        if !self.copy_mode {
            if let Ok(c) = snapshot.cursor() {
                if c.visible {
                    if let Some(vp) = c.viewport {
                        let r = Rect::from_min_size(origin + egui::vec2(vp.x as f32 * self.cell.x, vp.y as f32 * self.cell.y), self.cell);
                        let color = colors.as_ref().and_then(|c| c.cursor).map(rgb).unwrap_or(theme.cursor);
                        let program = match c.visual_style {
                            CursorVisualStyle::Bar => CursorShape::Bar,
                            CursorVisualStyle::Underline => CursorShape::Underline,
                            CursorVisualStyle::BlockHollow => CursorShape::Hollow,
                            _ => CursorShape::Block,
                        };
                        let shape = match vim_shape {
                            _ if !focused => CursorShape::Hollow,
                            Some(s) if !alt => s,
                            _ => program,
                        };
                        live = Some((r, shape, color, Pos::new(top + vp.y as usize, vp.x as usize)));
                    }
                }
            }
        }
        let _ = snapshot.clean();
        let cursor_char = |p: Pos| self.char_at(p).filter(|c| !c.is_whitespace() && *c != '\n');
        if let Some((r, shape, color, p)) = live {
            paint_cursor(&painter, r, shape, color, cursor_char(p), &font, default_bg);
        }

        // Copy-mode cursor.
        if self.copy_mode {
            if self.cursor.line >= top && self.cursor.line < top + self.rows as usize {
                let r = Rect::from_min_size(
                    origin + egui::vec2(self.cursor.col as f32 * self.cell.x, (self.cursor.line - top) as f32 * self.cell.y),
                    self.cell,
                );
                let shape = if focused { vim_shape.unwrap_or(CursorShape::Block) } else { CursorShape::Hollow };
                paint_cursor(&painter, r, shape, theme.cursor, cursor_char(self.cursor), &font, default_bg);
            }
            painter.text(rect.right_top() + egui::vec2(-10.0, 6.0), egui::Align2::RIGHT_TOP, "COPY", FontId::monospace(10.0), theme.yellow);
        }
        if self.exited {
            painter.text(rect.center_bottom() - egui::vec2(0.0, 18.0), egui::Align2::CENTER_CENTER, "[process exited]", theme.mono(), theme.fg_dim);
        }
    }

    fn handle_key(&mut self, chord: &KeyChord, _ed: &mut Editor) -> bool {
        if self.exited {
            return false;
        }
        if self.copy_mode {
            if chord.is("escape") || chord.key == Key::Char('q') {
                self.exit_copy_mode();
            }
            return true;
        }
        let ok = self.encode(chord, None);
        self.flush();
        if ok {
            self.term.scroll_viewport(ScrollViewport::Bottom);
        }
        ok
    }

    fn handle_text(&mut self, text: &str, _ed: &mut Editor) -> bool {
        if self.exited {
            return false;
        }
        self.send_text(text);
        true
    }

    fn handle_paste(&mut self, text: &str, _ed: &mut Editor) -> bool {
        self.paste(text);
        true
    }

    fn surface(&mut self) -> Option<&mut dyn TextSurface> {
        Some(self)
    }

    fn modeline(&self) -> Modeline {
        let mut status = Vec::new();
        if let Ok(pwd) = self.term.pwd() {
            if !pwd.is_empty() {
                status.push(pwd.rsplit('/').next().unwrap_or(pwd).to_string());
            }
        }
        Modeline {
            title: self.title.clone(),
            status,
            busy: false,
        }
    }

    /// Agents work while their output flows (or their title shows a spinner) and wait for
    /// the user when quiet; other programs work until they exit, unless they are full-screen.
    fn activity(&self) -> Activity {
        if self.exited {
            return Activity::new(ActivityState::Exited, "exited");
        }
        let Some(p) = self.fg.clone() else { return Activity::default() };
        let busy_title = self.busy_title_re.as_ref().is_some_and(|r| r.is_match(&self.title));
        let quiet = self.last_output.is_none_or(|t| t.elapsed().as_secs_f64() > self.quiet_after);
        let state = if self.fg_agent {
            if busy_title || !quiet { ActivityState::Working } else { ActivityState::Waiting }
        } else if self.alt_screen() {
            ActivityState::Idle
        } else {
            ActivityState::Working
        };
        Activity { state, detail: p.clone(), since: None, program: Some(p) }
    }

    fn title(&self) -> String {
        self.title.clone()
    }

    fn directory(&self) -> Option<std::path::PathBuf> {
        // The shell reports its directory via OSC 7 (file://host/path) when integrated.
        let pwd = self.term.pwd().ok().filter(|p| !p.is_empty()).map(|p| {
            url::Url::parse(p).ok().and_then(|u| u.to_file_path().ok()).unwrap_or_else(|| std::path::PathBuf::from(p))
        });
        Some(pwd.unwrap_or_else(|| self.cwd.clone()))
    }

    fn process_status(&self) -> ProcessStatus {
        if self.exited {
            return ProcessStatus::None;
        }
        // The terminal's foreground process group is the shell itself while it waits for input.
        match nix::unistd::tcgetpgrp(self.master.as_ref()) {
            Ok(pg) if pg != self.child => ProcessStatus::Running(process_name(pg.as_raw()).unwrap_or_else(|| format!("pid {pg}"))),
            _ => ProcessStatus::Idle,
        }
    }

    fn on_kill(&mut self, _ed: &mut Editor) {
        if !self.exited {
            let _ = nix::sys::signal::kill(self.child, nix::sys::signal::Signal::SIGHUP);
            let pid = self.child;
            std::thread::spawn(move || {
                let _ = nix::sys::wait::waitpid(pid, None);
            });
        }
    }

    impl_any!();
}

impl TextSurface for TerminalBuffer {
    fn line_count(&self) -> usize {
        self.term.total_rows().unwrap_or(0).max(1)
    }
    fn line(&self, i: usize) -> String {
        let mut rows = self.row_cache.borrow_mut();
        match rows.get_mut(i) {
            Some(r) => r.get_or_insert_with(|| self.row_text(i).trim_end().to_string()).clone(),
            None => self.row_text(i).trim_end().to_string(),
        }
    }
    fn cursor(&self) -> Pos {
        if self.copy_mode { self.cursor } else { self.term_cursor() }
    }
    fn set_cursor(&mut self, p: Pos) {
        self.enter_copy_mode();
        self.cursor = self.clamp(p);
        self.scroll_cursor_into_view();
    }
    fn anchor(&self) -> Option<Pos> {
        self.anchor
    }
    fn set_anchor(&mut self, a: Option<Pos>) {
        if a.is_some() {
            self.enter_copy_mode();
        }
        self.anchor = a;
    }
    fn visible_lines(&self) -> Range<usize> {
        let top = self.viewport_top();
        top..top + self.rows as usize
    }
    fn screen_rect(&self, p: Pos) -> Option<Rect> {
        let top = self.viewport_top();
        if p.line < top || p.line >= top + self.rows as usize {
            return None;
        }
        Some(Rect::from_min_size(
            self.origin + egui::vec2(p.col as f32 * self.cell.x, (p.line - top) as f32 * self.cell.y),
            self.cell,
        ))
    }
    fn reveal_cursor(&mut self) {
        self.scroll_cursor_into_view();
    }
    /// The live input line is editable; so is the end of the line above it, so linewise
    /// operators whose range starts at the previous newline still reach the input.
    fn editable_at(&self, p: Pos) -> bool {
        self.input().is_some_and(|i| (i.first_line..=i.last_line).contains(&p.line) || (p.line + 1 == i.first_line && p.col >= self.line_len(p.line)))
    }
    fn text_start(&self, line: usize) -> usize {
        self.input().filter(|i| i.start.line == line).map_or(0, |i| i.start.col)
    }
    fn insert(&mut self, at: Pos, text: &str) -> Pos {
        let Some(inp) = self.input() else { return at };
        let at = at.clamp(inp.start, inp.end);
        if !text.is_empty() && self.drive_to(at) {
            let before = self.term_cursor();
            if self.term.mode(Mode::BRACKETED_PASTE).unwrap_or(false) {
                self.paste_raw(text);
            } else {
                for c in text.chars().filter(|c| *c != '\n') {
                    self.encode(&KeyChord::ch(c), Some(c.to_string()));
                }
                self.flush();
            }
            let cols = self.cols as usize;
            let expect = (text.is_ascii() && !text.contains('\n')).then(|| {
                let i = before.line * cols + before.col + text.len();
                Pos::new(i / cols, i % cols)
            });
            self.wait_until(ECHO_TIMEOUT, |t| expect.map_or(t.term_cursor() != before, |e| t.term_cursor() == e));
        }
        self.cursor = self.term_cursor();
        self.cursor
    }
    fn delete(&mut self, from: Pos, to: Pos) -> String {
        let Some(inp) = self.input() else { return String::new() };
        let (a, b) = ordered(from, to);
        let (a, b) = (a.clamp(inp.start, inp.end), b.clamp(inp.start, inp.end));
        let text = if a < b { self.text_range(a, b) } else { String::new() };
        if a < b && self.drive_to(b) {
            // Backspace from the end, stopping on the echoed cursor rather than a key count:
            // one backspace may remove a whole grapheme.
            loop {
                let cur = self.term_cursor();
                if cur <= a {
                    break;
                }
                let (n, ascii) = self.cell_distance(a, cur);
                self.send_key("backspace", if ascii { n } else { 1 });
                self.wait_until(ECHO_TIMEOUT, |t| t.term_cursor() <= a);
                if self.term_cursor() >= cur {
                    break;
                }
            }
        }
        self.cursor = self.term_cursor();
        text
    }
    /// Line editors' undo (readline, zle, Claude Code) is C-_.
    fn undo(&mut self) -> bool {
        if self.input().is_none() {
            return false;
        }
        self.write(b"\x1f");
        let seen = self.output_gen;
        if self.wait_until(ECHO_TIMEOUT, |t| t.output_gen != seen) {
            self.wait_until(Duration::from_millis(30), |_| false);
        }
        self.cursor = self.term_cursor();
        true
    }
}
