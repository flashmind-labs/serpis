//! The `*Log*` page: what serpis logs (and what it also prints to the terminal it was started
//! from), and what each connected machine's node logs, where its chats run. A row a record:
//! when, how severe, where from, what; a click opens one in full, a drag selects what they say.
//! Filtered by level and text, it follows new records unless scrolled up. Scripts log to it with
//! `serpis.log`.

use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant};

use egui::{Color32, FontId, Rect, Stroke};
use serpis_proto::wire::{LogRecord, Request, Response};

use crate::buffer::{Buffer, BufferId, Modeline};
use crate::editor::Editor;
use crate::impl_any;
use crate::theme::Theme;

pub const NAME: &str = "*Log*";

/// A place in what a record says: its `seq`, and a character of [`text_of`].
type Spot = (u64, usize);

/// What a record says in full: its message, then its fields.
fn text_of(r: &LogRecord) -> String {
    let mut s = r.message.clone();
    for (k, v) in &r.fields {
        s.push_str(&format!("\n{k} = {v}"));
    }
    s
}

/// The characters of the record `seq`, `len` long, that a selection covers.
fn selected_in(selection: (Spot, Spot), seq: u64, len: usize) -> Option<std::ops::Range<usize>> {
    let (from, to) = if selection.0 <= selection.1 { selection } else { (selection.1, selection.0) };
    if seq < from.0 || seq > to.0 {
        return None;
    }
    let start = if seq == from.0 { from.1.min(len) } else { 0 };
    let end = if seq == to.0 { to.1.min(len) } else { len };
    (start < end).then_some(start..end)
}

/// The text a selection covers: a line for each record it reaches.
fn selected_text<'a>(records: impl Iterator<Item = &'a LogRecord>, selection: (Spot, Spot)) -> String {
    let mut parts = Vec::new();
    for r in records {
        let text = text_of(r);
        if let Some(range) = selected_in(selection, r.seq, text.chars().count()) {
            parts.push(text.chars().skip(range.start).take(range.len()).collect::<String>());
        }
    }
    parts.join("\n")
}

/// Shade the characters `range` of `galley`, drawn at `origin`.
fn paint_selected(painter: &egui::Painter, galley: &egui::Galley, origin: egui::Pos2, range: &std::ops::Range<usize>, color: Color32) {
    use egui::epaint::text::CharIndex;
    let mut start = 0;
    for row in &galley.rows {
        let n = row.char_count_excluding_newline().0;
        let (a, b) = (range.start.max(start), range.end.min(start + n));
        if a < b {
            let top_left = origin + row.pos.to_vec2();
            let (x0, x1) = (row.x_offset(CharIndex(a - start)), row.x_offset(CharIndex(b - start)));
            painter.rect_filled(Rect::from_min_max(egui::pos2(top_left.x + x0, top_left.y), egui::pos2(top_left.x + x1, top_left.y + row.height())), 0.0, color);
        }
        start += row.char_count_including_newline().0;
    }
}

/// Where records come from: this window, or a machine's node.
struct Source {
    /// None: this window.
    machine: Option<String>,
    records: VecDeque<LogRecord>,
    /// The last record read (its `seq`).
    after: u64,
    /// A request for more is out.
    asking: bool,
    /// Why the last one failed.
    error: Option<String>,
}

impl Source {
    fn new(machine: Option<String>) -> Self {
        Source { machine, records: VecDeque::new(), after: 0, asking: false, error: None }
    }

    fn label(&self) -> String {
        match self.machine.as_deref() {
            None => "This window".into(),
            Some(crate::remote::LOCAL) => "This machine's node".into(),
            Some(m) => format!("{m}'s node"),
        }
    }

    fn take(&mut self, records: Vec<LogRecord>) {
        if let Some(r) = records.last() {
            self.after = r.seq;
        }
        self.records.extend(records);
        while self.records.len() > serpis_node::logs::KEEP {
            self.records.pop_front();
        }
    }
}

pub struct LogBuffer {
    id: Option<BufferId>,
    sources: Vec<Source>,
    current: usize,
    /// The least severe level shown: 1 error … 5 trace.
    level: u8,
    search: String,
    /// Stay at the newest record as more come.
    pub follow: bool,
    /// How far down the rows are scrolled, in points.
    scroll: f32,
    /// Records opened in full: (source, seq).
    open: HashSet<(usize, u64)>,
    /// What a drag selected: where it began, and where it is.
    selection: Option<(Spot, Spot)>,
    polled: Option<Instant>,
    /// Its controls as last drawn (`serpis.debug.composer` reads them).
    pub hits: Vec<(String, Rect)>,
}

