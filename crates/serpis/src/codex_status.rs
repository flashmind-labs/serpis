//! Codex account and usage status, read from `codex app-server` without starting a model turn.

use std::time::Instant;

use egui::{Color32, Pos2, Rect, Stroke};
use serde_json::Value;
use serpis_proto::wire::{Request, Response};

use crate::buffer::{Buffer, Modeline};
use crate::claude_usage::{galley, level, until};
use crate::editor::Editor;
use crate::impl_any;

/// Ask `machine`'s Codex app-server, or this machine's when it is `None`.
pub fn fetch(
    ed: &mut Editor,
    machine: Option<String>,
    done: impl FnOnce(&mut Editor, Result<Value, String>) + 'static,
) {
    let who = machine.clone();
    let answered = move |ed: &mut Editor, response: Result<Response, String>| {
        let on = |error: String| {
            who.as_ref()
                .map(|m| format!("{m}: {error}"))
                .unwrap_or(error)
        };
        done(
            ed,
            match response {
                Ok(Response::CodexStatus { status }) => Ok(status),
                Ok(other) => Err(on(format!("unexpected answer {other:?}"))),
                Err(error) => Err(on(error)),
            },
        )
    };
    if let Some(machine) = machine {
        return crate::remote::request(ed, &machine, Request::CodexStatus, answered);
    }
    let set = ed
        .var_str("codex-path")
        .filter(|path| !path.is_empty())
        .map(|path| crate::expand_home(&path));
    let Some(binary) = set.or_else(|| ed.registry.codex_binary()) else {
        return answered(
            ed,
            Err("Codex isn't installed on this machine (codex not found; set codex-path)".into()),
        );
    };
    let asked = async move {
        serpis_ai::codex::status(&binary)
            .await
            .map(|status| Response::CodexStatus { status })
            .map_err(|error| format!("{error:#}"))
    };
    crate::remote::answer_with(ed, asked, answered);
}

/// Show fresh status for a local or remote Codex installation, in a page of cards like
/// `claude-code-usage`'s; the page says it is asking meanwhile.
pub fn show(ed: &mut Editor, machine: Option<String>) {
    let name = match &machine {
        Some(machine) => format!("*codex-status {machine}*"),
        None => "*codex-status*".to_string(),
    };
    let id = match ed.buffer_by_name(&name) {
        Some(id) => id,
        None => ed.add_buffer(
            &name,
            Box::new(StatusBuffer {
                machine: machine.clone(),
                state: State::Asking,
                scroll: 0.0,
            }),
        ),
    };
    ed.with_buffer_as::<StatusBuffer, _>(id, |buffer, _| buffer.state = State::Asking);
    ed.pop_to_buffer(id);
    fetch(ed, machine, move |ed, result| {
        ed.with_buffer_as::<StatusBuffer, _>(id, |buffer, _| {
            buffer.state = match result {
                Ok(status) => State::Shown(Box::new(Summary::of(&status)), Instant::now()),
                Err(error) => State::Failed(error),
            }
        });
    });
}

#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub plan: Option<String>,
    pub account: Option<String>,
    pub workspace: Option<String>,
    pub auth: Option<String>,
    pub windows: Vec<Window>,
    pub credits: Option<String>,
    pub reset_credits: Option<u64>,
    pub lifetime_tokens: Option<u64>,
    pub peak_daily_tokens: Option<u64>,
    pub current_streak_days: Option<u64>,
    pub longest_turn_seconds: Option<u64>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    pub label: String,
    pub used: f64,
    pub resets: Option<u64>,
}

