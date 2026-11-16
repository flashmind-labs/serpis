//! Pinned chats: persistent shortcuts in the sidebar and a buffer that can live in a split.

use std::time::{Duration, SystemTime};

use egui::{Color32, FontId, Rect, Sense, Stroke, StrokeKind};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::buffer::{ActivityState, Buffer, BufferId, Modeline};
use crate::editor::{Args, BUILTIN, Editor, buffer_id_from_u64, buffer_id_to_u64};
use crate::impl_any;

pub const KIND: &str = "pinned-chats";
pub const NAME: &str = "*Pinned chats*";

/// A chat's identity on its node. Buffer ids change between runs; this does not.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Pin {
    pub machine: String,
    pub id: String,
}

#[derive(Debug, Clone)]
pub struct PinnedChat {
    pub pin: Pin,
    pub buffer: Option<BufferId>,
    pub title: String,
    pub project: String,
    pub state: ActivityState,
    pub detail: String,
    pub attention: bool,
    pub age: Option<Duration>,
    pub available: bool,
}

impl PinnedChat {
    pub fn to_json(&self) -> Value {
        json!({
            "machine": self.pin.machine,
            "id": self.pin.id,
            "buffer": self.buffer.map(buffer_id_to_u64),
            "title": self.title,
            "project": self.project,
            "state": self.state.as_str(),
            "detail": self.detail,
            "attention": self.attention,
            "seconds": self.age.map(|a| a.as_secs()),
            "available": self.available,
        })
    }
}

fn stored(ed: &Editor) -> Vec<Pin> {
    let mut out: Vec<Pin> = ed.var("chat-pins").cloned().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
    let mut seen = std::collections::HashSet::new();
    out.retain(|p| !p.id.is_empty() && seen.insert(p.clone()));
    out
}

fn save(ed: &mut Editor, pins: &[Pin]) {
    let value = serde_json::to_value(pins).unwrap_or_else(|_| json!([]));
    ed.set_var("chat-pins", value.clone());
    ed.persist_state("chat-pins", value);
}

fn pin_for_buffer(ed: &mut Editor, id: BufferId) -> anyhow::Result<Pin> {
    ed.chat_id(Some(id))?;
    if let Some(info) = ed.chats.get(&id) {
        return Ok(Pin { machine: info.node.0.clone(), id: info.node.1.clone() });
    }
    ed.with_buffer_as::<crate::buffers::chat::ChatBuffer, _>(id, |chat, _| {
        let info = chat.info();
        Pin { machine: info.node.0, id: info.node.1 }
    })
    .ok_or_else(|| anyhow::anyhow!("chat is not ready yet"))
}

pub fn is_pinned(ed: &Editor, id: BufferId) -> bool {
    let Some(info) = ed.chats.get(&id) else { return false };
    stored(ed).iter().any(|p| p.machine == info.node.0 && p.id == info.node.1)
}

pub fn pin(ed: &mut Editor, id: BufferId) -> anyhow::Result<bool> {
    let pin = pin_for_buffer(ed, id)?;
    let mut pins = stored(ed);
    if pins.contains(&pin) {
        return Ok(false);
    }
    pins.push(pin);
    save(ed, &pins);
    Ok(true)
}

pub fn unpin(ed: &mut Editor, id: BufferId) -> anyhow::Result<bool> {
    let pin = pin_for_buffer(ed, id)?;
    Ok(unpin_key(ed, &pin))
}

pub fn unpin_key(ed: &mut Editor, pin: &Pin) -> bool {
    let mut pins = stored(ed);
    let n = pins.len();
    pins.retain(|p| p != pin);
    if pins.len() != n {
        save(ed, &pins);
        return true;
    }
    false
}

pub fn toggle(ed: &mut Editor, id: BufferId) -> anyhow::Result<bool> {
    if is_pinned(ed, id) {
        unpin(ed, id)?;
        Ok(false)
    } else {
        pin(ed, id)?;
        Ok(true)
    }
}

fn short_dir(path: &std::path::Path) -> String {
    path.file_name().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string())
}

