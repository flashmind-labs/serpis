//! Frecency: how often and how recently something was picked in a prompt (M-x commands,
//! buffers, ...), so the usual choices come first. Each pick adds 1 to a score that halves every
//! `minibuffer-frecency-half-life` days. Kept per category in `state.json`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde_json::{Value, json};

use crate::editor::Editor;

/// category -> key -> (score when last picked, when, in seconds since the epoch).
type Store = HashMap<String, HashMap<String, (f64, u64)>>;

/// Keys kept per category (the lowest scores go first).
const MAX_KEYS: usize = 400;

fn store() -> &'static Mutex<Store> {
    static S: OnceLock<Mutex<Store>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Editor::persisted_state("frecency").and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()))
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn decayed((score, at): (f64, u64), now: u64, half_life_days: f64) -> f64 {
    let age_days = now.saturating_sub(at) as f64 / 86_400.0;
    score * 0.5f64.powf(age_days / half_life_days.max(0.01))
}

fn half_life(ed: &Editor) -> f64 {
    ed.var("minibuffer-frecency-half-life").and_then(Value::as_f64).unwrap_or(7.0)
}

/// Current scores of `keys` in `category` (0 for unknown ones).
pub fn scores(ed: &Editor, category: &str, keys: &[&str]) -> Vec<f64> {
    let s = store().lock().unwrap();
    let (t, hl) = (now(), half_life(ed));
    let cat = s.get(category);
    keys.iter().map(|k| cat.and_then(|c| c.get(*k)).map_or(0.0, |e| decayed(*e, t, hl))).collect()
}

/// Record that `key` was picked in `category`.
pub fn bump(ed: &Editor, category: &str, key: &str) {
    let mut s = store().lock().unwrap();
    let (t, hl) = (now(), half_life(ed));
    let cat = s.entry(category.to_string()).or_default();
    let score = cat.get(key).map_or(0.0, |e| decayed(*e, t, hl)) + 1.0;
    cat.insert(key.to_string(), (score, t));
    if cat.len() > MAX_KEYS {
        let mut all: Vec<(String, f64)> = cat.iter().map(|(k, e)| (k.clone(), decayed(*e, t, hl))).collect();
        all.sort_by(|a, b| b.1.total_cmp(&a.1));
        let keep: std::collections::HashSet<String> = all.into_iter().take(MAX_KEYS).map(|(k, _)| k).collect();
        cat.retain(|k, _| keep.contains(k));
    }
    let snapshot = json!(&*s);
    drop(s);
    ed.persist_state("frecency", snapshot);
}

/// The ranking bonus for a score: grows slowly, so a much better fuzzy match still wins
/// (one pick ≈ 17, ten ≈ 60, a hundred ≈ 115; a strong match scores 100-300).
pub fn boost(score: f64) -> i64 {
    (25.0 * (1.0 + score).ln()).round() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scores_decay_and_boost_slowly() {
        assert_eq!(decayed((4.0, 0), 7 * 86_400, 7.0), 2.0);
        assert_eq!(decayed((4.0, 100), 100, 7.0), 4.0);
        assert_eq!(boost(0.0), 0);
        assert!(boost(1.0) > 10 && boost(100.0) < 130 && boost(10.0) > boost(1.0));
    }
}
