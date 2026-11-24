//! Antigravity plan usage, read from `agy -p /usage --output-format stream-json` without a model turn.

use std::time::Instant;

use egui::{Color32, Pos2, Rect, Stroke};
use serde_json::Value;
use serpis_proto::wire::{Request, Response};

use crate::buffer::{Buffer, Modeline};
use crate::claude_usage::{galley, level, until};
use crate::editor::Editor;
use crate::impl_any;

/// Ask `machine`'s Antigravity CLI (`agy`), or this machine's when it is `None`.
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
                Ok(Response::AntigravityUsage { usage }) => Ok(usage),
                Ok(other) => Err(on(format!("unexpected answer {other:?}"))),
                Err(error) => Err(on(error)),
            },
        )
    };
    if let Some(machine) = machine {
        return crate::remote::request(ed, &machine, Request::AntigravityUsage, answered);
    }
    let set = ed
        .var_str("antigravity-path")
        .filter(|path| !path.is_empty())
        .map(|path| crate::expand_home(&path));
    let Some(binary) = set.or_else(|| ed.registry.antigravity_binary()) else {
        return answered(
            ed,
            Err("Antigravity isn't installed on this machine (agy not found; set antigravity-path)".into()),
        );
    };
    let asked = async move {
        serpis_ai::antigravity::usage(&binary)
            .await
            .map(|usage| Response::AntigravityUsage { usage })
            .map_err(|error| format!("{error:#}"))
    };
    crate::remote::answer_with(ed, asked, answered);
}

/// Show fresh usage for a local or remote Antigravity installation, in a page of cards;
/// the page says it is asking meanwhile.
pub fn show(ed: &mut Editor, machine: Option<String>) {
    let name = match &machine {
        Some(machine) => format!("*antigravity-usage {machine}*"),
        None => "*antigravity-usage*".to_string(),
    };
    let id = match ed.buffer_by_name(&name) {
        Some(id) => id,
        None => ed.add_buffer(
            &name,
            Box::new(UsageBuffer {
                machine: machine.clone(),
                state: State::Asking,
                scroll: 0.0,
            }),
        ),
    };
    ed.with_buffer_as::<UsageBuffer, _>(id, |buffer, _| buffer.state = State::Asking);
    ed.pop_to_buffer(id);
    fetch(ed, machine, move |ed, result| {
        ed.with_buffer_as::<UsageBuffer, _>(id, |buffer, _| {
            buffer.state = match result {
                Ok(usage) => State::Shown(Box::new(Summary::of(&usage)), Instant::now()),
                Err(error) => State::Failed(error),
            }
        });
    });
}

#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub description: Option<String>,
    pub groups: Vec<Group>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub name: String,
    pub description: Option<String>,
    pub windows: Vec<Window>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    pub label: String,
    pub used: f64,
    pub resets: Option<u64>,
}

impl Summary {
    pub fn of(usage: &Value) -> Self {
        let description = usage["description"].as_str().map(str::to_string);
        let groups = usage["groups"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|group| {
                let name = group["name"].as_str()?.to_string();
                let description = group["description"].as_str().map(str::to_string);
                let windows: Vec<Window> = group["buckets"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|bucket| {
                        let left = bucket["remaining_fraction"].as_f64()?.clamp(0.0, 1.0);
                        let label = match bucket["window"].as_str() {
                            Some("5h") => "5-hour window".to_string(),
                            Some("weekly") => "Weekly window".to_string(),
                            Some("daily") => "Daily window".to_string(),
                            _ => bucket["name"].as_str().unwrap_or("Limit").to_string(),
                        };
                        let resets = (left < 1.0)
                            .then(|| bucket["reset_time"].as_str())
                            .flatten()
                            .and_then(parse_rfc3339_secs);
                        Some(Window {
                            label,
                            used: (1.0 - left) * 100.0,
                            resets,
                        })
                    })
                    .collect();
                Some(Group {
                    name,
                    description,
                    windows,
                })
            })
            .collect();
        Self {
            description,
            groups,
        }
    }

    pub fn text(&self, machine: Option<&str>) -> String {
        let mut output = format!(
            "Antigravity usage{}\n\n",
            machine
                .map(|machine| format!(" (on {machine})"))
                .unwrap_or_default()
        );
        let now = unix_now();
        for group in &self.groups {
            output.push_str(&format!("{}\n", group.name));
            if let Some(desc) = &group.description {
                output.push_str(&format!("{desc}\n"));
            }
            for window in &group.windows {
                let reset = window
                    .resets
                    .map(|at| format!(", resets in {}", until(at, now)))
                    .unwrap_or_default();
                output.push_str(&format!(
                    "  {}: {:.0}% used, {:.0}% left{reset}\n",
                    window.label,
                    window.used,
                    (100.0 - window.used).max(0.0)
                ));
            }
            output.push('\n');
        }
        output
    }
}

enum State {
    Asking,
    Shown(Box<Summary>, Instant),
    Failed(String),
}