/// The ordered pins as both the sidebar and pinned buffer show them.
pub fn list(ed: &mut Editor) -> Vec<PinnedChat> {
    let pins = stored(ed);
    let items = crate::sidebar::items(ed);
    pins
        .into_iter()
        .map(|pin| {
            let open = ed.chats.iter().find(|(_, info)| info.node.0 == pin.machine && info.node.1 == pin.id).map(|(id, _)| *id);
            if let Some(id) = open
                && let Some(item) = items.iter().find(|item| item.buffer == id)
            {
                let project = item.project.as_deref().map(short_dir).or_else(|| item.dir.as_deref().map(short_dir)).unwrap_or_else(|| pin.machine.clone());
                return PinnedChat {
                    pin,
                    buffer: Some(id),
                    title: item.label(),
                    project,
                    state: item.activity.state,
                    detail: item.activity.detail.clone(),
                    attention: item.attention,
                    age: Some(item.since.elapsed()),
                    available: true,
                };
            }
            if pin.machine == crate::remote::LOCAL
                && let Some(info) = ed.sessions.list.iter().find(|info| info.id == pin.id)
            {
                return PinnedChat {
                    pin,
                    buffer: None,
                    title: info.title(),
                    project: info.project.as_deref().map(short_dir).unwrap_or_else(|| short_dir(&info.cwd)),
                    state: ActivityState::Idle,
                    detail: format!("{} messages · saved", info.messages),
                    attention: false,
                    age: info.modified.and_then(|at| SystemTime::now().duration_since(at).ok()),
                    available: true,
                };
            }
            if let Some(info) = ed.remote.sessions.get(&pin.machine).and_then(|sessions| sessions.iter().find(|info| info.id == pin.id)) {
                let state = if info.running { ActivityState::Working } else { ActivityState::Idle };
                let detail = if info.running { "working".into() } else { format!("{} messages · on {}", info.messages, pin.machine) };
                return PinnedChat {
                    pin,
                    buffer: None,
                    title: info.title.clone(),
                    project: short_dir(std::path::Path::new(&info.cwd)),
                    state,
                    detail,
                    attention: false,
                    age: None,
                    available: true,
                };
            }
            PinnedChat {
                title: "Unavailable chat".into(),
                project: pin.machine.clone(),
                state: ActivityState::Error,
                detail: "session not found".into(),
                attention: false,
                age: None,
                available: false,
                buffer: None,
                pin,
            }
        })
        .collect()
}

pub fn open(ed: &mut Editor, pin: &Pin) {
    if let Some(id) = ed.chats.iter().find(|(_, info)| info.node.0 == pin.machine && info.node.1 == pin.id).map(|(id, _)| *id) {
        ed.open_buffer(id);
        return;
    }
    if pin.machine == crate::remote::LOCAL {
        let path = ed.sessions.list.iter().find(|info| info.id == pin.id).map(|info| info.path.clone());
        match path {
            Some(path) => {
                if let Err(error) = crate::session_ui::resume(ed, &path) {
                    ed.error(format!("opening pinned chat: {error:#}"));
                }
            }
            None => ed.warn("The pinned chat's saved session was not found"),
        }
        return;
    }
    let path = ed.remote.sessions.get(&pin.machine).and_then(|sessions| sessions.iter().find(|info| info.id == pin.id)).map(|info| info.path.clone());
    match path {
        Some(path) => crate::remote::resume_on(ed, &pin.machine, &path),
        None => ed.warn(format!("The pinned chat was not found on {}", pin.machine)),
    }
}

pub fn show(ed: &mut Editor, split: bool) -> anyhow::Result<()> {
    let id = match ed.buffer_by_name(NAME) {
        Some(id) => id,
        None => ed.create_buffer(KIND, Some(NAME), &Value::Null)?,
    };
    if split {
        ed.pop_to_buffer(id);
    } else {
        ed.open_buffer(id);
    }
    crate::session_ui::scan_then(ed, false, |_| {});
    Ok(())
}

pub struct PinnedChatsBuffer {
    selected: usize,
    scroll: f32,
}

impl PinnedChatsBuffer {
    fn move_by(&mut self, n: isize, len: usize) {
        if len > 0 {
            self.selected = (self.selected as isize + n).clamp(0, len as isize - 1) as usize;
        }
    }
}

fn state_color(t: &crate::theme::Theme, state: ActivityState) -> Color32 {
    match state {
        ActivityState::Working => t.accent,
        ActivityState::Waiting => t.yellow,
        ActivityState::Error => t.red,
        ActivityState::Idle | ActivityState::Exited => t.fg_faint,
    }
}

/// A compact pushpin glyph used by the sidebar and header controls.
pub fn icon(p: &egui::Painter, r: Rect, color: Color32) {
    let c = r.center();
    let s = r.width().min(r.height());
    let head = Rect::from_center_size(egui::pos2(c.x, c.y - s * 0.16), egui::vec2(s * 0.48, s * 0.28));
    p.rect_stroke(head, 1.5, Stroke::new(1.4, color), StrokeKind::Middle);
    p.line_segment([egui::pos2(c.x - s * 0.28, c.y), egui::pos2(c.x + s * 0.28, c.y)], Stroke::new(1.4, color));
    p.line_segment([egui::pos2(c.x, c.y), egui::pos2(c.x, c.y + s * 0.38)], Stroke::new(1.4, color));
}

impl Buffer for PinnedChatsBuffer {
    fn kind(&self) -> &str {
        KIND
    }