impl Summary {
    pub fn of(status: &Value) -> Self {
        let account = &status["account"]["account"];
        let auth = account["type"]
            .as_str()
            .map(|kind| match kind {
                "chatgpt" => "ChatGPT".to_string(),
                "apiKey" => "API key".to_string(),
                "amazonBedrock" => "Amazon Bedrock".to_string(),
                "gcp" => "Google Cloud".to_string(),
                other => other.to_string(),
            })
            .or_else(|| {
                (status["account"]["requiresOpenaiAuth"] == true).then(|| "Not signed in".into())
            });
        let rate_limits = &status["rateLimits"];
        let mut snapshots: Vec<(&str, &Value)> = rate_limits["rateLimitsByLimitId"]
            .as_object()
            .into_iter()
            .flat_map(|limits| limits.iter().map(|(id, value)| (id.as_str(), value)))
            .collect();
        if snapshots.is_empty() && rate_limits["rateLimits"].is_object() {
            snapshots.push((
                rate_limits["rateLimits"]["limitId"]
                    .as_str()
                    .unwrap_or("codex"),
                &rate_limits["rateLimits"],
            ));
        }
        let mut windows = Vec::new();
        for (id, snapshot) in &snapshots {
            let bucket = snapshot["limitName"]
                .as_str()
                .filter(|name| !name.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| pretty(id));
            for (kind, window) in [
                ("primary", &snapshot["primary"]),
                ("secondary", &snapshot["secondary"]),
            ] {
                let Some(used) = window["usedPercent"].as_f64() else {
                    continue;
                };
                let duration = window["windowDurationMins"]
                    .as_u64()
                    .map(window_duration)
                    .unwrap_or_else(|| pretty(kind));
                windows.push(Window {
                    label: if snapshots.len() > 1 {
                        format!("{bucket}, {duration}")
                    } else {
                        capitalized(&duration)
                    },
                    used,
                    resets: window["resetsAt"].as_u64(),
                });
            }
        }
        let first = snapshots.first().map(|(_, snapshot)| *snapshot);
        let credits = snapshots.iter().find_map(|(_, snapshot)| {
            let credits = &snapshot["credits"];
            if credits["unlimited"] == true {
                Some("unlimited".to_string())
            } else if credits["hasCredits"] == true {
                Some(
                    credits["balance"]
                        .as_str()
                        .map(|balance| format!("{balance} remaining"))
                        .unwrap_or_else(|| "available".into()),
                )
            } else if credits.is_object() {
                Some("none remaining".into())
            } else {
                None
            }
        });
        let summary = &status["usage"]["summary"];
        let errors = [
            status["rateLimitsError"].as_str(),
            status["usageError"].as_str(),
        ]
        .into_iter()
        .flatten()
        .map(str::to_string)
        .collect();
        Self {
            plan: account["planType"]
                .as_str()
                .or_else(|| first.and_then(|snapshot| snapshot["planType"].as_str()))
                .map(capitalized),
            account: account["email"].as_str().map(str::to_string),
            workspace: account["workspaceName"].as_str().map(str::to_string),
            auth,
            windows,
            credits,
            reset_credits: rate_limits["rateLimitResetCredits"]["availableCount"].as_u64(),
            lifetime_tokens: summary["lifetimeTokens"].as_u64(),
            peak_daily_tokens: summary["peakDailyTokens"].as_u64(),
            current_streak_days: summary["currentStreakDays"].as_u64(),
            longest_turn_seconds: summary["longestRunningTurnSec"].as_u64(),
            errors,
        }
    }

    pub fn text(&self, machine: Option<&str>) -> String {
        let mut output = format!(
            "Codex status{}{}\n\n",
            self.plan
                .as_ref()
                .map(|plan| format!(" · {plan} plan"))
                .unwrap_or_default(),
            machine
                .map(|machine| format!(" (on {machine})"))
                .unwrap_or_default()
        );
        if let Some(account) = &self.account {
            output.push_str(&format!("Account: {account}\n"));
        }
        if let Some(workspace) = &self.workspace {
            output.push_str(&format!("Workspace: {workspace}\n"));
        }
        if let Some(auth) = &self.auth {
            output.push_str(&format!("Signed in with: {auth}\n"));
        }
        if !self.windows.is_empty() {
            output.push_str("\nRate limits\n\n");
            let now = unix_now();
            for window in &self.windows {
                let reset = window
                    .resets
                    .map(|at| format!(", resets in {}", until(at, now)))
                    .unwrap_or_default();
                output.push_str(&format!(
                    "{}: {:.0}% used, {:.0}% left{reset}\n",
                    window.label,
                    window.used,
                    (100.0 - window.used).max(0.0)
                ));
            }
        }
        if let Some(credits) = &self.credits {
            output.push_str(&format!("\nCredits: {credits}\n"));
        }
        if let Some(count) = self.reset_credits {
            output.push_str(&format!("Rate-limit resets available: {count}\n"));
        }
        if self.lifetime_tokens.is_some()
            || self.peak_daily_tokens.is_some()
            || self.current_streak_days.is_some()
            || self.longest_turn_seconds.is_some()
        {
            output.push_str("\nAccount activity\n\n");
            if let Some(tokens) = self.lifetime_tokens {
                output.push_str(&format!(
                    "Lifetime tokens: {}\n",
                    crate::buffers::chat::fmt_tokens(tokens)
                ));
            }
            if let Some(tokens) = self.peak_daily_tokens {
                output.push_str(&format!(
                    "Peak daily tokens: {}\n",
                    crate::buffers::chat::fmt_tokens(tokens)
                ));
            }
            if let Some(days) = self.current_streak_days {
                output.push_str(&format!("Current streak: {days} days\n"));
            }
            if let Some(seconds) = self.longest_turn_seconds {
                output.push_str(&format!("Longest turn: {}\n", elapsed(seconds)));
            }
        }
        for error in &self.errors {
            output.push_str(&format!("\nUnavailable: {error}\n"));
        }
        output
    }
}