fn rank(level: &str) -> u8 {
    match level {
        "error" => 1,
        "warn" => 2,
        "info" => 3,
        "debug" => 4,
        _ => 5,
    }
}

const LEVELS: [(&str, &str); 4] = [("error", "Errors"), ("warn", "Warnings"), ("info", "Info"), ("debug", "Debug")];

/// Open the log page (made the first time).
pub fn show(ed: &mut Editor) {
    let id = match ed.buffer_by_name(NAME) {
        Some(id) => id,
        None => {
            let level = ed.var("log-view-level").and_then(|v| v.as_str().map(rank)).unwrap_or(3);
            let b = LogBuffer { id: None, sources: vec![Source::new(None)], current: 0, level, search: String::new(), follow: true, scroll: 0.0, selection: None, open: HashSet::new(), polled: None, hits: Vec::new() };
            let id = ed.add_buffer(NAME, Box::new(b));
            ed.with_buffer_as::<LogBuffer, _>(id, |b, _| b.id = Some(id));
            id
        }
    };
    ed.pop_to_buffer(id);
}

/// `14:03:22.114`, in local time.
fn clock(ms: u64) -> String {
    let secs = (ms / 1000) as nix::libc::time_t;
    let mut tm: nix::libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: localtime_r only writes the `tm` it is given.
    unsafe { nix::libc::localtime_r(&secs, &mut tm) };
    format!("{:02}:{:02}:{:02}.{:03}", tm.tm_hour, tm.tm_min, tm.tm_sec, ms % 1000)
}

/// Where a record is from, short: `chat::session` for `serpis_chat::session`.
fn short_target(t: &str) -> String {
    let t = t.strip_prefix("serpis::").unwrap_or(t);
    match t.strip_prefix("serpis_") {
        Some(rest) => rest.to_string(),
        None => t.to_string(),
    }
}

fn level_style(t: &Theme, level: &str) -> (&'static str, Color32) {
    match level {
        "error" => ("ERROR", t.red),
        "warn" => ("WARN", t.yellow),
        "info" => ("INFO", t.accent),
        "debug" => ("DEBUG", t.fg_faint),
        _ => ("TRACE", t.fg_faint),
    }
}

/// A record as a line of text: `14:03:22.114 WARN chat::session: couldn't save · path=…`.
pub fn line(r: &LogRecord) -> String {
    let mut s = format!("{} {:<5} {}: {}", clock(r.time), r.level.to_uppercase(), short_target(&r.target), r.message);
    for (k, v) in &r.fields {
        s.push_str(&format!(" · {k}={v}"));
    }
    s
}

impl LogBuffer {
    fn source(&self) -> &Source {
        &self.sources[self.current.min(self.sources.len() - 1)]
    }

    /// The records shown: at the level and up, matching the search.
    fn shown(&self) -> Vec<&LogRecord> {
        let needle = self.search.to_lowercase();
        self.source()
            .records
            .iter()
            .filter(|r| rank(&r.level) <= self.level)
            .filter(|r| {
                needle.is_empty()
                    || r.message.to_lowercase().contains(&needle)
                    || r.target.to_lowercase().contains(&needle)
                    || r.fields.iter().any(|(k, v)| k.to_lowercase().contains(&needle) || v.to_lowercase().contains(&needle))
            })
            .collect()
    }

    pub fn set_level(&mut self, level: &str) {
        self.level = rank(level);
    }