/// The `*antigravity-usage*` page: `g` asks again, `q` closes it.
pub struct UsageBuffer {
    pub machine: Option<String>,
    state: State,
    scroll: f32,
}

impl UsageBuffer {
    pub fn summary(&self) -> Option<&Summary> {
        match &self.state {
            State::Shown(summary, _) => Some(summary),
            _ => None,
        }
    }
}

impl Buffer for UsageBuffer {
    fn kind(&self) -> &str {
        "antigravity-usage"
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

        let response = ui.interact(
            rect,
            ui.scope_id().with("antigravity-usage"),
            egui::Sense::hover(),
        );
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
        let text_right = |p: &egui::Painter, right: f32, y: f32, s: &str, size: f32, color: Color32, weight: Option<f32>| -> f32 {
            let g = galley(p, &t, s, size, color, weight, f32::INFINITY);
            let w = g.size().x;
            p.galley(egui::pos2(right - w, y), g, color);
            w
        };

        let title = galley(&painter, &t, "Antigravity usage", base + 8.0, t.fg, bold, width);
        let title_size = title.size();
        painter.galley(egui::pos2(left, y), title, t.fg);

        let sub_font = egui::FontId::proportional(base - 2.0);
        let refresh = "Refresh";
        let rw = crate::chrome::text_width(&painter, refresh, sub_font.clone()) + 22.0 * k;
        if title_size.x + 12.0 * k + rw <= width {
            let rb = Rect::from_min_size(
                egui::pos2(left + width - rw, y + title_size.y / 2.0 - 13.0 * k),
                egui::vec2(rw, 26.0 * k),
            );
            let rresponse = ui
                .interact(rb, ui.scope_id().with("antigravity-usage-refresh"), egui::Sense::click())
                .on_hover_text("Ask Antigravity again (g)");
            painter.rect_filled(
                rb,
                8.0 * k,
                if rresponse.hovered() {
                    crate::chrome::hover_bg(&t)
                } else {
                    t.chip()
                },
            );
            crate::chrome::text(
                &painter,
                egui::pos2(rb.left() + 11.0 * k, rb.center().y),
                refresh,
                sub_font,
                t.fg,
                rw,
            );
            run |= rresponse.clicked();
        }
        y += title_size.y + 6.0 * k;

        let whose = self
            .machine
            .as_ref()
            .map(|m| format!("Antigravity on {m}"))
            .unwrap_or_else(|| "Antigravity on this machine".into());
        let when = match &self.state {
            State::Asking => "asking…".to_string(),
            State::Shown(_, at) => match at.elapsed().as_secs() {
                0..60 => "updated just now".into(),
                seconds => format!("updated {} ago", until(seconds, 0)),
            },
            State::Failed(_) => "couldn't ask".into(),
        };
        let g = galley(&painter, &t, &format!("{whose} · {when}"), base - 2.0, t.fg_dim, None, width);
        let gy = g.size().y;
        painter.galley(egui::pos2(left, y), g, t.fg_dim);
        y += gy + 22.0 * k;

        let card = |p: &egui::Painter, y: f32, h: f32| {
            let r = Rect::from_min_size(egui::pos2(left, y), egui::vec2(width, h));
            p.rect_filled(r, 12.0 * k, t.bg_elevated);
            p.rect_stroke(r, 12.0 * k, Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
            r
        };
        let pad = 18.0 * k;

        match &self.state {
            State::Asking => {
                let r = card(&painter, y, 64.0 * k);
                crate::chrome::spinner(
                    &painter,
                    egui::pos2(r.left() + pad + 8.0 * k, r.center().y),
                    8.0 * k,
                    ui.input(|input| input.time),
                    t.accent,
                );
                let g = galley(&painter, &t, "Asking Antigravity…", base, t.fg_dim, None, f32::INFINITY);
                let gy = r.center().y - g.size().y / 2.0;
                painter.galley(egui::pos2(r.left() + pad + 28.0 * k, gy), g, t.fg_dim);
                ui.ctx().request_repaint();
                y = r.bottom();
            }
            State::Failed(error) => {
                let g = galley(&painter, &t, error, base, t.fg, None, width - 2.0 * pad);
                let r = card(&painter, y, g.size().y + 2.0 * pad + 24.0 * k);
                painter.rect_filled(
                    Rect::from_min_size(r.min, egui::vec2(4.0 * k, r.height())),
                    2.0 * k,
                    t.red,
                );
                text(
                    &painter,
                    egui::pos2(r.left() + pad, r.top() + pad - 2.0 * k),
                    "Couldn't get Antigravity usage",
                    base - 1.0,
                    t.red,
                    medium,
                );
                painter.galley(egui::pos2(r.left() + pad, r.top() + pad + 22.0 * k), g, t.fg);
                y = r.bottom();
            }
            State::Shown(s, _) => {
                let now = unix_now();
                for group in &s.groups {
                    y += text(&painter, egui::pos2(left + 2.0 * k, y), &group.name, base - 1.0, t.fg_dim, medium).y + 4.0 * k;
                    if let Some(desc) = &group.description {
                        let dg = galley(&painter, &t, desc, base - 2.0, t.fg_faint, None, width - 4.0 * k);
                        let dh = dg.size().y;
                        painter.galley(egui::pos2(left + 2.0 * k, y), dg, t.fg_faint);
                        y += dh + 8.0 * k;
                    } else {
                        y += 6.0 * k;
                    }
                    if group.windows.is_empty() {
                        continue;
                    }
                    let resets = |w: &Window| w.resets.map(|at| format!("resets in {}", until(at, now)));
                    let measure = |s: &str, size: f32, weight: Option<f32>| {
                        galley(&painter, &t, s, size, t.fg, weight, f32::INFINITY).size().x
                    };
                    let below = group.windows.iter().any(|w| {
                        let reset = resets(w).map_or(0.0, |r| measure(&r, base - 2.0, None) + 16.0 * k);
                        measure(&w.label, base, medium) + 16.0 * k + reset + measure("100%", base, bold) > width - 2.0 * pad
                    });
                    let row_h = if below { 84.0 * k } else { 66.0 * k };
                    let r = card(&painter, y, row_h * group.windows.len() as f32 + 8.0 * k);
                    for (i, w) in group.windows.iter().enumerate() {
                        let ry = r.top() + 4.0 * k + row_h * i as f32;
                        if i > 0 {
                            painter.hline(r.left() + pad..=r.right() - pad, ry, Stroke::new(1.0, t.border));
                        }
                        let color = level(&t, w.used);
                        text(&painter, egui::pos2(r.left() + pad, ry + 12.0 * k), &w.label, base, t.fg, medium);
                        let shown = format!("{:.0}%", w.used);
                        let pw = text_right(
                            &painter,
                            r.right() - pad,
                            ry + 12.0 * k,
                            &shown,
                            base,
                            if w.used >= 75.0 { color } else { t.fg },
                            bold,
                        );
                        if let Some(reset) = resets(w) {
                            if below {
                                text(&painter, egui::pos2(r.left() + pad, ry + 58.0 * k), &reset, base - 2.0, t.fg_dim, None);
                            } else {
                                text_right(&painter, r.right() - pad - pw - 16.0 * k, ry + 14.0 * k, &reset, base - 2.0, t.fg_dim, None);
                            }
                        }
                        let bar = Rect::from_min_size(
                            egui::pos2(r.left() + pad, ry + 42.0 * k),
                            egui::vec2(r.width() - 2.0 * pad, 8.0 * k),
                        );
                        painter.rect_filled(bar, 4.0 * k, t.chip());
                        let fill = (w.used.clamp(0.0, 100.0) / 100.0) as f32 * bar.width();
                        if fill > 0.0 {
                            painter.rect_filled(
                                Rect::from_min_size(bar.min, egui::vec2(fill.max(8.0 * k), bar.height())),
                                4.0 * k,
                                color,
                            );
                        }
                    }
                    y = r.bottom() + 20.0 * k;
                }
            }
        }

        y += 12.0 * k;
        let hint = "g refresh  ·  q close";
        let g = galley(&painter, &t, hint, base - 2.0, t.fg_faint, None, width);
        let gh = g.size().y;
        painter.galley(egui::pos2(left + 2.0 * k, y), g, t.fg_faint);
        y += gh;
        let tall = y - top + 56.0 * k;
        self.scroll = self.scroll.clamp(0.0, (tall - rect.height()).max(0.0));
        if run {
            ed.main_sender().send(|ed| ed.call("antigravity-usage-refresh"));
        }
    }

    fn full_text(&mut self) -> Option<String> {
        Some(match &self.state {
            State::Asking => "Asking Antigravity…\n".into(),
            State::Shown(summary, _) => summary.text(self.machine.as_deref()),
            State::Failed(error) => format!("Couldn't get Antigravity usage: {error}\n"),
        })
    }

    fn modeline(&self) -> Modeline {
        Modeline {
            title: "Antigravity usage".into(),
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

/// Parse an RFC 3339 UTC timestamp (`YYYY-MM-DDTHH:MM:SSZ`) into Unix seconds.
fn parse_rfc3339_secs(s: &str) -> Option<u64> {
    let s = s.trim().strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-');
    let year: i64 = d.next()?.parse().ok()?;
    let month: u32 = d.next()?.parse().ok()?;
    let day: u32 = d.next()?.parse().ok()?;
    let time = time.split('.').next()?;
    let mut t = time.split(':');
    let hour: u64 = t.next()?.parse().ok()?;
    let min: u64 = t.next()?.parse().ok()?;
    let sec: u64 = t.next()?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || min > 59 || sec > 59 {
        return None;
    }
    // Civil date to days since 1970-01-01 (Howard Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = (y - era * 400) as u64;
    let m = month as u64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe as i64 - 719_468;
    if days < 0 {
        return None;
    }
    Some(days as u64 * 86_400 + hour * 3_600 + min * 60 + sec)
}
