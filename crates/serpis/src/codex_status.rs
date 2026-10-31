//! Codex account and usage status, read from `codex app-server` without starting a model turn.

use std::time::Instant;

use egui::Rect;
use serde_json::Value;
use serpis_proto::wire::{Request, Response};

use crate::buffer::{Buffer, Modeline};
use crate::buffers::text::TextBuffer;
use crate::editor::Editor;
use crate::impl_any;
use crate::text::{Pos, TextSurface};

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

/// Show fresh status for a local or remote Codex installation.
pub fn show(ed: &mut Editor, machine: Option<String>) {
    let name = match &machine {
        Some(machine) => format!("*codex-status {machine}*"),
        None => "*codex-status*".to_string(),
    };
    let asking = format!(
        "Codex status{}\n\nAsking Codex...\n",
        machine
            .as_ref()
            .map(|machine| format!(" on {machine}"))
            .unwrap_or_default()
    );
    let id = match ed.buffer_by_name(&name) {
        Some(id) => id,
        None => ed.add_buffer(
            &name,
            Box::new(StatusBuffer {
                machine: machine.clone(),
                body: TextBuffer::page(&asking, "codex-status"),
                asking: true,
                updated: None,
            }),
        ),
    };
    ed.with_buffer_as::<StatusBuffer, _>(id, |buffer, _| {
        buffer.asking = true;
        buffer.updated = None;
        buffer.replace(&asking);
    });
    ed.pop_to_buffer(id);
    fetch(ed, machine.clone(), move |ed, result| {
        ed.with_buffer_as::<StatusBuffer, _>(id, |buffer, _| {
            buffer.asking = false;
            let text = match result {
                Ok(status) => {
                    buffer.updated = Some(Instant::now());
                    Summary::of(&status).text(machine.as_deref())
                }
                Err(error) => {
                    buffer.updated = None;
                    format!("Couldn't get Codex status\n\n{error}\n\nPress g to try again, or q to close.\n")
                }
            };
            buffer.replace(&text);
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
                    label: format!("{bucket}, {duration}"),
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
            output.push_str(&format!("Authentication: {auth}\n"));
        }
        if !self.windows.is_empty() {
            output.push_str("\nRate limits\n\n");
            let now = unix_now();
            for window in &self.windows {
                let reset = window
                    .resets
                    .map(|at| format!(", resets in {}", crate::claude_usage::until(at, now)))
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
        output.push_str("\nPress g to refresh, or q to close.\n");
        output
    }
}

pub struct StatusBuffer {
    pub machine: Option<String>,
    body: TextBuffer,
    asking: bool,
    updated: Option<Instant>,
}

impl StatusBuffer {
    fn replace(&mut self, text: &str) {
        self.body.area.read_only = false;
        self.body.area.set_text(text);
        self.body.area.read_only = true;
        self.body.area.view.cursor = Pos::ZERO;
    }
}

impl Buffer for StatusBuffer {
    fn kind(&self) -> &str {
        "codex-status"
    }

    fn ui(&mut self, ui: &mut egui::Ui, rect: Rect, focused: bool, ed: &mut Editor) {
        self.body.ui(ui, rect, focused, ed);
    }

    fn surface(&mut self) -> Option<&mut dyn TextSurface> {
        self.body.surface()
    }

    fn surface_ref(&self) -> Option<&dyn TextSurface> {
        self.body.surface_ref()
    }

    fn full_text(&mut self) -> Option<String> {
        self.body.full_text()
    }

    fn modeline(&self) -> Modeline {
        let age = self.updated.map(|at| match at.elapsed().as_secs() {
            0..60 => "updated now".to_string(),
            seconds => format!("updated {} ago", crate::claude_usage::until(seconds, 0)),
        });
        Modeline {
            title: "Codex status".into(),
            status: age.into_iter().collect(),
            busy: self.asking,
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
        0..60 => format!("{minutes}-minute window"),
        minutes if minutes % 1440 == 0 => format!("{}-day window", minutes / 1440),
        minutes if minutes % 60 == 0 => format!("{}-hour window", minutes / 60),
        minutes => format!("{minutes}-minute window"),
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
        assert_eq!(summary.windows[0].label, "Codex, 5-hour window");
        assert_eq!(summary.credits.as_deref(), Some("12.5 remaining"));
        assert_eq!(summary.reset_credits, Some(2));
        assert_eq!(summary.lifetime_tokens, Some(1_234_567));
        let text = summary.text(Some("workstation"));
        assert!(text.contains("25% used, 75% left"));
        assert!(text.contains("1.2M"));
        assert!(text.contains("on workstation"));
    }
}
