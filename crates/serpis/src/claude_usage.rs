//! Claude Code's usage, as its `/usage` shows it: the subscription's rate-limit windows (the
//! five-hour session, the week, per-model weeks), extra usage, and what the last week's
//! transcripts say spends it. Asked of the `claude` CLI on the chat's machine (the node there
//! runs it) or on this one; nothing is sent to a model. Shown in a page of its own
//! ([`UsageBuffer`]: a card per window, its bar colored by how full it is).

use std::sync::Arc;
use std::time::Instant;

use egui::{Color32, Pos2, Rect, Stroke};
use serde_json::Value;
use serpis_proto::wire::{Request, Response};

use crate::buffer::{Buffer, Modeline};
use crate::editor::Editor;
use crate::impl_any;
use crate::theme::{Face, Theme};

/// Ask `machine`'s Claude Code (this machine's: `None`) for its usage, with its advice when
/// `advice`; `done` gets the CLI's answer.
pub fn fetch(ed: &mut Editor, machine: Option<String>, advice: bool, done: impl FnOnce(&mut Editor, Result<Value, String>) + 'static) {
    let who = machine.clone();
    let answered = move |ed: &mut Editor, r: Result<Response, String>| {
        let on = |e: String| who.as_ref().map(|m| format!("{m}: {e}")).unwrap_or(e);
        done(ed, match r {
            Ok(Response::ClaudeCodeUsage { usage }) => Ok(usage),
            Ok(other) => Err(on(format!("unexpected answer {other:?}"))),
            Err(e) => Err(on(e)),
        })
    };
    if let Some(m) = machine {
        return crate::remote::request(ed, &m, Request::ClaudeCodeUsage { advice }, answered);
    }
    let set = ed.var_str("claude-code-path").filter(|p| !p.is_empty()).map(|p| crate::expand_home(&p));
    let Some(binary) = set.or_else(|| ed.registry.claude_code_binary()) else {
        return answered(ed, Err("Claude Code isn't installed on this machine (claude not found; set claude-code-path)".into()));
    };
    let asked = async move { serpis_ai::claude_code::usage(&binary, advice).await.map(|usage| Response::ClaudeCodeUsage { usage }).map_err(|e| format!("{e:#}")) };
    crate::remote::answer_with(ed, asked, answered);
}