    fn ui(&mut self, ui: &mut egui::Ui, rect: Rect, _focused: bool, ed: &mut Editor) {
        let t = ed.theme.clone();
        let k = t.ui_scale();
        let pins = list(ed);
        self.selected = self.selected.min(pins.len().saturating_sub(1));
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, t.bg);
        let width = (rect.width() - 48.0 * k).clamp(220.0, 760.0 * k);
        let left = rect.center().x - width / 2.0;
        let top = rect.top() + 24.0 * k;
        crate::chrome::text(&painter, egui::pos2(left, top + 10.0 * k), "Pinned chats", FontId::proportional(t.font_size + 7.0), t.fg, width);
        let summary = format!("{} shortcut{} across projects", pins.len(), if pins.len() == 1 { "" } else { "s" });
        crate::chrome::text(&painter, egui::pos2(left, top + 38.0 * k), &summary, FontId::proportional(t.font_size - 2.0), t.fg_dim, width);
        let list_rect = Rect::from_min_max(egui::pos2(left, top + 62.0 * k), egui::pos2(left + width, rect.bottom() - 18.0 * k));
        let response = ui.interact(list_rect, ui.scope_id().with("pinned-chats-scroll"), Sense::hover());
        if response.hovered() {
            self.scroll = (self.scroll - ui.input(|input| input.smooth_scroll_delta.y)).max(0.0);
        }
        let mut y = list_rect.top() - self.scroll;
        let mut open_pin = None;
        let mut remove = None;
        if pins.is_empty() {
            crate::chrome::text(&painter, egui::pos2(left, y + 22.0 * k), "No pinned chats. Pin one from its sidebar row or window header.", FontId::proportional(t.font_size), t.fg_faint, width);
        }
        for (i, chat) in pins.iter().enumerate() {
            let card = Rect::from_min_size(egui::pos2(left, y), egui::vec2(width, 72.0 * k));
            y = card.bottom() + 8.0 * k;
            if !card.intersects(list_rect) {
                continue;
            }
            let card = card.intersect(list_rect);
            let id = ui.scope_id().with(("pinned-chat", &chat.pin.machine, &chat.pin.id));
            let resp = ui.interact(card, id, Sense::click());
            let bg = if i == self.selected { crate::chrome::selected_bg(&t) } else if resp.hovered() { crate::chrome::hover_bg(&t) } else { t.bg_elevated };
            painter.rect_filled(card, 10.0 * k, bg);
            painter.rect_stroke(card, 10.0 * k, Stroke::new(1.0, if i == self.selected { crate::chrome::tint(t.accent, 0.55) } else { t.border }), StrokeKind::Inside);
            let col = state_color(&t, chat.state);
            let icon = egui::pos2(card.left() + 18.0 * k, card.top() + 22.0 * k);
            match chat.state {
                ActivityState::Working => crate::chrome::spinner(&painter, icon, 5.0 * k, ui.input(|input| input.time), col),
                ActivityState::Waiting => { painter.circle_filled(icon, 4.0 * k, col); }
                ActivityState::Error => crate::chrome::cross(&painter, Rect::from_center_size(icon, egui::Vec2::splat(9.0 * k)), col),
                _ => crate::chrome::bubble(&painter, icon, 9.0 * k, col),
            }
            let tx = card.left() + 34.0 * k;
            crate::chrome::text(&painter, egui::pos2(tx, card.top() + 22.0 * k), &chat.title, FontId::proportional(t.font_size + 0.5), t.fg, card.width() - 96.0 * k);
            let slot = format!("{}", i + 1);
            crate::chrome::pill(&painter, card.right() - 34.0 * k, card.top() + 22.0 * k, &slot, FontId::monospace(t.font_size - 3.0), t.fg_dim, t.chip(), true);
            let detail = if chat.project.is_empty() { chat.detail.clone() } else { format!("{} · {}", chat.project, chat.detail) };
            crate::chrome::text(&painter, egui::pos2(tx, card.bottom() - 20.0 * k), &detail, FontId::proportional(t.font_size - 2.0), if chat.attention { col } else { t.fg_dim }, card.width() - 86.0 * k);
            let unpin = Rect::from_center_size(egui::pos2(card.right() - 17.0 * k, card.bottom() - 18.0 * k), egui::Vec2::splat(22.0 * k));
            if crate::chrome::ghost_button(ui, unpin, id.with("unpin"), &t, "Unpin chat", crate::chrome::cross).clicked() {
                remove = Some(chat.pin.clone());
            }
            if resp.clicked() {
                self.selected = i;
                open_pin = Some(chat.pin.clone());
            }
        }
        self.scroll = self.scroll.min((y - list_rect.bottom()).max(0.0));
        if pins.iter().any(|pin| pin.state == ActivityState::Working) {
            ui.ctx().request_repaint_after(Duration::from_millis(60));
        }
        if let Some(pin) = remove {
            unpin_key(ed, &pin);
        }
        if let Some(pin) = open_pin {
            open(ed, &pin);
        }
    }

    fn modeline(&self) -> Modeline {
        Modeline { title: "Pinned chats".into(), ..Default::default() }
    }

    impl_any!();
}