    pub fn level_name(&self) -> &'static str {
        LEVELS.iter().find(|(l, _)| rank(l) == self.level).map(|(l, _)| *l).unwrap_or("trace")
    }

    /// The next level, round: errors, warnings, info, debug.
    pub fn cycle_level(&mut self) -> &'static str {
        self.level = if self.level >= 4 { 1 } else { self.level + 1 };
        self.level_name()
    }

    pub fn search(&self) -> &str {
        &self.search
    }

    pub fn set_search(&mut self, s: String) {
        self.search = s;
        self.follow = true;
    }

    /// Show the next source (this window, then each machine's node). Its label.
    pub fn cycle_source(&mut self) -> String {
        self.current = (self.current + 1) % self.sources.len();
        self.open.clear();
        self.selection = None;
        self.follow = true;
        self.source().label()
    }

    /// Show `machine`'s node (None: this window). False when it isn't among the sources.
    pub fn select_source(&mut self, machine: Option<&str>) -> bool {
        match self.sources.iter().position(|s| s.machine.as_deref() == machine) {
            Some(i) => {
                self.current = i;
                self.follow = true;
                self.selection = None;
                true
            }
            None => false,
        }
    }

    /// What is selected, if anything.
    pub fn selected(&self) -> Option<String> {
        let selection = self.selection?;
        Some(selected_text(self.source().records.iter(), selection)).filter(|s| !s.is_empty())
    }

    pub fn source_label(&self) -> String {
        self.source().label()
    }

    /// Clear what this page shows of its source (the source keeps logging).
    pub fn clear(&mut self) {
        let i = self.current.min(self.sources.len() - 1);
        self.sources[i].records.clear();
        if self.sources[i].machine.is_none() {
            serpis_node::logs::clear();
        }
        self.open.clear();
        self.selection = None;
    }

    /// Read what is new: this window's records at once, the nodes' every second.
    fn fetch(&mut self, ed: &mut Editor) {
        let (records, _) = serpis_node::logs::since(self.sources[0].after);
        self.sources[0].take(records);
        if self.polled.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
            return;
        }
        self.polled = Some(Instant::now());
        // The nodes connected now (this machine's first), each kept once seen.
        let mut machines = crate::remote::connected_machines(ed);
        machines.sort_by_key(|m| (m != crate::remote::LOCAL, m.clone()));
        for m in machines {
            if !self.sources.iter().any(|s| s.machine.as_deref() == Some(m.as_str())) {
                self.sources.push(Source::new(Some(m)));
            }
        }
        let Some(id) = self.id else { return };
        for (i, s) in self.sources.iter_mut().enumerate().skip(1) {
            let Some(machine) = s.machine.clone().filter(|m| !s.asking && crate::remote::is_up(ed, m)) else { continue };
            s.asking = true;
            crate::remote::request(ed, &machine, Request::Logs { after: s.after }, move |ed, r| {
                ed.with_buffer_as::<LogBuffer, _>(id, |b, _| {
                    let Some(s) = b.sources.get_mut(i) else { return };
                    s.asking = false;
                    match r {
                        Ok(Response::Logs { records, .. }) => {
                            s.error = None;
                            s.take(records);
                        }
                        Ok(_) => s.error = Some("its node doesn't keep a log (an older serpis-node)".into()),
                        Err(e) => s.error = Some(e),
                    }
                });
            });
        }
    }
}

impl Buffer for LogBuffer {
    fn kind(&self) -> &str {
        "log"
    }

    fn poll(&mut self, ed: &mut Editor) {
        self.fetch(ed);
    }

    fn ui(&mut self, ui: &mut egui::Ui, rect: Rect, _focused: bool, ed: &mut Editor) {
        let t = ed.theme.clone();
        let k = t.ui_scale();
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, t.bg);
        let base = t.font_size;
        let ui_font = FontId::proportional(base - 1.5);
        let small = FontId::proportional(base - 2.5);
        let mono = FontId::monospace(t.mono_size - 1.0);
        let mono_small = FontId::monospace(t.mono_size - 2.5);
        let left = rect.left() + 20.0 * k;
        let right = rect.right() - 20.0 * k;
        let id = ui.scope_id().with("log-view");
        let mut run: Option<&'static str> = None;
        let count = self.shown().len();
        let total = self.source().records.len();

        // Title, and what is shown.
        let mut y = rect.top() + 18.0 * k;
        let title = painter.layout_no_wrap("Log".into(), FontId::proportional(base + 7.0), t.fg);
        let th = title.size().y;
        painter.galley(egui::pos2(left, y), title, t.fg);
        let what = if count == total { format!("{total} records") } else { format!("{count} of {total} records") };
        let state = match (&self.source().error, self.follow) {
            (Some(e), _) => format!(" · couldn't read: {e}"),
            (None, true) => " · following".into(),
            (None, false) => " · paused (G follows)".into(),
        };
        let whose = if self.sources.len() > 1 { String::new() } else { format!("{} · ", self.source().label()) };
        let tx = left + crate::chrome::text_width(&painter, "Log", FontId::proportional(base + 7.0)) + 12.0 * k;
        crate::chrome::text(&painter, egui::pos2(tx, y + th / 2.0 + 1.0), &format!("{whose}{what}{state}"), small.clone(), t.fg_dim, right - tx);
        y += th + 12.0 * k;

