//! This process's log, kept to be read back (serpis's `*Log*` page, a node's `Logs` request):
//! a [`tracing`] layer that keeps the last records in memory, beside the one printing them to
//! stderr. serpis's own crates (and `lua`, `message`) are kept from [`set_level`] up (info by
//! default); anything else from warn up.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};

use serpis_proto::wire::LogRecord;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Metadata, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;

/// Records kept, at most (older ones are dropped).
pub const KEEP: usize = 10_000;

struct Store {
    records: VecDeque<LogRecord>,
    seq: u64,
}

fn store() -> &'static Mutex<Store> {
    static S: OnceLock<Mutex<Store>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Store { records: VecDeque::new(), seq: 0 }))
}

/// The least severe level kept from serpis's own code: 1 error … 5 trace.
static LEVEL: AtomicU8 = AtomicU8::new(3);

fn rank(l: &Level) -> u8 {
    match *l {
        Level::ERROR => 1,
        Level::WARN => 2,
        Level::INFO => 3,
        Level::DEBUG => 4,
        Level::TRACE => 5,
    }
}

/// Keep serpis's own records from `level` up (`error`, `warn`, `info`, `debug`, `trace`).
/// False for anything else.
pub fn set_level(level: &str) -> bool {
    let r = match level {
        "error" => 1,
        "warn" => 2,
        "info" => 3,
        "debug" => 4,
        "trace" => 5,
        _ => return false,
    };
    LEVEL.store(r, Ordering::Relaxed);
    true
}

pub fn level() -> &'static str {
    ["error", "warn", "info", "debug", "trace"][(LEVEL.load(Ordering::Relaxed).clamp(1, 5) - 1) as usize]
}

/// serpis's own code: its crates, and what scripts and messages log.
pub fn own(target: &str) -> bool {
    target.starts_with("serpis") || target == "lua" || target.starts_with("lua:") || target == "message"
}

fn wanted(m: &Metadata) -> bool {
    let r = rank(m.level());
    if own(m.target()) { r <= LEVEL.load(Ordering::Relaxed) } else { r <= 2 }
}

/// Keep a record (what tracing logs, and what is logged around it: a script's `serpis.log`).
pub fn push(level: &str, target: &str, message: String, fields: Vec<(String, String)>) {
    let time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    let mut s = store().lock().unwrap_or_else(|e| e.into_inner());
    s.seq += 1;
    let seq = s.seq;
    s.records.push_back(LogRecord { seq, time, level: level.to_string(), target: target.to_string(), message, fields });
    while s.records.len() > KEEP {
        s.records.pop_front();
    }
}

/// The records after `after` (by `seq`), oldest first, and how many after it were dropped.
pub fn since(after: u64) -> (Vec<LogRecord>, u64) {
    let s = store().lock().unwrap_or_else(|e| e.into_inner());
    let first = s.records.front().map(|r| r.seq).unwrap_or(s.seq + 1);
    let dropped = first.saturating_sub(after + 1);
    (s.records.iter().filter(|r| r.seq > after).cloned().collect(), dropped)
}

/// The last record's `seq` (0: none yet).
pub fn last() -> u64 {
    store().lock().unwrap_or_else(|e| e.into_inner()).seq
}

/// Forget what was kept (new records go on numbering after it).
pub fn clear() {
    store().lock().unwrap_or_else(|e| e.into_inner()).records.clear();
}

struct Fields {
    message: String,
    fields: Vec<(String, String)>,
}

impl Visit for Fields {
    fn record_str(&mut self, f: &Field, v: &str) {
        if f.name() == "message" {
            self.message.push_str(v);
        } else {
            self.fields.push((f.name().to_string(), v.to_string()));
        }
    }

    fn record_debug(&mut self, f: &Field, v: &dyn std::fmt::Debug) {
        if f.name() == "message" {
            let _ = write!(self.message, "{v:?}");
        } else {
            self.fields.push((f.name().to_string(), format!("{v:?}")));
        }
    }
}

/// The layer that keeps records.
pub struct Keep;

impl<S: Subscriber> Layer<S> for Keep {
    fn on_event(&self, e: &Event<'_>, _: Context<'_, S>) {
        let mut f = Fields { message: String::new(), fields: Vec::new() };
        e.record(&mut f);
        // A script's fields come as one, `data` (JSON): kept apart again.
        if e.metadata().target() == "lua"
            && let Some(i) = f.fields.iter().position(|(k, _)| k == "data")
            && let Ok(serde_json::Value::Object(o)) = serde_json::from_str(&f.fields[i].1)
        {
            f.fields.remove(i);
            f.fields.extend(o.into_iter().map(|(k, v)| (k, v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()))));
        }
        push(&e.metadata().level().as_str().to_ascii_lowercase(), e.metadata().target(), f.message, f.fields);
    }
}

/// Log to stderr as `filter` (RUST_LOG, else `default`) says, and keep records to read back.
pub fn init(default: &str) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| default.into());
    let _ = tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr).with_filter(filter))
        // What is kept changes as it runs (set_level): records of any level are looked at, and
        // asked about each time (a plain filter_fn caches its first answer per callsite).
        .with(Keep.with_filter(tracing_subscriber::filter::dynamic_filter_fn(|m, _| wanted(m)).with_max_level_hint(tracing::level_filters::LevelFilter::TRACE)))
        .try_init();
}

/// `serpis.log`, the same in the window and a node: `error`, `warn`, `info`, `debug` and
/// `trace` (text, fields?) log a record as `lua` (fields: a table, shown beside it);
/// `level(level?)` reads or sets what is kept; `records(after?)` reads them back.
pub fn lua_api(lua: &mlua::Lua) -> mlua::Result<mlua::Table> {
    use mlua::LuaSerdeExt;
    let t = lua.create_table()?;
    for name in ["error", "warn", "info", "debug", "trace"] {
        t.set(name, lua.create_function(move |lua, (text, fields): (String, Option<mlua::Value>)| {
            let data = match fields {
                Some(v @ mlua::Value::Table(_)) => Some(serde_json::Value::to_string(&lua.from_value(v)?)),
                _ => None,
            };
            let data = data.as_deref();
            macro_rules! log {
                ($l:expr) => {
                    match data {
                        Some(d) => tracing::event!(target: "lua", $l, data = d, "{text}"),
                        None => tracing::event!(target: "lua", $l, "{text}"),
                    }
                };
            }
            match name {
                "error" => log!(Level::ERROR),
                "warn" => log!(Level::WARN),
                "info" => log!(Level::INFO),
                "debug" => log!(Level::DEBUG),
                _ => log!(Level::TRACE),
            }
            Ok(())
        })?)?;
    }
    t.set("level", lua.create_function(|_, l: Option<String>| {
        if let Some(l) = l
            && !set_level(&l)
        {
            return Err(mlua::Error::runtime(format!("{l} isn't a log level (error, warn, info, debug, trace)")));
        }
        Ok(level())
    })?)?;
    t.set("records", lua.create_function(|lua, after: Option<u64>| lua.to_value(&since(after.unwrap_or(0)).0))?)?;
    Ok(t)
}
