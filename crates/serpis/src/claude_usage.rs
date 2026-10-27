//! Claude Code's usage, as its `/usage` shows it: the subscription's rate-limit windows (the
//! five-hour session, the week, per-model weeks), extra usage, and what the last week's
//! transcripts say spends it. Asked of the `claude` CLI on the chat's machine (the node there
//! runs it) or on this one; nothing is sent to a model.

use serde_json::Value;
use serpis_proto::wire::{Request, Response};

use crate::editor::Editor;

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

/// The usage page: `usage` as the CLI answered, `now` in Unix seconds (for "resets in").
pub fn report(usage: &Value, now: u64, machine: Option<&str>) -> String {
    let plan = usage["subscription_type"].as_str().map(|p| format!(" · {} plan", capitalized(p))).unwrap_or_default();
    let mut out = format!("Claude Code usage{plan}{}\n\n", machine.map(|m| format!(" (on {m})")).unwrap_or_default());
    let limits = &usage["rate_limits"];
    if usage["rate_limits_available"] != true || limits.is_null() {
        out.push_str("No plan limits: this claude uses an API key or a cloud provider, billed by use.\n");
    } else {
        let mut rows: Vec<(String, &Value)> = Vec::new();
        for (key, label) in [("five_hour", "Current session"), ("seven_day", "This week"), ("seven_day_opus", "This week, Opus"), ("seven_day_sonnet", "This week, Sonnet"), ("seven_day_oauth_apps", "This week, apps")] {
            if limits[key].is_object() {
                rows.push((label.into(), &limits[key]));
            }
        }
        for m in limits["model_scoped"].as_array().into_iter().flatten() {
            rows.push((format!("This week, {}", m["display_name"].as_str().unwrap_or("a model")), m));
        }
        let width = rows.iter().map(|(l, _)| l.chars().count()).max().unwrap_or(0);
        for (label, w) in &rows {
            let pct = w["utilization"].as_f64();
            let resets = w["resets_at"].as_str().and_then(unix_seconds).map(|t| format!("   resets in {}", until(t, now))).unwrap_or_default();
            let shown = pct.map(|p| format!("{p:>3.0}%")).unwrap_or_else(|| "   ?".into());
            out.push_str(&format!("{label:<width$}  {}  {shown}{resets}\n", bar(pct.unwrap_or(0.0))));
        }
        let extra = &limits["extra_usage"];
        if extra.is_object() {
            out.push('\n');
            out.push_str(&match (extra["is_enabled"].as_bool(), extra["utilization"].as_f64()) {
                (Some(true), Some(p)) => format!("Extra usage: on, {p:.0}% of its monthly limit used.\n"),
                (Some(true), None) => "Extra usage: on.\n".into(),
                _ => "Extra usage: off.\n".into(),
            });
        }
        let shares: Vec<String> = limits["seven_day_breakdown"]["rows"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|r| r["percent"].as_f64().unwrap_or(0.0) > 0.0)
            .map(|r| format!("{} {:.0}%", r["display_name"].as_str().unwrap_or("?"), r["percent"].as_f64().unwrap_or(0.0)))
            .collect();
        if !shares.is_empty() {
            out.push_str(&format!("This week went to: {}.\n", shares.join(" · ")));
        }
    }
    let week = &usage["behaviors"]["week"];
    if week.is_object() {
        out.push_str(&format!(
            "\nWhat spends it, from this machine's last 7 days ({} requests in {} sessions):\n",
            week["request_count"].as_u64().unwrap_or(0),
            week["session_count"].as_u64().unwrap_or(0)
        ));
        let tips: Vec<String> = week["recommendations"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|r| {
                let detail = r["detail"].as_str().map(|d| format!(": {d}")).unwrap_or_default();
                format!("- {} ({:.0}% of it){detail}", r["title"].as_str().unwrap_or("?"), r["pct"].as_f64().unwrap_or(0.0))
            })
            .collect();
        out.push_str(&if tips.is_empty() { "- nothing stands out".to_string() } else { tips.join("\n") });
        out.push('\n');
    }
    out.push_str("\nSettings: claude-code-usage-advice.\n");
    out
}

fn capitalized(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// `pct` (0-100) as a 20-cell bar.
fn bar(pct: f64) -> String {
    let full = ((pct.clamp(0.0, 100.0) / 5.0).round()) as usize;
    format!("{}{}", "█".repeat(full), "░".repeat(20 - full))
}

/// `3d 4h`, `3h 12m`, `12m`: from `now` to `t`.
fn until(t: u64, now: u64) -> String {
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
    let tz = s[19..].trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
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
    fn the_page_shows_each_window_and_the_advice() {
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
        let page = report(&usage, 0, Some("box"));
        assert!(page.starts_with("Claude Code usage · Pro plan (on box)\n"), "{page}");
        assert!(page.contains("Current session   ████████░░░░░░░░░░░░   42%   resets in 3h 12m\n"), "{page}");
        assert!(page.contains("This week         █░░░░░░░░░░░░░░░░░░░    7%   resets in 2d 0h\n"), "{page}");
        assert!(page.contains("This week, Fable  ████") && page.contains("61%\n"), "{page}");
        assert!(!page.contains("Opus"), "{page}");
        assert!(page.contains("Extra usage: off.\nThis week went to: Claude Code 100%.\n"), "{page}");
        assert!(page.contains("(80 requests in 5 sessions):\n- Start fresh when you switch tasks (90% of it): run /clear\n"), "{page}");
        let api = report(&json!({"subscription_type": null, "rate_limits_available": false, "rate_limits": null, "behaviors": null}), 0, None);
        assert!(api.starts_with("Claude Code usage\n\nNo plan limits"), "{api}");
    }
}
