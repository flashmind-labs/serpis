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
use crate::editor::{Editor, buffer_id_from_u64, buffer_id_to_u64};

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
    ed.defvar("notify", json!(true), "Send desktop notifications when a chat's turn ends, an agent waits for you or fails, or a terminal's command finishes, while you aren't looking (notify-when). Which: notify-events.");
    ed.defvar("notify-events", json!(["done", "waiting", "failed"]), "What notifies: \"done\" (a chat's turn ended, a terminal's command finished: after notify-after seconds), \"waiting\" (an agent waits for you: a permission, its questions, a plan), \"failed\" (a run ended in an error).");
    ed.defvar("notify-after", json!(15), "Seconds a turn or a terminal's command must have run for its end to notify (\"done\"); what waits for you or failed notifies at once.");
    ed.defvar("notify-when", json!("always"), "When to notify: \"always\", \"unseen\" (serpis isn't the focused app or its window is hidden, or the buffer isn't on screen) or \"away\" (serpis isn't the focused app, or its window is hidden). Some window managers (AeroSpace) hide windows without the app being told it lost focus, so the last two can miss.");
    ed.defvar("notify-terminals", json!(true), "Also notify when a terminal's command finishes (after notify-after seconds), and when an agent run in one waits.");
    ed.defvar("notify-sound", json!(true), "Notifications play the system's sound.");
    ed.defvar("notify-app", json!(""), "macOS: the app notifications come from (its icon, and where System Settings > Notifications allows them), a bundle id. \"\": Serpis.app when it is installed (a click brings serpis back), else they are shown through AppleScript (as Script Editor's, and a click doesn't). An app that sends its own notifications can't be named: macOS refuses them.");
    ed.defvar("notify-backend", json!(if std::env::var_os("SERPIS_TEST_SCRIPT").is_some() { "none" } else { "system" }), "\"system\" shows notifications on the desktop; \"none\" only logs them (and lists them for serpis.debug.notifications), as tests do.");
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
        tracing::debug!(target: "serpis::notify", "not notifying \"{event}\": not in notify-events");
        return;
    }
    // A hidden window (covered, minimized, on another space) isn't looked at, whatever focus it
    // last had (while hidden, its input is the last shown frame's).
    let focused = ed.ctx.input(|i| {
        let v = i.viewport();
        v.focused.unwrap_or(true) && v.occluded != Some(true) && v.minimized != Some(true)
    });
    let looking = match ed.var("notify-when").and_then(|v| v.as_str().map(str::to_string)).as_deref() {
        Some("always") => false,
        Some("away") => focused,
        _ => focused && visible,
    };
    if looking {
        tracing::debug!(target: "serpis::notify", "not notifying \"{event}\": you are looking (notify-when; focused {focused}, on screen {visible})");
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
///
/// macOS shows a process's notifications as some app's: serpis's own when Serpis.app is
/// installed. Not the terminal it was started from: such an app sends its own notifications
/// (the newer API), and macOS refuses the older one this uses under the same app ("can't mix
/// modern clients with legacy clients"), silently. With no app to send as, AppleScript shows it.
#[cfg(target_os = "macos")]
fn deliver(title: &str, body: &str, sound: bool, app: Option<String>) -> bool {
    use mac_notification_sys::{Notification, NotificationResponse};
    static APP: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    let app = APP.get_or_init(|| {
        let app = app.or_else(|| mac_notification_sys::get_bundle_identifier("Serpis"))?;
        match mac_notification_sys::set_application(&app) {
            Ok(()) => Some(app),
            Err(e) => {
                tracing::warn!("notifications as {app}: {e}");
                None
            }
        }
    });
    if app.is_none() {
        return applescript(title, body, sound);
    }
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

/// Show it with AppleScript's `display notification` (as Script Editor's; no click to wait for).
#[cfg(target_os = "macos")]
fn applescript(title: &str, body: &str, sound: bool) -> bool {
    let q = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let mut script = format!("display notification {} with title {}", q(body), q(title));
    if sound {
        script.push_str(" sound name \"default\"");
    }
    if let Err(e) = std::process::Command::new("osascript").args(["-e", &script]).status() {
        tracing::warn!("couldn't show a notification (osascript: {e})");
    }
    false
}

/// Show it with `notify-send` (no click to wait for).
#[cfg(not(target_os = "macos"))]
fn deliver(title: &str, body: &str, _sound: bool, _app: Option<String>) -> bool {
    if let Err(e) = std::process::Command::new("notify-send").args(["-a", "serpis", title, body]).status() {
        tracing::warn!("couldn't show a notification (notify-send: {e})");
    }
    false
}
