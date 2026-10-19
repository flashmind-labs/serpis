//! Terminal buffer backed by libghostty-vt.
//!
//! The PTY is read on a background thread; bytes are fed to the VT engine on the UI thread.
//! Unbound keys are encoded by libghostty's key encoder (honouring application cursor mode,
//! the Kitty keyboard protocol, ...). The scrollback is exposed as a [`TextSurface`]
//! ("copy mode"), so vim normal state, search and yank work on terminal output.

use std::ops::Range;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, mpsc};

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Pos2, Rect, Stroke, Vec2};
use libghostty_vt::key::{self, Key as GKey};
use libghostty_vt::render::{CellIterator, RenderState, RowIterator};
use libghostty_vt::style::RgbColor;
use libghostty_vt::terminal::{Mode, Point, PointCoordinate, ScrollViewport, Terminal};

use crate::buffer::{Buffer, Modeline};
use crate::editor::Editor;
use crate::impl_any;
use crate::keys::{Key, KeyChord};
use crate::text::{Pos, TextSurface, char_len, ordered};

const TERM: &str = "xterm-256color";
const PAD: f32 = 6.0;

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
    cell: Vec2,
    cols: u16,
    rows: u16,
    pub exited: bool,
    /// Copy mode: a cursor and selection over the scrollback, driven by motions.
    pub copy_mode: bool,
    cursor: Pos,
    anchor: Option<Pos>,
    text_cache: Option<Vec<String>>,
    origin: Pos2,
    out: Vec<u8>,
    title: String,
    cwd: std::path::PathBuf,
}

fn rgb(c: RgbColor) -> Color32 {
    Color32::from_rgb(c.r, c.g, c.b)
}