        // Controls, laid out in rows as wide as the page allows: whose log (when there is a
        // choice), then the level, search, follow and clear.
        let mut hits: Vec<(String, Rect)> = Vec::new();
        let gap = 6.0 * k;
        let ctl_h = 26.0 * k;
        let width = |label: &str| crate::chrome::text_width(&painter, label, ui_font.clone()) + 22.0 * k;
        // The next place for something `w` wide, on this row or the next.
        let mut at = egui::pos2(left, y);
        let place = |at: &mut egui::Pos2, w: f32| -> egui::Pos2 {
            if at.x > left && at.x + w > right {
                *at = egui::pos2(left, at.y + ctl_h + gap);
            }
            let p = *at;
            at.x += w + gap;
            p
        };
        let button = |ui: &mut egui::Ui, hits: &mut Vec<(String, Rect)>, p: egui::Pos2, key: String, label: &str, tip: &str, on: bool| -> bool {
            let r = Rect::from_min_size(p, egui::vec2(width(label), ctl_h));
            let resp = ui.interact(r, id.with(&key), egui::Sense::click()).on_hover_text(tip);
            let (bg, fg) = if on { (crate::chrome::tint(t.accent, 0.18), t.accent) } else if resp.hovered() { (crate::chrome::hover_bg(&t), t.fg) } else { (t.chip(), t.fg_dim) };
            painter.rect_filled(r, 8.0 * k, bg);
            crate::chrome::text(&painter, egui::pos2(r.left() + 11.0 * k, r.center().y), label, ui_font.clone(), fg, r.width());
            hits.push((key, r));
            resp.clicked()
        };
        let mut pick_source = None;
        if self.sources.len() > 1 {
            for (i, s) in self.sources.iter().enumerate() {
                let label = s.label();
                let p = place(&mut at, width(&label));
                if button(ui, &mut hits, p, format!("source-{i}"), &label, "Whose log: this window's, or a machine's node, where its chats run (s cycles)", i == self.current) {
                    pick_source = Some(i);
                }
            }
            // The rest on a row of its own.
            at = egui::pos2(left, at.y + ctl_h + gap);
        }
        // The level: one control in parts, short words when space is short.
        let full: f32 = LEVELS.iter().map(|(_, l)| width(l)).sum::<f32>() + 4.0 * k;
        let short = full > right - left;
        let labels: Vec<&str> = LEVELS.iter().map(|(l, label)| if short { match *l { "error" => "Err", "warn" => "Warn", "info" => "Info", _ => "Dbg" } } else { label }).collect();
        let seg_w: f32 = labels.iter().map(|l| width(l)).sum::<f32>() + 4.0 * k;
        let p = place(&mut at, seg_w);
        let seg = Rect::from_min_size(p, egui::vec2(seg_w, ctl_h));
        painter.rect_filled(seg, 8.0 * k, t.chip());
        let mut sx = seg.left() + 2.0 * k;
        let mut pick_level = None;
        for ((l, _), label) in LEVELS.iter().zip(&labels) {
            let r = Rect::from_min_size(egui::pos2(sx, seg.top() + 2.0 * k), egui::vec2(width(label), ctl_h - 4.0 * k));
            sx = r.right();
            let resp = ui.interact(r, id.with(("level", *l)), egui::Sense::click()).on_hover_text("Show records this severe and more (log-cycle-level, l; log-view-level)");
            let on = rank(l) == self.level;
            if on {
                painter.rect_filled(r, 6.0 * k, crate::chrome::tint(t.accent, 0.22));
            } else if resp.hovered() {
                painter.rect_filled(r, 6.0 * k, crate::chrome::hover_bg(&t));
            }
            painter.text(r.center(), egui::Align2::CENTER_CENTER, *label, ui_font.clone(), if on { t.accent } else { t.fg_dim });
            hits.push((format!("level-{l}"), r));
            if resp.clicked() {
                pick_level = Some(*l);
            }
        }
        let search = if self.search.is_empty() { "Search  /".to_string() } else { format!("“{}”  ✕", self.search) };
        let p = place(&mut at, width(&search));
        if button(ui, &mut hits, p, "search".into(), &search, "Show only records with this text (log-search, /; empty shows all)", !self.search.is_empty()) {
            if self.search.is_empty() {
                run = Some("log-search");
            } else {
                self.set_search(String::new());
            }
        }
        let follow = if self.follow { "Following" } else { "Follow" };
        let p = place(&mut at, width(follow));
        if button(ui, &mut hits, p, "follow".into(), follow, "Stay at the newest record (log-toggle-follow, f; G goes to the end)", self.follow) {
            self.follow = !self.follow;
        }
        let p = place(&mut at, width("Clear"));
        if button(ui, &mut hits, p, "clear".into(), "Clear", "Clear the records shown (log-clear, c)", false) {
            run = Some("log-clear");
        }
        if let Some(i) = pick_source {
            self.current = i;
            self.open.clear();
            self.follow = true;
        }
        if let Some(l) = pick_level {
            self.set_level(l);
        }
        y = at.y + ctl_h + 12.0 * k;
        painter.hline(rect.left()..=rect.right(), y, Stroke::new(1.0, t.border));
        y += 1.0;