enum State {
    Asking,
    Shown(Box<Summary>, Instant),
    Failed(String),
}

/// The `*codex-status*` page: `g` asks again, `q` closes it.
pub struct StatusBuffer {
    pub machine: Option<String>,
    state: State,
    scroll: f32,
}

impl StatusBuffer {
    pub fn summary(&self) -> Option<&Summary> {
        match &self.state {
            State::Shown(summary, _) => Some(summary),
            _ => None,
        }
    }
}

impl Buffer for StatusBuffer {
    fn kind(&self) -> &str {
        "codex-status"
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
        let mut run = false;

        // Wheel scrolling, for pages taller than the window.
        let response = ui.interact(rect, ui.scope_id().with("codex-status"), egui::Sense::hover());
        if response.hovered() {
            self.scroll -= ui.input(|input| input.smooth_scroll_delta.y);
        }
        let mut y = rect.top() + 28.0 * k - self.scroll;
        let top = y;
        let text = |p: &egui::Painter, pos: Pos2, s: &str, size: f32, color: Color32, weight: Option<f32>| -> egui::Vec2 {
            let g = galley(p, &t, s, size, color, weight, f32::INFINITY);
            let size = g.size();
            p.galley(pos, g, color);
            size
        };
        // Text ending at `right`.
        let text_right = |p: &egui::Painter, right: f32, y: f32, s: &str, size: f32, color: Color32, weight: Option<f32>| -> f32 {
            let g = galley(p, &t, s, size, color, weight, f32::INFINITY);
            let w = g.size().x;
            p.galley(egui::pos2(right - w, y), g, color);
            w
        };

        // The title, the plan, and a way to ask again.
        let title = galley(&painter, &t, "Codex status", base + 8.0, t.fg, bold, width);
        let title = {
            let size = title.size();
            painter.galley(egui::pos2(left, y), title, t.fg);
            size
        };
        let sub_font = egui::FontId::proportional(base - 2.0);
        // In a narrow window: Refresh goes first (g still asks), then the pill (the plan goes
        // under the title).
        let plan = match &self.state {
            State::Shown(summary, _) => summary.plan.as_ref().map(|plan| format!("{plan} plan")),
            _ => None,
        };
        let pill_w = plan.as_ref().map_or(0.0, |plan| crate::chrome::text_width(&painter, plan, sub_font.clone()) + (sub_font.size + 8.0).round() * 0.8 + 12.0 * k);
        let refresh = "Refresh";
        let rw = crate::chrome::text_width(&painter, refresh, sub_font.clone()) + 22.0 * k;
        let pill_fits = title.x + pill_w <= width;
        if let Some(plan) = plan.as_ref().filter(|_| pill_fits) {
            crate::chrome::pill(&painter, left + title.x + 12.0 * k, y + title.y / 2.0, plan, sub_font.clone(), t.accent, crate::chrome::tint(t.accent, 0.16), false);
        }
        if title.x + pill_w + 12.0 * k + rw <= width {
            let rb = Rect::from_min_size(egui::pos2(left + width - rw, y + title.y / 2.0 - 13.0 * k), egui::vec2(rw, 26.0 * k));
            let rresponse = ui.interact(rb, ui.scope_id().with("codex-status-refresh"), egui::Sense::click()).on_hover_text("Ask Codex again (g)");
            painter.rect_filled(rb, 8.0 * k, if rresponse.hovered() { crate::chrome::hover_bg(&t) } else { t.chip() });
            crate::chrome::text(&painter, egui::pos2(rb.left() + 11.0 * k, rb.center().y), refresh, sub_font.clone(), t.fg, rw);
            run |= rresponse.clicked();
        }
        y += title.y + 6.0 * k;
        let whose = self.machine.as_ref().map(|m| format!("Codex on {m}")).unwrap_or_else(|| "Codex on this machine".into());
        let whose = match plan.filter(|_| !pill_fits) {
            Some(plan) => format!("{plan} · {whose}"),
            None => whose,
        };
        let when = match &self.state {
            State::Asking => "asking…".to_string(),
            State::Shown(_, at) => match at.elapsed().as_secs() {
                0..60 => "updated just now".into(),
                seconds => format!("updated {} ago", until(seconds, 0)),
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
        // A card of rows, a name on the left and its value on the right; the card's bottom.
        // The value goes under its name when both don't fit on a line.
        let rows = |p: &egui::Painter, y: f32, rows: &[(&str, String)]| -> f32 {
            let laid: Vec<_> = rows
                .iter()
                .map(|(name, value)| {
                    let name = galley(p, &t, name, base, t.fg, medium, f32::INFINITY);
                    let value = galley(p, &t, value, base - 1.0, t.fg_dim, None, width - 2.0 * pad);
                    let one_line = name.size().x + 16.0 * k + value.size().x <= width - 2.0 * pad;
                    let h = if one_line { 44.0 * k } else { 36.0 * k + value.size().y + 14.0 * k };
                    (name, value, one_line, h)
                })
                .collect();
            let r = card(p, y, laid.iter().map(|row| row.3).sum());
            let mut ry = r.top();
            for (i, (name, value, one_line, h)) in laid.into_iter().enumerate() {
                if i > 0 {
                    p.hline(r.left() + pad..=r.right() - pad, ry, Stroke::new(1.0, t.border));
                }
                p.galley(egui::pos2(r.left() + pad, ry + 13.0 * k), name, t.fg);
                let at = if one_line { egui::pos2(r.right() - pad - value.size().x, ry + 14.0 * k) } else { egui::pos2(r.left() + pad, ry + 36.0 * k) };
                p.galley(at, value, t.fg_dim);
                ry += h;
            }
            r.bottom()
        };

        match &self.state {
            State::Asking => {
                let r = card(&painter, y, 64.0 * k);
                crate::chrome::spinner(&painter, egui::pos2(r.left() + pad + 8.0 * k, r.center().y), 8.0 * k, ui.input(|input| input.time), t.accent);
                let g = galley(&painter, &t, "Asking Codex…", base, t.fg_dim, None, f32::INFINITY);
                let gy = r.center().y - g.size().y / 2.0;
                painter.galley(egui::pos2(r.left() + pad + 28.0 * k, gy), g, t.fg_dim);
                ui.ctx().request_repaint();
                y = r.bottom();
            }
            State::Failed(error) => {
                let g = galley(&painter, &t, error, base, t.fg, None, width - 2.0 * pad);
                let r = card(&painter, y, g.size().y + 2.0 * pad + 24.0 * k);
                painter.rect_filled(Rect::from_min_size(r.min, egui::vec2(4.0 * k, r.height())), 2.0 * k, t.red);
                text(&painter, egui::pos2(r.left() + pad, r.top() + pad - 2.0 * k), "Couldn't get Codex status", base - 1.0, t.red, medium);
                painter.galley(egui::pos2(r.left() + pad, r.top() + pad + 22.0 * k), g, t.fg);
                y = r.bottom();
            }
            State::Shown(s, _) => {
                let now = unix_now();
                let account: Vec<(&str, String)> = [("Account", &s.account), ("Workspace", &s.workspace), ("Signed in with", &s.auth)]
                    .into_iter()
                    .filter_map(|(name, value)| Some((name, value.clone()?)))
                    .collect();
                if !account.is_empty() {
                    y = rows(&painter, y, &account) + 26.0 * k;
                }

                // One card, a row per window: its name, how full, its bar, when it resets.
                if !s.windows.is_empty() {
                    heading(&painter, &mut y, "Rate limits");
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
                        let color = level(&t, w.used);
                        text(&painter, egui::pos2(r.left() + pad, ry + 12.0 * k), &w.label, base, t.fg, medium);
                        let shown = format!("{:.0}%", w.used);
                        let pw = text_right(&painter, r.right() - pad, ry + 12.0 * k, &shown, base, if w.used >= 75.0 { color } else { t.fg }, bold);
                        if let Some(reset) = resets(w) {
                            if below {
                                text(&painter, egui::pos2(r.left() + pad, ry + 58.0 * k), &reset, base - 2.0, t.fg_dim, None);
                            } else {
                                text_right(&painter, r.right() - pad - pw - 16.0 * k, ry + 14.0 * k, &reset, base - 2.0, t.fg_dim, None);
                            }
                        }
                        let bar = Rect::from_min_size(egui::pos2(r.left() + pad, ry + 42.0 * k), egui::vec2(r.width() - 2.0 * pad, 8.0 * k));
                        painter.rect_filled(bar, 4.0 * k, t.chip());
                        let fill = (w.used.clamp(0.0, 100.0) / 100.0) as f32 * bar.width();
                        if fill > 0.0 {
                            painter.rect_filled(Rect::from_min_size(bar.min, egui::vec2(fill.max(8.0 * k), bar.height())), 4.0 * k, color);
                        }
                    }
                    y = r.bottom() + 12.0 * k;
                }
                let extra: Vec<(&str, String)> = [
                    s.credits.clone().map(|credits| ("Credits", capitalized(&credits))),
                    s.reset_credits.map(|count| ("Rate-limit resets", format!("{count} available"))),
                ]
                .into_iter()
                .flatten()
                .collect();
                if !extra.is_empty() {
                    y = rows(&painter, y, &extra) + 26.0 * k;
                } else {
                    y += 14.0 * k;
                }

                // What the account has done: a tile per number.
                let tiles: Vec<(String, &str)> = [
                    s.lifetime_tokens.map(|n| (crate::buffers::chat::fmt_tokens(n), "lifetime tokens")),
                    s.peak_daily_tokens.map(|n| (crate::buffers::chat::fmt_tokens(n), "peak day")),
                    s.current_streak_days.map(|n| (format!("{n} {}", if n == 1 { "day" } else { "days" }), "current streak")),
                    s.longest_turn_seconds.map(|n| (elapsed(n), "longest turn")),
                ]
                .into_iter()
                .flatten()
                .collect();
                if !tiles.is_empty() {
                    heading(&painter, &mut y, "Account activity");
                    let widest = tiles
                        .iter()
                        .map(|(value, name)| {
                            let value = galley(&painter, &t, value, base + 4.0, t.fg, bold, f32::INFINITY).size().x;
                            value.max(galley(&painter, &t, name, base - 2.0, t.fg, None, f32::INFINITY).size().x)
                        })
                        .fold(0.0, f32::max);
                    let columns = [tiles.len(), 2, 1].into_iter().find(|&n| width / n as f32 >= widest + 2.0 * pad).unwrap_or(1).min(tiles.len());
                    let tile_w = width / columns as f32;
                    let tile_h = 76.0 * k;
                    let r = card(&painter, y, tile_h * tiles.len().div_ceil(columns) as f32);
                    for (i, (value, name)) in tiles.iter().enumerate() {
                        let (x, ty) = (r.left() + tile_w * (i % columns) as f32, r.top() + tile_h * (i / columns) as f32);
                        if i % columns > 0 {
                            painter.vline(x, ty + 14.0 * k..=ty + tile_h - 14.0 * k, Stroke::new(1.0, t.border));
                        } else if i > 0 {
                            painter.hline(r.left() + pad..=r.right() - pad, ty, Stroke::new(1.0, t.border));
                        }
                        text(&painter, egui::pos2(x + pad, ty + 14.0 * k), value, base + 4.0, t.fg, bold);
                        text(&painter, egui::pos2(x + pad, ty + 46.0 * k), name, base - 2.0, t.fg_dim, None);
                    }
                    y = r.bottom() + 26.0 * k;
                }

                // What app-server couldn't say.
                for error in &s.errors {
                    let g = galley(&painter, &t, error, base - 1.0, t.fg, None, width - 2.0 * pad);
                    let r = card(&painter, y, g.size().y + 2.0 * pad + 22.0 * k);
                    painter.rect_filled(Rect::from_min_size(r.min, egui::vec2(4.0 * k, r.height())), 2.0 * k, t.yellow);
                    text(&painter, egui::pos2(r.left() + pad, r.top() + pad - 2.0 * k), "Unavailable", base - 1.0, t.yellow, medium);
                    painter.galley(egui::pos2(r.left() + pad, r.top() + pad + 20.0 * k), g, t.fg);
                    y = r.bottom() + 12.0 * k;
                }
            }
        }

        // What its keys do.
        y += 18.0 * k;
        let hint = "g refresh  ·  q close";
        let g = galley(&painter, &t, hint, base - 2.0, t.fg_faint, None, width);
        y += g.size().y;
        painter.galley(egui::pos2(left + 2.0 * k, y - g.size().y), g, t.fg_faint);
        let tall = y - top + 56.0 * k;
        self.scroll = self.scroll.clamp(0.0, (tall - rect.height()).max(0.0));
        if run {
            ed.main_sender().send(|ed| ed.call("codex-status-refresh"));
        }
    }

    fn full_text(&mut self) -> Option<String> {
        Some(match &self.state {
            State::Asking => "Asking Codex…\n".into(),
            State::Shown(summary, _) => summary.text(self.machine.as_deref()),
            State::Failed(error) => format!("Couldn't get Codex status: {error}\n"),
        })
    }

    fn modeline(&self) -> Modeline {
        Modeline {
            title: "Codex status".into(),
            busy: matches!(self.state, State::Asking),
            ..Default::default()
        }
    }

    fn machine(&self) -> Option<String> {
        self.machine.clone()
    }

    impl_any!();
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn capitalized(value: &str) -> String {
    let mut chars = value.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

fn pretty(value: &str) -> String {
    value
        .split(['_', '-'])
        .filter(|part| !part.is_empty())
        .map(capitalized)
        .collect::<Vec<_>>()
        .join(" ")
}

fn window_duration(minutes: u64) -> String {
    match minutes {
        10080 => "weekly limit".into(),
        0..60 => format!("{minutes}-minute limit"),
        minutes if minutes % 1440 == 0 => format!("{}-day limit", minutes / 1440),
        minutes if minutes % 60 == 0 => format!("{}-hour limit", minutes / 60),
        minutes => format!("{minutes}-minute limit"),
    }
}

fn elapsed(seconds: u64) -> String {
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m {}s", seconds / 60, seconds % 60),
        _ => format!("{}h {:02}m", seconds / 3600, (seconds % 3600) / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn status_is_read_into_account_limits_and_activity() {
        let status = json!({
            "account":{"account":{"type":"chatgpt","email":"me@example.test","planType":"pro","workspaceName":"Acme"}},
            "rateLimits":{
                "rateLimits":{"limitId":"codex","planType":"pro","primary":{"usedPercent":25,"windowDurationMins":300,"resetsAt":2000},"secondary":{"usedPercent":40,"windowDurationMins":10080,"resetsAt":3000},"credits":{"hasCredits":true,"unlimited":false,"balance":"12.5"}},
                "rateLimitsByLimitId":null,
                "rateLimitResetCredits":{"availableCount":2}
            },
            "usage":{"summary":{"lifetimeTokens":1234567,"peakDailyTokens":45678,"longestRunningTurnSec":540,"currentStreakDays":8}},
            "rateLimitsError":null,
            "usageError":null
        });
        let summary = Summary::of(&status);
        assert_eq!(summary.plan.as_deref(), Some("Pro"));
        assert_eq!(summary.account.as_deref(), Some("me@example.test"));
        assert_eq!(summary.windows.len(), 2);
        assert_eq!(summary.windows[0].label, "5-hour limit");
        assert_eq!(summary.windows[1].label, "Weekly limit");
        assert_eq!(summary.credits.as_deref(), Some("12.5 remaining"));
        assert_eq!(summary.reset_credits, Some(2));
        assert_eq!(summary.lifetime_tokens, Some(1_234_567));
        let text = summary.text(Some("workstation"));
        assert!(text.contains("25% used, 75% left"));
        assert!(text.contains("1.2M"));
        assert!(text.contains("on workstation"));
    }
}