fn spawn_shell(cwd: &std::path::Path, cols: u16, rows: u16) -> anyhow::Result<(OwnedFd, nix::unistd::Pid)> {
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
    pub fn new(ed: &mut Editor) -> anyhow::Result<Self> {
        let (cols, rows) = (100u16, 30u16);
        let (master, child) = spawn_shell(&ed.cwd, cols, rows)?;
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
        let mut term = Terminal::new(cols, rows).map_err(|e| anyhow::anyhow!("libghostty: {e:?}"))?;
        term.set_scrollback_max_lines(Some(20_000)).map_err(|e| anyhow::anyhow!("{e:?}"))?;
        let _ = term.set_terminfo_name(TERM);
        {
            let w = master.clone();
            let g = grid.clone();
            term.on_pty_write(move |_t, data| {
                let _ = nix::unistd::write(w.as_ref(), data);
            })
            .map_err(|e| anyhow::anyhow!("{e:?}"))?
            .on_size(move |_t| {
                let v = g.load(Ordering::Relaxed);
                Some(libghostty_vt::terminal::SizeReportSize {
                    rows: (v & 0xffff) as u16,
                    columns: (v >> 16) as u16,
                    cell_width: 8,
                    cell_height: 16,
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
            cell: Vec2::new(8.0, 16.0),
            cols,
            rows,
            exited: false,
            copy_mode: false,
            cursor: Pos::ZERO,
            anchor: None,
            text_cache: None,
            origin: Pos2::ZERO,
            out: Vec::new(),
            title: String::new(),
            cwd: ed.cwd.clone(),
        })
    }

    pub fn write(&self, data: &[u8]) {
        if !self.exited {
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
        let _ = self.key_enc.set_options_from_terminal(&self.term).encode_to_vec(&self.key_ev, &mut self.out);
        if self.out.len() == before {
            if let Some(t) = utf8 {
                self.out.extend_from_slice(t.as_bytes());
            }
        }
        true
    }

    pub fn exit_copy_mode(&mut self) {
        if self.copy_mode {
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

    fn lines(&mut self) -> &Vec<String> {
        if self.text_cache.is_none() {
            let total = self.term.total_rows().unwrap_or(0);
            let mut out = Vec::with_capacity(total);
            let mut buf = ['\0'; 16];
            for y in 0..total {
                let mut s = String::new();
                for x in 0..self.cols {
                    let pt = Point::Screen(PointCoordinate { x, y: y as u32 });
                    match self.term.grid_ref(pt) {
                        Ok(r) => {
                            let n = r.graphemes(&mut buf).unwrap_or(0);
                            if n == 0 {
                                s.push(' ');
                            } else {
                                s.extend(&buf[..n]);
                            }
                        }
                        Err(_) => s.push(' '),
                    }
                }
                out.push(s.trim_end().to_string());
            }
            self.text_cache = Some(out);
        }
        self.text_cache.as_ref().unwrap()
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
                    self.term.vt_write(&d);
                    got = true;
                }
                PtyMsg::Exit => {
                    if !self.exited {
                        self.exited = true;
                        let pid = self.child;
                        std::thread::spawn(move || {
                            let _ = nix::sys::wait::waitpid(pid, None);
                        });
                        ed.run_hook("terminal-exited", &serde_json::Value::Null);
                    }
                }
            }
        }
        if got {
            self.text_cache = None;
            if let Ok(t) = self.term.title() {
                self.title = t.to_string();
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, rect: Rect, focused: bool, ed: &mut Editor) {
        let theme = ed.theme.clone();
        let font = FontId::monospace(theme.mono_size);
        let (cw, ch) = ui.fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
        self.cell = Vec2::new(cw.max(1.0), ch.max(1.0));
        let cols = (((rect.width() - 2.0 * PAD) / self.cell.x).floor() as u16).max(2);
        let rows = (((rect.height() - 2.0 * PAD) / self.cell.y).floor() as u16).max(2);
        if (cols, rows) != (self.cols, self.rows) {
            self.cols = cols;
            self.rows = rows;
            let ppp = ui.ctx().pixels_per_point();
            let _ = self.term.resize(cols, rows, (self.cell.x * ppp) as u32, (self.cell.y * ppp) as u32);
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
            self.text_cache = None;
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

        let painter = ui.painter_at(rect);
        let origin = rect.min + Vec2::splat(PAD);
        self.origin = origin;
        let Ok(snapshot) = self.render.update(&self.term) else { return };
        let colors = snapshot.colors().ok();
        let default_bg = colors.as_ref().map(|c| rgb(c.background)).unwrap_or(theme.bg);
        let default_fg = colors.as_ref().map(|c| rgb(c.foreground)).unwrap_or(theme.fg);
        let bg = if theme.dark { theme.bg } else { default_bg };
        painter.rect_filled(rect, 0.0, bg);
        let Ok(mut row_iter) = self.rows_it.update(&snapshot) else { return };
        let mut y = origin.y;
        let mut text = String::new();
        while let Some(row) = row_iter.next() {
            let Ok(mut cell_iter) = self.cells_it.update(row) else { break };
            let mut x = origin.x;
            let mut job = LayoutJob::default();
            let mut run_start_x = x;
            let flush = |job: &mut LayoutJob, run_x: f32| {
                if !job.text.is_empty() {
                    let g = painter.layout_job(std::mem::take(job));
                    painter.galley(egui::pos2(run_x, y), g, default_fg);
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
                if cell.is_selected().unwrap_or(false) {
                    cbg = Some(theme.selection);
                }
                if let Some(b) = cbg {
                    painter.rect_filled(Rect::from_min_size(egui::pos2(x, y), self.cell), 0.0, b);
                }
                if n > 0 {
                    let _ = cell.graphemes_utf8(&mut text);
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
                        flush(&mut job, run_start_x);
                        let mut single = LayoutJob::default();
                        single.append(&text, 0.0, fmt);
                        painter.galley(egui::pos2(x, y), painter.layout_job(single), fg);
                    }
                    if bold {
                        // Fake bold: a second pass offset by a fraction of a pixel.
                    }
                } else {
                    flush(&mut job, run_start_x);
                }
                x += self.cell.x;
            }
            flush(&mut job, run_start_x);
            y += self.cell.y;
        }

        // Terminal cursor (hidden in copy mode).
        if !self.copy_mode {
            if let Ok(c) = snapshot.cursor() {
                if c.visible {
                    if let Some(vp) = c.viewport {
                        let r = Rect::from_min_size(origin + egui::vec2(vp.x as f32 * self.cell.x, vp.y as f32 * self.cell.y), self.cell);
                        let color = colors.as_ref().and_then(|c| c.cursor).map(rgb).unwrap_or(theme.cursor);
                        if focused {
                            painter.rect_filled(r, 1.0, color.gamma_multiply(0.75));
                        } else {
                            painter.rect_stroke(r, 1.0, Stroke::new(1.0, color), egui::StrokeKind::Inside);
                        }
                    }
                }
            }
        }
        let _ = snapshot.clean();

        // Copy-mode cursor and selection.
        if self.copy_mode {
            let top = self.viewport_top();
            if let Some((a, b)) = self.anchor.map(|a| ordered(a, self.cursor)) {
                for l in a.line.max(top)..=b.line.min(top + self.rows as usize) {
                    let s = if l == a.line { a.col } else { 0 };
                    let e = if l == b.line { b.col + 1 } else { self.cols as usize };
                    let r = Rect::from_min_size(
                        origin + egui::vec2(s as f32 * self.cell.x, (l - top) as f32 * self.cell.y),
                        egui::vec2((e.saturating_sub(s)) as f32 * self.cell.x, self.cell.y),
                    );
                    painter.rect_filled(r, 0.0, theme.selection);
                }
            }
            if self.cursor.line >= top && self.cursor.line < top + self.rows as usize {
                let r = Rect::from_min_size(
                    origin + egui::vec2(self.cursor.col as f32 * self.cell.x, (self.cursor.line - top) as f32 * self.cell.y),
                    self.cell,
                );
                painter.rect_stroke(r, 1.0, Stroke::new(1.5, theme.cursor), egui::StrokeKind::Inside);
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
        status.push(format!("{}x{}", self.cols, self.rows));
        Modeline {
            title: self.title.clone(),
            status,
            busy: false,
        }
    }

    fn directory(&self) -> Option<std::path::PathBuf> {
        // The shell reports its directory via OSC 7 (file://host/path) when integrated.
        let pwd = self.term.pwd().ok().filter(|p| !p.is_empty()).map(|p| {
            url::Url::parse(p).ok().and_then(|u| u.to_file_path().ok()).unwrap_or_else(|| std::path::PathBuf::from(p))
        });
        Some(pwd.unwrap_or_else(|| self.cwd.clone()))
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
        self.text_cache.as_ref().map(Vec::len).unwrap_or_else(|| self.term.total_rows().unwrap_or(0)).max(1)
    }
    fn line(&self, i: usize) -> String {
        // `lines()` needs `&mut self` to fill the cache; callers go through `surface()` which
        // primes it via `cursor()`/`set_cursor()`.
        self.text_cache.as_ref().and_then(|c| c.get(i).cloned()).unwrap_or_default()
    }
    fn cursor(&self) -> Pos {
        self.cursor
    }
    fn set_cursor(&mut self, p: Pos) {
        self.enter_copy_mode();
        let lines = self.lines();
        let n = lines.len().max(1);
        let line = p.line.min(n - 1);
        let col = p.col.min(lines.get(line).map(|l| char_len(l)).unwrap_or(0));
        self.cursor = Pos::new(line, col);
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
    fn undo_boundary(&mut self) {
        // Prime the text cache so read-only queries see fresh scrollback.
        if self.copy_mode || self.text_cache.is_none() {
            let _ = self.lines();
        }
    }
}