        // The records: a row each, those opened in full taller. Wide, in columns (when, level,
        // where, what); narrow, on two lines (when, level and where above what).
        let list = Rect::from_min_max(egui::pos2(rect.left(), y), rect.max);
        let shown: Vec<LogRecord> = self.shown().into_iter().cloned().collect();
        let narrow = right - left < 620.0 * k;
        let line_h = (t.mono_size * 1.5).round();
        let row_h = if narrow { (line_h * 2.0 + 6.0 * k).round() } else { (t.mono_size * 1.75).round() };
        let time_w = crate::chrome::text_width(&painter, "00:00:00.000", mono_small.clone());
        let badge_w = crate::chrome::text_width(&painter, "ERROR", mono_small.clone()) + 12.0 * k;
        let target_x = left + time_w + 10.0 * k + badge_w + 10.0 * k;
        // As wide as the targets shown need, within reason.
        let target_w = shown.iter().rev().take(300).map(|r| crate::chrome::text_width(&painter, &short_target(&r.target), mono_small.clone())).fold(40.0 * k, f32::max).min(170.0 * k);
        let msg_x = if narrow { left } else { target_x + target_w + 14.0 * k };
        let msg_w = (right - msg_x).max(80.0);
        let cur = self.current;
        // The full text of an open record, laid out to the message's width.
        let full = |r: &LogRecord| -> std::sync::Arc<egui::Galley> {
            let mut job = egui::text::LayoutJob::simple(text_of(r), mono.clone(), t.fg, msg_w);
            job.wrap.max_rows = 400;
            painter.layout_job(job)
        };
        let open_extra = |g: &egui::Galley| if narrow { g.size().y + line_h + 12.0 * k } else { g.size().y + row_h * 0.6 };
        let heights: Vec<f32> = shown.iter().map(|r| if self.open.contains(&(cur, r.seq)) { open_extra(&full(r)) } else { row_h }).collect();
        let content: f32 = heights.iter().sum();
        let resp = ui.interact(list, id.with("rows"), egui::Sense::click_and_drag());
        if resp.contains_pointer() {
            let d = ui.input(|i| i.smooth_scroll_delta.y);
            if d != 0.0 {
                self.scroll -= d;
                // Scrolling up stops following; back at the end, it follows again.
                self.follow = self.scroll >= content - list.height() - 1.0;
            }
        }
        let max = (content - list.height()).max(0.0);
        if self.follow {
            self.scroll = max;
        }
        self.scroll = self.scroll.clamp(0.0, max);
        if shown.is_empty() {
            let msg = if total == 0 { "Nothing logged yet." } else { "No records match." };
            painter.text(list.center(), egui::Align2::CENTER_CENTER, msg, ui_font.clone(), t.fg_faint);
        }
        let clip = painter.with_clip_rect(list);
        let mut ry = list.top() - self.scroll;
        let pointer = ui.input(|i| i.pointer.latest_pos());
        let mut toggle = None;
        // The rows drawn: what a drag or a click lands on.
        struct Drawn {
            seq: u64,
            row: Rect,
            galley: std::sync::Arc<egui::Galley>,
            origin: egui::Pos2,
            /// Its end stands for the rest of the record's text, which a closed row doesn't show.
            closed_len: Option<usize>,
            len: usize,
        }
        let mut drawn: Vec<Drawn> = Vec::new();
        for (r, h) in shown.iter().zip(&heights) {
            if ry + h < list.top() {
                ry += h;
                continue;
            }
            if ry > list.bottom() {
                break;
            }
            let row = Rect::from_min_size(egui::pos2(rect.left(), ry), egui::vec2(rect.width(), *h));
            let (badge, color) = level_style(&t, &r.level);
            let wash = match r.level.as_str() {
                "error" => crate::chrome::tint(t.red, 0.08),
                "warn" => crate::chrome::tint(t.yellow, 0.06),
                _ => Color32::TRANSPARENT,
            };
            clip.rect_filled(row, 0.0, wash);
            if resp.contains_pointer() && pointer.is_some_and(|p| row.contains(p)) {
                clip.rect_filled(row, 0.0, crate::chrome::hover_bg(&t));
            }
            // A thin rule between rows, for two-line ones.
            if narrow {
                clip.hline(left..=right, row.bottom() - 0.5, Stroke::new(1.0, crate::chrome::tint(t.border, 0.6)));
            }
            let my = if narrow { ry + 3.0 * k + line_h / 2.0 } else { ry + row_h / 2.0 };
            crate::chrome::text(&clip, egui::pos2(left, my), &clock(r.time), mono_small.clone(), t.fg_faint, time_w + 2.0);
            let bx = left + time_w + 10.0 * k;
            let br = Rect::from_min_size(egui::pos2(bx, my - 9.0 * k), egui::vec2(badge_w, 18.0 * k));
            clip.rect_filled(br, 5.0 * k, crate::chrome::tint(color, 0.16));
            clip.text(br.center(), egui::Align2::CENTER_CENTER, badge, mono_small.clone(), color);
            let tw = if narrow { right - target_x } else { target_w };
            crate::chrome::text(&clip, egui::pos2(target_x, my), &short_target(&r.target), mono_small.clone(), t.fg_dim, tw);
            // What it says: its own column, or the second line.
            let (my, top) = if narrow { (my + line_h, ry + 3.0 * k + line_h) } else { (my, ry + row_h * 0.3) };
            let len = text_of(r).chars().count();
            let selected = self.selection.and_then(|s| selected_in(s, r.seq, len));
            if self.open.contains(&(cur, r.seq)) {
                let origin = egui::pos2(msg_x, top);
                let galley = full(r);
                if let Some(range) = &selected {
                    paint_selected(&clip, &galley, origin, range, t.selection);
                }
                clip.galley(origin, galley.clone(), t.fg);
                drawn.push(Drawn { seq: r.seq, row, galley, origin, closed_len: None, len });
            } else {
                // One line: the message's first, then its fields as chips.
                let first = r.message.lines().next().unwrap_or("");
                let more = r.message.lines().nth(1).is_some();
                let galley = painter.layout_no_wrap(first.to_string(), mono.clone(), t.fg);
                let origin = egui::pos2(msg_x, my - galley.size().y / 2.0);
                if let Some(range) = &selected {
                    paint_selected(&clip, &galley, origin, range, t.selection);
                }
                drawn.push(Drawn { seq: r.seq, row, galley, origin, closed_len: Some(first.chars().count()), len });
                let mw = crate::chrome::text(&clip, egui::pos2(msg_x, my), &format!("{first}{}", if more { " …" } else { "" }), mono.clone(), if r.level == "debug" || r.level == "trace" { t.fg_dim } else { t.fg }, msg_w);
                let mut fx = msg_x + mw + 10.0 * k;
                for (key, v) in &r.fields {
                    let label = format!("{key}={v}");
                    let w = crate::chrome::text_width(&clip, &label, mono_small.clone()) + 12.0 * k;
                    if fx + w > right {
                        break;
                    }
                    let fr = Rect::from_min_size(egui::pos2(fx, my - 9.0 * k), egui::vec2(w, 18.0 * k));
                    clip.rect_filled(fr, 5.0 * k, t.chip());
                    crate::chrome::text(&clip, egui::pos2(fx + 6.0 * k, my), &label, mono_small.clone(), t.fg_dim, w);
                    fx += w + 6.0 * k;
                }
            }
            ry += h;
        }
        drop(shown);
        // The place in the text nearest a point: its row, then its character.
        let spot_at = |p: egui::Pos2| -> Option<Spot> {
            let d = drawn.iter().find(|d| p.y < d.row.bottom()).or(drawn.last())?;
            let at = d.galley.cursor_from_pos(p - d.origin).index.0;
            // Past what a closed row shows is the rest of it.
            Some((d.seq, if d.closed_len.is_some_and(|n| at >= n) { d.len } else { at }))
        };
        if resp.contains_pointer() && pointer.is_some_and(|p| p.x >= msg_x && drawn.iter().any(|d| d.row.contains(p))) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
        }
        if resp.drag_started() {
            // Where the press was, not where the pointer has got to by now.
            let from = ui.input(|i| i.pointer.press_origin()).and_then(spot_at);
            self.selection = from.map(|s| (s, s));
            self.follow = false;
        }
        if resp.dragged()
            && let Some(p) = pointer
        {
            if let (Some(selection), Some(to)) = (&mut self.selection, spot_at(p)) {
                selection.1 = to;
            }
            // Held past the top or bottom, the list moves under it.
            let by = if p.y < list.top() { p.y - list.top() } else if p.y > list.bottom() { p.y - list.bottom() } else { 0.0 };
            if by != 0.0 {
                self.scroll = (self.scroll + by * 0.25).clamp(0.0, max);
                ui.ctx().request_repaint();
            }
        }
        if resp.clicked() {
            // With something selected, a click only lets go of it.
            if self.selection.take().is_none() {
                toggle = pointer.and_then(|p| drawn.iter().find(|d| d.row.contains(p))).map(|d| d.seq);
            }
        }
        if let Some(seq) = toggle
            && !self.open.remove(&(cur, seq))
        {
            self.open.insert((cur, seq));
            self.follow = false;
        }
        // What each row says, for scripts and tests to find it by.
        for d in &drawn {
            hits.push((format!("text:{}", d.galley.text().lines().next().unwrap_or("")), Rect::from_min_size(d.origin, d.galley.size())));
        }
        self.hits = hits;
        // New records come without any input event.
        ui.ctx().request_repaint_after(Duration::from_millis(250));
        if let Some(cmd) = run {
            ed.main_sender().send(move |ed| ed.call(cmd));
        }
    }

    fn full_text(&mut self) -> Option<String> {
        let mut s: String = self.shown().iter().map(|r| line(r) + "\n").collect();
        if s.is_empty() {
            s = "Nothing logged yet.\n".into();
        }
        Some(s)
    }

    fn modeline(&self) -> Modeline {
        Modeline { title: format!("Log · {}", self.source().label()), ..Default::default() }
    }

    impl_any!();
}