/// Show the usage of `machine`'s Claude Code (this one's: `None`) in its page, asking for it
/// afresh; the page says it is asking meanwhile.
pub fn show(ed: &mut Editor, machine: Option<String>) {
    let name = match &machine {
        Some(m) => format!("*claude-code-usage {m}*"),
        None => "*claude-code-usage*".to_string(),
    };
    let id = match ed.buffer_by_name(&name) {
        Some(id) => id,
        None => ed.add_buffer(&name, Box::new(UsageBuffer { machine: machine.clone(), state: State::Asking(Instant::now()), scroll: 0.0 })),
    };
    ed.with_buffer_as::<UsageBuffer, _>(id, |b, _| b.state = State::Asking(Instant::now()));
    ed.pop_to_buffer(id);
    let advice = ed.var_bool("claude-code-usage-advice");
    fetch(ed, machine, advice, move |ed, r| {
        ed.with_buffer_as::<UsageBuffer, _>(id, |b, _| {
            b.state = match r {
                Ok(u) => State::Shown(Box::new(Summary::of(&u)), Instant::now()),
                Err(e) => State::Failed(e),
            }
        });
    });
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// What the CLI's answer says, read once.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    /// `Pro`, `Max`, ... (None: an API key or a cloud provider).
    pub plan: Option<String>,
    /// False: no plan limits apply (billed by use).
    pub limited: bool,
    pub windows: Vec<Window>,
    /// Extra usage: on, and how much of its monthly limit is used.
    pub extra: Option<(bool, Option<f64>)>,
    /// Where the week went: (surface, percent), the ones used.
    pub shares: Vec<(String, f64)>,
    pub advice: Option<Advice>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    pub label: String,
    pub pct: Option<f64>,
    /// Unix seconds.
    pub resets: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Advice {
    pub requests: u64,
    pub sessions: u64,
    /// (share of the usage it touches, what to do, what was seen or the command).
    pub tips: Vec<(f64, String, Option<String>)>,
}

impl Summary {
    pub fn of(usage: &Value) -> Summary {
        let limits = &usage["rate_limits"];
        let limited = usage["rate_limits_available"] == true && limits.is_object();
        let mut windows = Vec::new();
        if limited {
            for (key, label) in [("five_hour", "Current session"), ("seven_day", "This week"), ("seven_day_opus", "This week, Opus"), ("seven_day_sonnet", "This week, Sonnet"), ("seven_day_oauth_apps", "This week, apps")] {
                if limits[key].is_object() {
                    windows.push(Window::of(label.into(), &limits[key]));
                }
            }
            for m in limits["model_scoped"].as_array().into_iter().flatten() {
                windows.push(Window::of(format!("This week, {}", m["display_name"].as_str().unwrap_or("a model")), m));
            }
        }
        let e = &limits["extra_usage"];
        let extra = (limited && e.is_object()).then(|| (e["is_enabled"] == true, e["utilization"].as_f64()));
        let shares = limits["seven_day_breakdown"]["rows"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| Some((r["display_name"].as_str()?.to_string(), r["percent"].as_f64().filter(|p| *p > 0.0)?)))
            .collect();
        let week = &usage["behaviors"]["week"];
        let advice = week.is_object().then(|| Advice {
            requests: week["request_count"].as_u64().unwrap_or(0),
            sessions: week["session_count"].as_u64().unwrap_or(0),
            tips: week["recommendations"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|r| (r["pct"].as_f64().unwrap_or(0.0), r["title"].as_str().unwrap_or("?").to_string(), r["detail"].as_str().map(str::to_string)))
                .collect(),
        });
        Summary { plan: usage["subscription_type"].as_str().map(capitalized), limited, windows, extra, shares, advice }
    }

    /// The page as text (what `serpis.buffer.text` and copying give).
    pub fn text(&self, now: u64, machine: Option<&str>) -> String {
        let plan = self.plan.as_ref().map(|p| format!(" · {p} plan")).unwrap_or_default();
        let mut out = format!("Claude Code usage{plan}{}\n\n", machine.map(|m| format!(" (on {m})")).unwrap_or_default());
        if !self.limited {
            out.push_str("No plan limits: this claude uses an API key or a cloud provider, billed by use.\n");
        }
        for w in &self.windows {
            let pct = w.pct.map(|p| format!("{p:.0}%")).unwrap_or_else(|| "?".into());
            let resets = w.resets.map(|t| format!(", resets in {}", until(t, now))).unwrap_or_default();
            out.push_str(&format!("{}: {pct}{resets}\n", w.label));
        }
        if let Some((on, used)) = self.extra {
            out.push_str(&match (on, used) {
                (true, Some(p)) => format!("\nExtra usage: on, {p:.0}% of its monthly limit used.\n"),
                (true, None) => "\nExtra usage: on.\n".into(),
                _ => "\nExtra usage: off.\n".into(),
            });
        }
        if !self.shares.is_empty() {
            let s: Vec<String> = self.shares.iter().map(|(n, p)| format!("{n} {p:.0}%")).collect();
            out.push_str(&format!("This week went to: {}.\n", s.join(" · ")));
        }
        if let Some(a) = &self.advice {
            out.push_str(&format!("\nWhat spends it, from this machine's last 7 days ({} requests in {} sessions):\n", a.requests, a.sessions));
            if a.tips.is_empty() {
                out.push_str("- nothing stands out\n");
            }
            for (pct, title, detail) in &a.tips {
                out.push_str(&format!("- {title} ({pct:.0}% of it){}\n", detail.as_ref().map(|d| format!(": {d}")).unwrap_or_default()));
            }
        }
        out
    }
}

impl Window {
    fn of(label: String, w: &Value) -> Window {
        Window { label, pct: w["utilization"].as_f64(), resets: w["resets_at"].as_str().and_then(unix_seconds) }
    }
}

enum State {
    Asking(Instant),
    Shown(Box<Summary>, Instant),
    Failed(String),
}

/// The `*claude-code-usage*` page: `g` asks again, `q` closes it.
pub struct UsageBuffer {
    pub machine: Option<String>,
    state: State,
    scroll: f32,
}

impl UsageBuffer {
    pub fn summary(&self) -> Option<&Summary> {
        match &self.state {
            State::Shown(s, _) => Some(s),
            _ => None,
        }
    }
}

/// How full a window is, in a color: the accent, then yellow from 75%, red from 90%.
pub(crate) fn level(t: &Theme, pct: f64) -> Color32 {
    if pct >= 90.0 {
        t.red
    } else if pct >= 75.0 {
        t.yellow
    } else {
        t.accent
    }
}

/// Text laid out in the theme's UI font at `size`, `weight` (None: regular).
pub(crate) fn galley(p: &egui::Painter, t: &Theme, s: &str, size: f32, color: Color32, weight: Option<f32>, wrap: f32) -> Arc<egui::Galley> {
    let mut f = t.format(Face::Default);
    f.font_id = egui::FontId::proportional(size);
    f.color = color;
    f.extra_letter_spacing = t.letter_spacing(size);
    f.coords = Default::default();
    if let Some(w) = weight {
        f.coords.push(b"wght", w);
    }
    let mut job = egui::text::LayoutJob::single_section(s.to_string(), f);
    job.wrap.max_width = wrap;
    p.layout_job(job)
}

impl Buffer for UsageBuffer {
    fn kind(&self) -> &str {
        "claude-usage"
    }

    fn ui(&mut self, ui: &mut egui::Ui, rect: Rect, _focused: bool, ed: &mut Editor) {
        let t = ed.theme.clone();
        let k = t.ui_scale();
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, t.bg);
        let base = t.font_size;
        let bold = Some(t.bold_weight);
        let medium = Some((t.bold_weight - 100.0).max(500.0));
        let width = (rect.width() - 48.0 * k).clamp(200.0, 640.0 * k);
        let left = rect.center().x - width / 2.0;
        let mut run: Option<&'static str> = None;

        // Wheel scrolling, for pages taller than the window.
        let resp = ui.interact(rect, ui.scope_id().with("claude-usage"), egui::Sense::hover());
        if resp.hovered() {
            self.scroll -= ui.input(|i| i.smooth_scroll_delta.y);
        }
        let mut y = rect.top() + 28.0 * k - self.scroll;
        let top = y;
        let text = |p: &egui::Painter, pos: Pos2, s: &str, size: f32, color: Color32, weight: Option<f32>| -> egui::Vec2 {
            let g = galley(p, &t, s, size, color, weight, f32::INFINITY);
            let size = g.size();
            p.galley(pos, g, color);
            size
        };

        // The title, the plan, and a way to ask again.
        let title = galley(&painter, &t, "Claude Code usage", base + 8.0, t.fg, bold, width);
        let title = {
            let size = title.size();
            painter.galley(egui::pos2(left, y), title, t.fg);
            size
        };
        let sub_font = egui::FontId::proportional(base - 2.0);
        // In a narrow window: Refresh goes first (g still asks), then the pill (the plan goes
        // under the title).
        let plan = match &self.state {
            State::Shown(s, _) => s.plan.as_ref().map(|p| format!("{p} plan")),
            _ => None,
        };
        let pill_w = plan.as_ref().map_or(0.0, |p| crate::chrome::text_width(&painter, p, sub_font.clone()) + (sub_font.size + 8.0).round() * 0.8 + 12.0 * k);
        let refresh = "Refresh";
        let rw = crate::chrome::text_width(&painter, refresh, sub_font.clone()) + 22.0 * k;
        let pill_fits = title.x + pill_w <= width;
        if let Some(plan) = plan.as_ref().filter(|_| pill_fits) {
            crate::chrome::pill(&painter, left + title.x + 12.0 * k, y + title.y / 2.0, plan, sub_font.clone(), t.accent, crate::chrome::tint(t.accent, 0.16), false);
        }
        if title.x + pill_w + 12.0 * k + rw <= width {
            let rb = Rect::from_min_size(egui::pos2(left + width - rw, y + title.y / 2.0 - 13.0 * k), egui::vec2(rw, 26.0 * k));
            let rresp = ui.interact(rb, ui.scope_id().with("claude-usage-refresh"), egui::Sense::click()).on_hover_text("Ask Claude Code again (g)");
            painter.rect_filled(rb, 8.0 * k, if rresp.hovered() { crate::chrome::hover_bg(&t) } else { t.chip() });
            crate::chrome::text(&painter, egui::pos2(rb.left() + 11.0 * k, rb.center().y), refresh, sub_font.clone(), t.fg, rw);
            if rresp.clicked() {
                run = Some("claude-code-usage-refresh");
            }
        }
        y += title.y + 6.0 * k;
        let whose = self.machine.as_ref().map(|m| format!("Claude Code on {m}")).unwrap_or_else(|| "Claude Code on this machine".into());
        let whose = match plan.filter(|_| !pill_fits) {
            Some(plan) => format!("{plan} · {whose}"),
            None => whose,
        };
        let when = match &self.state {
            State::Asking(_) => "asking…".to_string(),
            State::Shown(_, at) => match at.elapsed().as_secs() {
                0..60 => "updated just now".into(),
                s => format!("updated {} ago", until(s, 0)),
            },
            State::Failed(_) => "couldn't ask".into(),
        };
        let g = galley(&painter, &t, &format!("{whose} · {when}"), base - 2.0, t.fg_dim, None, width);
        y += g.size().y + 22.0 * k;
        painter.galley(egui::pos2(left, y - 22.0 * k - g.size().y), g, t.fg_dim);

        // A card: its rectangle, from `y`, `h` tall.
        let card = |p: &egui::Painter, y: f32, h: f32| {
            let r = Rect::from_min_size(egui::pos2(left, y), egui::vec2(width, h));
            p.rect_filled(r, 12.0 * k, t.bg_elevated);
            p.rect_stroke(r, 12.0 * k, Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
            r
        };
        let pad = 18.0 * k;
        let heading = |p: &egui::Painter, y: &mut f32, s: &str| {
            *y += text(p, egui::pos2(left + 2.0 * k, *y), s, base - 1.0, t.fg_dim, medium).y + 10.0 * k;
        };

        match &self.state {
            State::Asking(since) => {
                let r = card(&painter, y, 64.0 * k);
                crate::chrome::spinner(&painter, egui::pos2(r.left() + pad + 8.0 * k, r.center().y), 8.0 * k, ui.input(|i| i.time), t.accent);
                let g = galley(&painter, &t, "Asking Claude Code…", base, t.fg_dim, None, f32::INFINITY);
                let gy = r.center().y - g.size().y / 2.0;
                painter.galley(egui::pos2(r.left() + pad + 28.0 * k, gy), g, t.fg_dim);
                if since.elapsed().as_secs() > 5 && ed.var_bool("claude-code-usage-advice") {
                    let note = "(it reads the last week of transcripts for its advice)";
                    text(&painter, egui::pos2(left + 2.0 * k, r.bottom() + 10.0 * k), note, base - 2.0, t.fg_faint, None);
                }
                ui.ctx().request_repaint();
                y = r.bottom();
            }
            State::Failed(e) => {
                let g = galley(&painter, &t, e, base, t.fg, None, width - 2.0 * pad);
                let r = card(&painter, y, g.size().y + 2.0 * pad + 24.0 * k);
                painter.rect_filled(Rect::from_min_size(r.min, egui::vec2(4.0 * k, r.height())), 2.0 * k, t.red);
                text(&painter, egui::pos2(r.left() + pad, r.top() + pad - 2.0 * k), "Couldn't get the usage", base - 1.0, t.red, medium);
                painter.galley(egui::pos2(r.left() + pad, r.top() + pad + 22.0 * k), g, t.fg);
                y = r.bottom();
            }
            State::Shown(s, _) => {
                let now = unix_now();
                if !s.limited {
                    let r = card(&painter, y, 64.0 * k);
                    let g = galley(&painter, &t, "No plan limits: this claude uses an API key or a cloud provider, billed by use.", base, t.fg_dim, None, width - 2.0 * pad);
                    painter.galley(egui::pos2(r.left() + pad, r.center().y - g.size().y / 2.0), g, t.fg_dim);
                    y = r.bottom() + 26.0 * k;
                } else {
                    heading(&painter, &mut y, "Plan limits");
                    // One card, a row per window: its name, how full, its bar, when it resets.
                    let resets = |w: &Window| w.resets.map(|at| format!("resets in {}", until(at, now)));
                    let measure = |s: &str, size: f32, weight: Option<f32>| galley(&painter, &t, s, size, t.fg, weight, f32::INFINITY).size().x;
                    let below = s.windows.iter().any(|w| {
                        let reset = resets(w).map_or(0.0, |r| measure(&r, base - 2.0, None) + 16.0 * k);
                        measure(&w.label, base, medium) + 16.0 * k + reset + measure("100%", base, bold) > width - 2.0 * pad
                    });
                    let row_h = if below { 84.0 * k } else { 66.0 * k };
                    let r = card(&painter, y, row_h * s.windows.len() as f32 + 8.0 * k);
                    for (i, w) in s.windows.iter().enumerate() {
                        let ry = r.top() + 4.0 * k + row_h * i as f32;
                        if i > 0 {
                            painter.hline(r.left() + pad..=r.right() - pad, ry, Stroke::new(1.0, t.border));
                        }
                        let pct = w.pct.unwrap_or(0.0);
                        let color = level(&t, pct);
                        text(&painter, egui::pos2(r.left() + pad, ry + 12.0 * k), &w.label, base, t.fg, medium);
                        let shown = w.pct.map(|p| format!("{p:.0}%")).unwrap_or_else(|| "?".into());
                        let g = galley(&painter, &t, &shown, base, if pct >= 75.0 { color } else { t.fg }, bold, f32::INFINITY);
                        let pw = g.size().x;
                        painter.galley(egui::pos2(r.right() - pad - pw, ry + 12.0 * k), g, t.fg);
                        if let Some(reset) = resets(w) {
                            let g = galley(&painter, &t, &reset, base - 2.0, t.fg_dim, None, f32::INFINITY);
                            let at = if below { egui::pos2(r.left() + pad, ry + 58.0 * k) } else { egui::pos2(r.right() - pad - pw - 16.0 * k - g.size().x, ry + 14.0 * k) };
                            painter.galley(at, g, t.fg_dim);
                        }
                        let bar = Rect::from_min_size(egui::pos2(r.left() + pad, ry + 42.0 * k), egui::vec2(r.width() - 2.0 * pad, 8.0 * k));
                        painter.rect_filled(bar, 4.0 * k, t.chip());
                        let fill = (pct.clamp(0.0, 100.0) / 100.0) as f32 * bar.width();
                        if fill > 0.0 {
                            painter.rect_filled(Rect::from_min_size(bar.min, egui::vec2(fill.max(8.0 * k), bar.height())), 4.0 * k, color);
                        }
                    }
                    y = r.bottom() + 12.0 * k;
                    if let Some((on, used)) = s.extra {
                        let label = match (on, used) {
                            (true, Some(p)) => format!("On · {p:.0}% of the monthly limit used"),
                            (true, None) => "On".into(),
                            _ => "Off".into(),
                        };
                        let name = galley(&painter, &t, "Extra usage", base, t.fg, medium, f32::INFINITY);
                        let g = galley(&painter, &t, &label, base - 1.0, if on { t.fg } else { t.fg_dim }, None, width - 2.0 * pad);
                        let one_line = name.size().x + 16.0 * k + g.size().x <= width - 2.0 * pad;
                        let r = card(&painter, y, if one_line { 48.0 * k } else { 38.0 * k + g.size().y + 14.0 * k });
                        painter.galley(egui::pos2(r.left() + pad, r.top() + 14.0 * k), name, t.fg);
                        let at = if one_line { egui::pos2(r.right() - pad - g.size().x, r.top() + 15.0 * k) } else { egui::pos2(r.left() + pad, r.top() + 38.0 * k) };
                        painter.galley(at, g, t.fg);
                        y = r.bottom() + 26.0 * k;
                    } else {
                        y += 14.0 * k;
                    }
                }

                // Where the week went: one bar in parts, and their names.
                if !s.shares.is_empty() {
                    heading(&painter, &mut y, "This week went to");
                    let colors = [t.accent, t.purple, t.cyan, t.green, t.yellow];
                    let total: f64 = s.shares.iter().map(|(_, p)| p).sum::<f64>().max(100.0);
                    // Where each name goes: a line of them, then the next when it is full.
                    let line_h = 28.0 * k;
                    let (mut lx, mut line) = (0.0, 0);
                    let legend: Vec<(f32, usize)> = s
                        .shares
                        .iter()
                        .map(|(name, pct)| {
                            let w = 14.0 * k + galley(&painter, &t, &format!("{name}  {pct:.0}%"), base - 1.0, t.fg, None, f32::INFINITY).size().x;
                            if lx > 0.0 && lx + w > width - 2.0 * pad {
                                (lx, line) = (0.0, line + 1);
                            }
                            let at = (lx, line);
                            lx += w + 34.0 * k;
                            at
                        })
                        .collect();
                    let r = card(&painter, y, 84.0 * k + line_h * line as f32);
                    let bar = Rect::from_min_size(egui::pos2(r.left() + pad, r.top() + pad), egui::vec2(r.width() - 2.0 * pad, 10.0 * k));
                    painter.rect_filled(bar, 5.0 * k, t.chip());
                    let mut x = bar.left();
                    for (i, ((name, pct), (lx, line))) in s.shares.iter().zip(legend).enumerate() {
                        let c = colors[i % colors.len()];
                        let w = (pct / total) as f32 * bar.width();
                        let part = Rect::from_min_size(egui::pos2(x, bar.top()), egui::vec2(w, bar.height()));
                        let first = i == 0;
                        let last = i + 1 == s.shares.len() && (pct / total) > 0.999 - (x - bar.left()) as f64 / bar.width() as f64;
                        let round = egui::CornerRadius { nw: if first { 5 } else { 0 }, sw: if first { 5 } else { 0 }, ne: if last { 5 } else { 0 }, se: if last { 5 } else { 0 } };
                        painter.rect_filled(part, round, c);
                        x += w;
                        let (lx, ly) = (r.left() + pad + lx, r.top() + pad + 36.0 * k + line_h * line as f32);
                        painter.circle_filled(egui::pos2(lx + 4.0 * k, ly), 4.0 * k, c);
                        text(&painter, egui::pos2(lx + 14.0 * k, ly - 9.0 * k), &format!("{name}  {pct:.0}%"), base - 1.0, t.fg, None);
                    }
                    y = r.bottom() + 26.0 * k;
                }

                // Claude Code's advice: a row per thing to change, with its share.
                if let Some(a) = &s.advice {
                    heading(&painter, &mut y, "What spends it");
                    let note = format!("From this machine's last 7 days: {} requests in {} sessions.", a.requests, a.sessions);
                    let g = galley(&painter, &t, &note, base - 2.0, t.fg_dim, None, width - 2.0 * k);
                    let gh = g.size().y;
                    painter.galley(egui::pos2(left + 2.0 * k, y - 4.0 * k), g, t.fg_dim);
                    y += gh + 8.0 * k;
                    // Each tip: its share in a pill, then its title and detail, wrapped beside it.
                    let pill_w = crate::chrome::text_width(&painter, "100%", sub_font.clone()) + (sub_font.size + 8.0).round() * 0.8;
                    let tw = (width - 2.0 * pad - pill_w - 14.0 * k).max(60.0 * k);
                    let tips: Vec<_> = a
                        .tips
                        .iter()
                        .map(|(pct, title, detail)| {
                            let title = galley(&painter, &t, title, base, t.fg, medium, tw);
                            let detail = detail.as_ref().map(|d| galley(&painter, &t, d, base - 2.0, t.fg_dim, None, tw));
                            let h = (20.0 * k + title.size().y + detail.as_ref().map_or(0.0, |d| d.size().y + 2.0 * k)).max(58.0 * k);
                            (pct, title, detail, h)
                        })
                        .collect();
                    let r = card(&painter, y, if tips.is_empty() { 58.0 * k } else { tips.iter().map(|tip| tip.3).sum() });
                    if tips.is_empty() {
                        text(&painter, egui::pos2(r.left() + pad, r.top() + 18.0 * k), "Nothing stands out.", base, t.fg_dim, None);
                    }
                    let mut ry = r.top();
                    for (i, (pct, title, detail, h)) in tips.into_iter().enumerate() {
                        if i > 0 {
                            painter.hline(r.left() + pad..=r.right() - pad, ry, Stroke::new(1.0, t.border));
                        }
                        let p = crate::chrome::pill(&painter, r.left() + pad, ry + h / 2.0, &format!("{pct:.0}%"), sub_font.clone(), t.fg_dim, t.chip(), false);
                        let tx = p.right() + 14.0 * k;
                        let th = title.size().y;
                        painter.galley(egui::pos2(tx, ry + 10.0 * k), title, t.fg);
                        if let Some(d) = detail {
                            painter.galley(egui::pos2(tx, ry + 12.0 * k + th), d, t.fg_dim);
                        }
                        ry += h;
                    }
                    y = r.bottom() + 20.0 * k;
                }
            }
        }

        // What its keys do.
        y += 18.0 * k;
        let hint = "g refresh  ·  q close  ·  claude-code-usage-advice turns the advice off";
        let g = galley(&painter, &t, hint, base - 2.0, t.fg_faint, None, width);
        y += g.size().y;
        painter.galley(egui::pos2(left + 2.0 * k, y - g.size().y), g, t.fg_faint);
        let tall = y - top + 56.0 * k;
        self.scroll = self.scroll.clamp(0.0, (tall - rect.height()).max(0.0));
        if let Some(cmd) = run {
            ed.main_sender().send(move |ed| ed.call(cmd));
        }
    }

    fn full_text(&mut self) -> Option<String> {
        Some(match &self.state {
            State::Asking(_) => "Asking Claude Code…\n".into(),
            State::Shown(s, _) => s.text(unix_now(), self.machine.as_deref()),
            State::Failed(e) => format!("Couldn't get the usage: {e}\n"),
        })
    }

    fn modeline(&self) -> Modeline {
        Modeline { title: "Claude Code usage".into(), busy: matches!(self.state, State::Asking(_)), ..Default::default() }
    }

    fn machine(&self) -> Option<String> {
        self.machine.clone()
    }

    impl_any!();
}

fn capitalized(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// `3d 4h`, `3h 12m`, `12m`: from `now` to `t`.
pub(crate) fn until(t: u64, now: u64) -> String {
    let s = t.saturating_sub(now);
    match s {
        0..60 => "under a minute".into(),
        60..3600 => format!("{}m", s / 60),
        3600..86_400 => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
        _ => format!("{}d {}h", s / 86_400, (s % 86_400) / 3600),
    }
}

/// Unix seconds of an RFC 3339 time (`2026-10-27T18:10:00.103+00:00`, `...Z`).
fn unix_seconds(s: &str) -> Option<u64> {
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, sec) = (num(0..4)?, num(5..7)?, num(8..10)?, num(11..13)?, num(14..16)?, num(17..19)?);
    let tz = s.get(19..)?.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    let offset = match tz.as_bytes().first()? {
        b'Z' | b'z' => 0,
        sign @ (b'+' | b'-') => {
            let (oh, om) = (tz.get(1..3)?.parse::<i64>().ok()?, tz.get(4..6)?.parse::<i64>().ok()?);
            (oh * 3600 + om * 60) * if *sign == b'-' { -1 } else { 1 }
        }
        _ => return None,
    };
    // Days from 1970-01-01 to y-mo-d (Howard Hinnant's days_from_civil).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (mo + if mo > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    u64::try_from(days * 86_400 + h * 3600 + mi * 60 + sec - offset).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn times_are_read_with_their_offset() {
        assert_eq!(unix_seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(unix_seconds("2026-10-27T18:10:00.103047+00:00"), Some(1_793_124_600));
        assert_eq!(unix_seconds("2026-10-27T20:10:00+02:00"), Some(1_793_124_600));
        assert_eq!(unix_seconds("soon"), None);
        assert_eq!(until(1000 + 3 * 3600 + 12 * 60, 1000), "3h 12m");
        assert_eq!(until(1000 + 2 * 86_400 + 5 * 3600, 1000), "2d 5h");
    }

    #[test]
    fn the_answer_is_read_into_windows_shares_and_advice() {
        let usage = json!({
            "subscription_type": "pro",
            "rate_limits_available": true,
            "rate_limits": {
                "five_hour": {"utilization": 42, "resets_at": "1970-01-01T03:12:00Z"},
                "seven_day": {"utilization": 7, "resets_at": "1970-01-03T00:00:00Z"},
                "seven_day_opus": null,
                "model_scoped": [{"display_name": "Fable", "utilization": 61, "resets_at": null}],
                "extra_usage": {"is_enabled": false},
                "seven_day_breakdown": {"rows": [{"display_name": "Claude Code", "percent": 100}, {"display_name": "Chats", "percent": 0}]},
            },
            "behaviors": {"week": {"request_count": 80, "session_count": 5, "recommendations": [{"title": "Start fresh when you switch tasks", "pct": 90, "detail": "run /clear"}]}},
        });
        let s = Summary::of(&usage);
        assert_eq!(s.plan.as_deref(), Some("Pro"));
        let labels: Vec<&str> = s.windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["Current session", "This week", "This week, Fable"]);
        assert_eq!((s.windows[0].pct, s.windows[0].resets), (Some(42.0), Some(3 * 3600 + 12 * 60)));
        assert_eq!(s.extra, Some((false, None)));
        assert_eq!(s.shares, [("Claude Code".to_string(), 100.0)]);
        let page = s.text(0, Some("box"));
        assert!(page.starts_with("Claude Code usage · Pro plan (on box)\n\nCurrent session: 42%, resets in 3h 12m\nThis week: 7%, resets in 2d 0h\nThis week, Fable: 61%\n"), "{page}");
        assert!(page.contains("Extra usage: off.\nThis week went to: Claude Code 100%.\n"), "{page}");
        assert!(page.contains("(80 requests in 5 sessions):\n- Start fresh when you switch tasks (90% of it): run /clear\n"), "{page}");
        let api = Summary::of(&json!({"subscription_type": null, "rate_limits_available": false, "rate_limits": null, "behaviors": null}));
        assert!(api.text(0, None).starts_with("Claude Code usage\n\nNo plan limits"));
        assert!(api.windows.is_empty() && api.extra.is_none());
    }
}