fn target(ed: &Editor, args: &Args) -> BufferId {
    args.params.first().and_then(Value::as_u64).map(buffer_id_from_u64).unwrap_or_else(|| ed.current_buffer())
}

fn selected_pin(ed: &mut Editor) -> Option<Pin> {
    let id = ed.current_buffer();
    let selected = ed.with_buffer_as::<PinnedChatsBuffer, _>(id, |buffer, _| buffer.selected)?;
    list(ed).get(selected).map(|chat| chat.pin.clone())
}

pub fn install(ed: &mut Editor) {
    let o = BUILTIN;
    ed.defvar("chat-pins", Editor::persisted_state("chat-pins").unwrap_or_else(|| json!([])), "Chats pinned for quick access. Use chat-pin, chat-unpin and pinned-chats rather than editing this list.");
    ed.define_buffer_kind(KIND, o, |_, _| Ok(Box::new(PinnedChatsBuffer { selected: 0, scroll: 0.0 })));
    ed.define_command("chat-pin", "Pin a chat for quick access (param: buffer; default: current).", o, |ed, args| {
        let id = target(ed, args);
        if pin(ed, id)? {
            ed.message(format!("Pinned {}", ed.buffer_display_name(id)));
        }
        Ok(())
    });
    ed.define_command("chat-unpin", "Remove a chat from the pinned list (param: buffer; default: current).", o, |ed, args| {
        let id = target(ed, args);
        if unpin(ed, id)? {
            ed.message(format!("Unpinned {}", ed.buffer_display_name(id)));
        }
        Ok(())
    });
    ed.define_command("chat-pin-toggle", "Pin or unpin a chat (param: buffer; default: current).", o, |ed, args| {
        let id = target(ed, args);
        let on = toggle(ed, id)?;
        ed.message(format!("{} {}", if on { "Pinned" } else { "Unpinned" }, ed.buffer_display_name(id)));
        Ok(())
    });
    ed.define_command("chat-pin-open", "Open a pinned chat by its one-based slot (param or prefix; default: 1).", o, |ed, args| {
        let slot = args.params.first().and_then(Value::as_u64).map(|n| n as usize).or_else(|| args.prefix.map(|n| n.max(1) as usize)).unwrap_or(1);
        let pin = stored(ed).get(slot.saturating_sub(1)).cloned().ok_or_else(|| anyhow::anyhow!("No pinned chat in slot {slot}"))?;
        open(ed, &pin);
        Ok(())
    });
    ed.define_command("pinned-chats", "Open the pinned chats buffer.", o, |ed, _| show(ed, false));
    ed.define_command("pinned-chats-split", "Open the pinned chats buffer in another window.", o, |ed, _| show(ed, true));
    ed.define_command("pinned-chats-next", "Select the next pinned chat.", o, |ed, _| {
        let len = stored(ed).len();
        let id = ed.current_buffer();
        ed.with_buffer_as::<PinnedChatsBuffer, _>(id, |buffer, _| buffer.move_by(1, len));
        Ok(())
    });
    ed.define_command("pinned-chats-previous", "Select the previous pinned chat.", o, |ed, _| {
        let len = stored(ed).len();
        let id = ed.current_buffer();
        ed.with_buffer_as::<PinnedChatsBuffer, _>(id, |buffer, _| buffer.move_by(-1, len));
        Ok(())
    });
    ed.define_command("pinned-chats-open", "Open the selected pinned chat.", o, |ed, _| {
        if let Some(pin) = selected_pin(ed) {
            open(ed, &pin);
        }
        Ok(())
    });
    ed.define_command("pinned-chats-unpin", "Unpin the selected chat.", o, |ed, _| {
        if let Some(pin) = selected_pin(ed) {
            unpin_key(ed, &pin);
        }
        Ok(())
    });
    for map in [KIND, "vim-normal:pinned-chats"] {
        for (key, command) in [("<down>", "pinned-chats-next"), ("j", "pinned-chats-next"), ("<up>", "pinned-chats-previous"), ("k", "pinned-chats-previous"), ("RET", "pinned-chats-open"), ("u", "pinned-chats-unpin")] {
            ed.bind(map, key, command, o);
        }
    }
    ed.add_segment(true, "chat-pin", o, |ed, id| {
        let id = id?;
        (ed.meta(id)?.major == "chat").then(|| if is_pinned(ed, id) { "● Pinned".into() } else { "○ Pin".into() })
    });
    ed.set_segment_command("chat-pin", Some("chat-pin-toggle".into()));
}