/// The log page's commands, keys and settings.
pub fn install(ed: &mut Editor) {
    let o = crate::editor::BUILTIN;
    ed.defvar("log-level", serde_json::json!("info"), "What serpis keeps of its own log, for the *Log* page and serpis.log.records: \"error\", \"warn\", \"info\", \"debug\" or \"trace\" and up (other libraries: warnings and up). What the terminal shows is RUST_LOG's to say.");
    ed.defvar("log-view-level", serde_json::json!("info"), "The least severe records the *Log* page shows when it opens: \"error\", \"warn\", \"info\" or \"debug\" (l cycles them there).");
    ed.add_hook("variable-changed", o, |ed, p| {
        if p.get("name").and_then(serde_json::Value::as_str) == Some("log-level") {
            let l = ed.var("log-level").and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
            if !serpis_node::logs::set_level(&l) {
                ed.warn(format!("log-level: {l} isn't a level (error, warn, info, debug, trace)"));
            }
        }
        None
    });
    let with_log = |ed: &mut Editor, f: &mut dyn FnMut(&mut LogBuffer) -> Option<String>| -> anyhow::Result<()> {
        let id = ed.current_buffer();
        match ed.with_buffer_as::<LogBuffer, _>(id, |b, _| f(b)) {
            Some(Some(m)) => ed.message(m),
            Some(None) => {}
            None => anyhow::bail!("Not in the *Log* page (log-view)"),
        }
        Ok(())
    };
    ed.define_command("log-view", "Show the *Log* page: what serpis logs (what it also prints to the terminal it was started from), and what each connected machine's node logs, where its chats run.", o, |ed, _| {
        show(ed);
        Ok(())
    });
    ed.define_command("log-search", "Show only the log records with some text (param: the text; empty shows all).", o, move |ed, a| {
        if let Some(s) = a.str(0).map(str::to_string) {
            return with_log(ed, &mut |b| {
                b.set_search(s.clone());
                None
            });
        }
        let id = ed.current_buffer();
        let Some(now) = ed.with_buffer_as::<LogBuffer, _>(id, |b, _| b.search().to_string()) else { anyhow::bail!("Not in the *Log* page (log-view)") };
        ed.read_string("Search the log: ", &now, move |ed, s| {
            ed.with_buffer_as::<LogBuffer, _>(id, |b, _| b.set_search(s));
        });
        Ok(())
    });
    ed.define_command("log-cycle-level", "Show log records from the next level up: errors, warnings, info, debug (param: a level).", o, move |ed, a| {
        let level = a.str(0).map(str::to_string);
        with_log(ed, &mut |b| {
            Some(match &level {
                Some(l) => {
                    b.set_level(l);
                    format!("Log: {} and up", b.level_name())
                }
                None => format!("Log: {} and up", b.cycle_level()),
            })
        })
    });
    ed.define_command("log-toggle-follow", "Whether the *Log* page stays at the newest record as more come.", o, move |ed, _| {
        with_log(ed, &mut |b| {
            b.follow = !b.follow;
            Some(if b.follow { "Log: following".into() } else { "Log: paused".into() })
        })
    });
    ed.define_command("log-end", "Go to the newest log record, and follow from there.", o, move |ed, _| {
        with_log(ed, &mut |b| {
            b.follow = true;
            None
        })
    });
    ed.define_command("log-cycle-source", "Show the next log: this window's, then each connected machine's node (param: a machine, or \"window\").", o, move |ed, a| {
        let which = a.str(0).map(str::to_string);
        with_log(ed, &mut |b| {
            Some(match &which {
                Some(w) if w == "window" => {
                    b.select_source(None);
                    format!("Log: {}", b.source_label())
                }
                Some(m) if b.select_source(Some(m)) => format!("Log: {}", b.source_label()),
                Some(m) => format!("No log from {m} yet (is its node connected?)"),
                None => format!("Log: {}", b.cycle_source()),
            })
        })
    });
    ed.define_command("log-copy", "Copy the text selected in the *Log* page (drag over records to select).", o, move |ed, _| {
        let id = ed.current_buffer();
        let Some(selected) = ed.with_buffer_as::<LogBuffer, _>(id, |b, _| b.selected()) else { anyhow::bail!("Not in the *Log* page (log-view)") };
        match selected {
            Some(text) => {
                ed.copy_to_clipboard(&text);
                ed.message("Copied");
            }
            None => ed.message("Nothing selected: drag over the text to select it"),
        }
        Ok(())
    });
    ed.define_command("log-clear", "Clear the log records the *Log* page shows (new ones still come).", o, move |ed, _| {
        with_log(ed, &mut |b| {
            b.clear();
            None
        })
    });
    for (k, c) in [("s-c", "log-copy"), ("M-w", "log-copy"), ("y", "log-copy"), ("/", "log-search"), ("l", "log-cycle-level"), ("f", "log-toggle-follow"), ("G", "log-end"), ("s", "log-cycle-source"), ("c", "log-clear"), ("q", "kill-current-buffer")] {
        ed.bind("log", k, c, o);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(seq: u64, message: &str) -> LogRecord {
        LogRecord { seq, time: 0, level: "info".into(), target: "test".into(), message: message.into(), fields: Vec::new() }
    }

    #[test]
    fn a_selection_covers_characters_within_a_record_and_whole_ones_between() {
        let rs = [record(1, "first line"), record(2, "second"), record(3, "third line")];
        // Within one, either way round.
        assert_eq!(selected_text(rs.iter(), ((1, 6), (1, 10))), "line");
        assert_eq!(selected_text(rs.iter(), ((1, 10), (1, 6))), "line");
        // Across several, in the order they show, whichever way the drag went.
        assert_eq!(selected_text(rs.iter(), ((1, 6), (3, 5))), "line\nsecond\nthird");
        assert_eq!(selected_text(rs.iter(), ((3, 5), (1, 6))), "line\nsecond\nthird");
        // Nothing dragged over: nothing.
        assert_eq!(selected_text(rs.iter(), ((2, 3), (2, 3))), "");
    }

    #[test]
    fn it_counts_characters_not_bytes_and_includes_fields() {
        let mut r = record(1, "naïve → ok");
        assert_eq!(selected_text([&r].into_iter(), ((1, 2), (1, 7))), "ïve →");
        r.fields.push(("n".into(), "3".into()));
        assert_eq!(text_of(&r), "naïve → ok\nn = 3");
        // The whole of it, for a record a selection passes through.
        assert_eq!(selected_in(((0, 0), (2, 0)), 1, 16), Some(0..16));
        assert_eq!(selected_in(((1, 99), (1, 4)), 1, 16), Some(4..16));
        assert_eq!(selected_in(((2, 0), (3, 0)), 1, 16), None);
    }
}
