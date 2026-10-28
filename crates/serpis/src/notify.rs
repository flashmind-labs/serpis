//! Desktop notifications when something you'd want to know about happens out of sight: a
//! chat's turn ends (on serpis's own agent or Claude Code, here or on another machine), an
//! agent waits for you (a permission, its questions, a plan), a run fails, or a terminal's
//! command finishes. Only when you aren't looking (`notify-when`), and a turn or command only
//! when it ran a while (`notify-after`). A click on one brings serpis to that buffer.
//! Scripts send their own with `serpis.notify`, and change or drop any in the `notify` hook.

use std::sync::Mutex;
use std::time::Duration;

use serde_json::{Value, json};

use crate::buffer::{Activity, ActivityState, BufferId};
use crate::editor::{BUILTIN, Editor, buffer_id_from_u64, buffer_id_to_u64};

/// A notification, as sent (and as `serpis.debug.notifications` lists them).
#[derive(Debug, Clone, serde::Serialize)]
pub struct Sent {
    pub title: String,
    pub body: String,
    /// `done`, `waiting`, `failed`, or `script`.
    pub event: String,
    pub buffer: Option<u64>,
}

static SENT: Mutex<Vec<Sent>> = Mutex::new(Vec::new());

/// What was sent so far (the last 100).
pub fn sent() -> Vec<Sent> {
    SENT.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

pub fn install(ed: &mut Editor) {
    let o = BUILTIN;
    ed.defvar("notify", json!(true), "Send desktop notifications when a chat's turn ends, an agent waits for you or fails, or a terminal's command finishes, while you aren't looking (notify-when). Which: notify-events.");
    ed.defvar("notify-events", json!(["done", "waiting", "failed"]), "What notifies: \"done\" (a chat's turn ended, a terminal's command finished: after notify-after seconds), \"waiting\" (an agent waits for you: a permission, its questions, a plan), \"failed\" (a run ended in an error).");
    ed.defvar("notify-after", json!(15), "Seconds a turn or a terminal's command must have run for its end to notify (\"done\"); what waits for you or failed notifies at once.");
    ed.defvar("notify-when", json!("unseen"), "When to notify: \"unseen\" (serpis isn't the focused app, or the buffer isn't on screen), \"away\" (serpis isn't the focused app) or \"always\".");
    ed.defvar("notify-terminals", json!(true), "Also notify when a terminal's command finishes (after notify-after seconds), and when an agent run in one waits.");
    ed.defvar("notify-sound", json!(true), "Notifications play the system's sound.");
    ed.defvar("notify-app", json!(""), "macOS: the app notifications come from (its icon, a bundle id like \"com.mitchellh.ghostty\"); \"\" picks: the app serpis was started from (the terminal, Serpis.app), else Terminal.");
    ed.defvar("notify-backend", json!(if std::env::var_os("SERPIS_TEST_SCRIPT").is_some() { "none" } else { "system" }), "\"system\" shows notifications on the desktop; \"none\" only logs them (and lists them for serpis.debug.notifications), as tests do.");
    ed.define_command("notify-test", "Send a test notification, to see how they look (and that the system lets serpis send them).", o, |ed, _| {
        send(ed, "serpis".into(), "Notifications work. A click brings you back here.".into(), "script", Some(ed.current_buffer()));
        Ok(())
    });
}

/// Whether to notify of a buffer's activity changing from `prev` (held for `ran`) to `now`.
/// Called by the sidebar's tracking, which sees every buffer's state change.
pub fn activity_changed(ed: &mut Editor, id: BufferId, prev: ActivityState, now: &Activity, agent: bool, ran: Duration, visible: bool) {
    if !ed.var_bool("notify") {
        return;
    }
    let Some(meta) = ed.meta(id).cloned() else { return };
    let chat = meta.major == "chat";
    if !chat && !(meta.major == "terminal" && ed.var_bool("notify-terminals")) {
        return;
    }
    let after = Duration::from_secs_f64(ed.var("notify-after").and_then(|v| v.as_f64()).unwrap_or(15.0));
    let event = match now.state {
        ActivityState::Waiting if chat || (agent && ran >= after) => "waiting",
        ActivityState::Error if chat && prev == ActivityState::Working => "failed",
        ActivityState::Idle | ActivityState::Exited if prev == ActivityState::Working && ran >= after => "done",
        _ => return,
    };
    let wanted = ed.var("notify-events").and_then(|v| v.as_array().cloned()).unwrap_or_default();
    if !wanted.iter().any(|e| e.as_str() == Some(event)) {
        return;
    }
    let focused = ed.ctx.input(|i| i.viewport().focused).unwrap_or(true);
    let looking = match ed.var("notify-when").and_then(|v| v.as_str().map(str::to_string)).as_deref() {
        Some("always") => false,
        Some("away") => focused,
        _ => focused && visible,
    };
    if looking {
        return;
    }
    let title = ed.buffer_display_name(id);
    let took = crate::buffers::chat::fmt_duration(ran, false);
    let body = if chat {
        let reply = ed.with_buffer_as::<crate::buffers::chat::ChatBuffer, _>(id, |c, _| c.last_reply()).flatten();
        match event {
            "waiting" => format!("Waiting for you: {}", now.detail),
            "failed" => format!("Failed: {}", now.detail),
            _ => match reply {
                Some(r) => format!("Done in {took}: {}", cut(&r, 180)),
                None => format!("Done in {took}"),
            },
        }
    } else {
        match event {
            "waiting" => format!("{} waits for you", now.program.clone().unwrap_or_else(|| "The agent".into())),
            _ => "Command finished".to_string() + &format!(" in {took}"),
        }
    };
    send(ed, title, body, event, Some(id));
}

fn cut(s: &str, n: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= n { s } else { format!("{}…", s.chars().take(n).collect::<String>().trim_end()) }
}

/// Send a notification (after the `notify` hook: false drops it, {title, body} changes it).
/// A click on it brings serpis to `buffer`.
pub fn send(ed: &mut Editor, mut title: String, mut body: String, event: &str, buffer: Option<BufferId>) {
    let payload = json!({"title": title, "body": body, "event": event, "buffer": buffer.map(buffer_id_to_u64)});
    for r in ed.run_hook("notify", &payload) {
        match r {
            Value::Bool(false) => return,
            Value::Object(o) => {
                if let Some(t) = o.get("title").and_then(Value::as_str) {
                    title = t.to_string();
                }
                if let Some(b) = o.get("body").and_then(Value::as_str) {
                    body = b.to_string();
                }
            }
            _ => {}
        }
    }
    tracing::info!(target: "serpis::notify", event, "{title}: {body}");
    {
        let mut s = SENT.lock().unwrap_or_else(|e| e.into_inner());
        s.push(Sent { title: title.clone(), body: body.clone(), event: event.to_string(), buffer: buffer.map(buffer_id_to_u64) });
        if s.len() > 100 {
            s.remove(0);
        }
    }
    if ed.var("notify-backend").and_then(|v| v.as_str().map(str::to_string)).as_deref() != Some("system") {
        return;
    }
    let sound = ed.var_bool("notify-sound");
    let app = ed.var("notify-app").and_then(|v| v.as_str().map(str::to_string)).filter(|a| !a.is_empty());
    let main = ed.main_sender();
    let ctx = ed.ctx.clone();
    let target = buffer.map(buffer_id_to_u64);
    std::thread::spawn(move || {
        if deliver(&title, &body, sound, app) {
            // Clicked: back to serpis, at that buffer.
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            if let Some(b) = target {
                main.send(move |ed| bring(ed, buffer_id_from_u64(b)));
            }
        }
    });
}

/// Show `id`: in its window if one shows it, else in the focused one.
fn bring(ed: &mut Editor, id: BufferId) {
    if !ed.buffers.contains_key(id) {
        return;
    }
    match ed.layout.visible_leaves().into_iter().find(|w| ed.windows[*w].buffer == id) {
        Some(w) => ed.focus_window(w),
        None => ed.show_buffer(id),
    }
}

/// Show it, and wait for a click on it: true when clicked.
#[cfg(target_os = "macos")]
fn deliver(title: &str, body: &str, sound: bool, app: Option<String>) -> bool {
    use mac_notification_sys::{Notification, NotificationResponse};
    static APP: std::sync::Once = std::sync::Once::new();
    APP.call_once(|| {
        // The app it comes from: the one asked for, else the one serpis was started from (a
        // terminal, Serpis.app), else Terminal.
        let app = app.or_else(|| std::env::var("__CFBundleIdentifier").ok().filter(|a| !a.is_empty())).unwrap_or_else(|| "com.apple.Terminal".into());
        if let Err(e) = mac_notification_sys::set_application(&app) {
            tracing::warn!("notifications from {app}: {e}");
        }
    });
    let mut n = Notification::new();
    n.title(title).message(body).wait_for_click(true);
    if sound {
        n.default_sound();
    }
    match n.send() {
        Ok(NotificationResponse::Click | NotificationResponse::ActionButton(_)) => true,
        Ok(_) => false,
        Err(e) => {
            tracing::warn!("couldn't show a notification: {e}");
            false
        }
    }
}

/// Show it with `notify-send` (no click to wait for).
#[cfg(not(target_os = "macos"))]
fn deliver(title: &str, body: &str, _sound: bool, _app: Option<String>) -> bool {
    if let Err(e) = std::process::Command::new("notify-send").args(["-a", "serpis", title, body]).status() {
        tracing::warn!("couldn't show a notification (notify-send: {e})");
    }
    false
}
