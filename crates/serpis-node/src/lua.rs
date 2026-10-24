//! The node's Lua: the same `init.lua` and extensions the window runs, without a window.
//!
//! A chat's side of the config takes effect here, where the chat runs: its hooks
//! (`before-prompt`, `before-tool-call`, `assistant-message`, ...), the agent tools Lua defines,
//! settings set with `serpis.var.set`, and `serpis.chat` on the node's chats (by chat id, which
//! is what hook payloads' `buffer` holds here). What only a window has (commands, keymaps,
//! buffers, fonts, the UI) does nothing here, so one config serves both; `serpis.where` says
//! which one is running it ("node" here, "window" there).
//!
//! Lua runs on the node's thread only. Its functions read a snapshot of the chats taken before
//! it runs, and what they ask of a chat is done once the call returns (the chat may be the one
//! running the hook), except an abort, which takes effect at once.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::FutureExt;
use mlua::{Function, Lua, LuaSerdeExt, Table, Value as LuaValue};
use serde_json::{Value, json};
use serpis_chat::{Chat, Level};

/// What Lua asked of the node, done when the Lua call that asked returns.
pub enum Action {
    Prompt { chat: String, text: String },
    SetModel { chat: String, spec: String },
    SetThinking { chat: String, level: String },
    Append { chat: String, kind: String, data: Value },
    ReloadModels,
    Reload,
}

/// A chat as Lua sees it.
#[derive(Clone)]
pub struct Info {
    pub json: Value,
    pub usage: Value,
    pub cache: Value,
    pub running: bool,
    pub entries: Vec<(String, Value)>,
    pub agent: Option<serpis_agent::AgentHandle>,
}

impl Info {
    pub fn of(id: &str, c: &Chat) -> Info {
        let model = c.model.as_ref().map(|m| format!("{}/{}", m.provider, m.id));
        let (used, window) = c.context();
        let usage = usage_json(&c.totals);
        Info {
            json: json!({
                "buffer": id,
                "chat": id,
                "model": model,
                "thinking": c.thinking.as_str(),
                "running": c.running(),
                "compacting": c.compacting(),
                "usage": usage,
                "title": c.title,
                "cwd": c.cwd.to_string_lossy(),
                "session": c.session.as_ref().filter(|w| w.exists()).map(|w| w.path.to_string_lossy().into_owned()),
                "session_id": c.session_id,
                "machine": Value::Null,
                "context": {
                    "used": used.unwrap_or(0),
                    "window": window,
                    "percent": if window > 0 { used.unwrap_or(0) as f64 * 100.0 / window as f64 } else { 0.0 },
                },
            }),
            usage,
            cache: c.cache_json(),
            running: c.running(),
            entries: c.custom_entries.clone(),
            agent: c.agent().cloned(),
        }
    }
}

pub fn usage_json(u: &serpis_ai::Usage) -> Value {
    json!({"input": u.input, "output": u.output, "cache_read": u.cache_read, "cache_write": u.cache_write, "total_tokens": u.total_tokens, "cost": u.cost.total})
}

/// A tool Lua defined (`serpis.agent.tool`).
#[derive(Clone)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// What Lua's functions read and change.
pub struct State {
    /// Settings: from settings.json, from Lua (`serpis.var.set`), and from this machine's
    /// window while it runs; a later layer wins.
    pub file: serde_json::Map<String, Value>,
    pub lua_vars: serde_json::Map<String, Value>,
    pub pushed: serde_json::Map<String, Value>,
    pub chats: BTreeMap<String, Info>,
    pub actions: Vec<Action>,
    /// Messages for clients: the chat whose clients see it (None: every client), and the text.
    pub notices: Vec<(Option<String>, Level, String)>,
    /// The chat whose hook or tool is running.
    pub current: Option<String>,
    pub registry: serpis_ai::Registry,
    hooks: HashMap<String, Vec<Function>>,
    tools: Vec<ToolSpec>,
    timers: Vec<(Instant, Function)>,
}

impl State {
    pub fn var(&self, name: &str) -> Option<Value> {
        self.pushed.get(name).or_else(|| self.lua_vars.get(name)).or_else(|| self.file.get(name)).cloned()
    }

    fn say(&mut self, level: Level, text: String) {
        self.notices.push((self.current.clone(), level, text));
    }
}

pub struct NodeLua {
    lua: Lua,
    /// Earlier states stay alive: a timer or tool call of theirs may still be on its way.
    retired: Vec<Lua>,
    pub state: Rc<RefCell<State>>,
    /// What went wrong loading the config, for clients to show.
    pub errors: Vec<String>,
}

impl NodeLua {
    /// The config in `dir` loaded, with settings.json's `file` settings.
    pub fn load(dir: &Path, registry: serpis_ai::Registry, file: serde_json::Map<String, Value>) -> NodeLua {
        let state = State {
            file,
            lua_vars: Default::default(),
            pushed: Default::default(),
            chats: BTreeMap::new(),
            actions: Vec::new(),
            notices: Vec::new(),
            current: None,
            registry,
            hooks: HashMap::new(),
            tools: Vec::new(),
            timers: Vec::new(),
        };
        let mut l = NodeLua { lua: Lua::new(), retired: Vec::new(), state: Rc::new(RefCell::new(state)), errors: Vec::new() };
        l.reload(dir);
        l
    }

    /// Run the config again in a fresh state: what the old one registered is gone first.
    pub fn reload(&mut self, dir: &Path) {
        {
            let mut s = self.state.borrow_mut();
            s.lua_vars.clear();
            s.hooks.clear();
            s.tools.clear();
            s.timers.clear();
        }
        let lua = Lua::new();
        self.retired.push(std::mem::replace(&mut self.lua, lua.clone()));
        self.errors.clear();
        let setup = || -> mlua::Result<()> {
            let package: Table = lua.globals().get("package")?;
            let path: String = package.get("path")?;
            package.set("path", serpis_chat::config::package_path(dir, &path))?;
            lua.globals().set("serpis", api(&lua, self.state.clone(), dir)?)?;
            Ok(())
        };
        if let Err(e) = setup() {
            self.errors.push(format!("lua setup: {e}"));
            return;
        }
        let mut files = Vec::new();
        let init = dir.join("init.lua");
        if init.exists() {
            files.push(("init.lua".to_string(), init));
        }
        let disabled: Vec<String> = self.var("extensions-disabled").and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
        files.extend(serpis_chat::config::extensions(dir).into_iter().filter(|(n, _)| !disabled.contains(n)).map(|(n, p)| (format!("extension {n}"), p)));
        // Each file on its own: one broken file never stops the rest.
        for (what, path) in files {
            let r = std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|code| lua.load(&code).set_name(path.to_string_lossy()).exec().map_err(|e| e.to_string()));
            if let Err(e) = r {
                tracing::warn!("{what}: {e}");
                self.errors.push(format!("{what}: {e}"));
            }
        }
        self.run_hook("config-loaded", &Value::Null);
    }

    pub fn var(&self, name: &str) -> Option<Value> {
        self.state.borrow().var(name)
    }

    /// Run hook `name`'s handlers (as `chat`'s, when it is one of a chat): their answers that
    /// aren't nil, in order.
    pub fn run_hook_for(&self, chat: Option<&str>, name: &str, payload: &Value) -> Vec<Value> {
        let handlers = self.state.borrow().hooks.get(name).cloned().unwrap_or_default();
        if handlers.is_empty() {
            return Vec::new();
        }
        let prev = std::mem::replace(&mut self.state.borrow_mut().current, chat.map(str::to_string));
        let out = handlers.iter().filter_map(|f| self.call(f, payload)).collect();
        self.state.borrow_mut().current = prev;
        out
    }

    pub fn run_hook(&self, name: &str, payload: &Value) -> Vec<Value> {
        self.run_hook_for(None, name, payload)
    }

    pub fn has_hook(&self, name: &str) -> bool {
        self.state.borrow().hooks.get(name).is_some_and(|h| !h.is_empty())
    }

    /// Call `f` with `arg`; its answer (None for nil, or when it failed: the error is told).
    fn call(&self, f: &Function, arg: &Value) -> Option<Value> {
        let r = (|| -> mlua::Result<Value> {
            let r: LuaValue = f.call(lua_of(&self.lua, arg)?)?;
            Ok(if r.is_nil() { Value::Null } else { json_of(&self.lua, r)? })
        })();
        match r {
            Ok(v) => (!v.is_null()).then_some(v),
            Err(e) => {
                self.state.borrow_mut().say(Level::Error, format!("lua: {e}"));
                None
            }
        }
    }

    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        self.state.borrow().tools.clone()
    }

    /// Run Lua tool `name` for `chat`: its answer as a tool result.
    pub fn call_tool(&self, chat: &str, name: &str, args: &Value) -> serpis_agent::ToolResult {
        let f = self.lua.named_registry_value::<Table>("serpis_tools").ok().and_then(|t| t.get::<Function>(name).ok());
        let Some(f) = f else {
            let mut r = serpis_agent::ToolResult::text(format!("Lua tool {name} is no longer defined"));
            r.is_error = true;
            return r;
        };
        let prev = std::mem::replace(&mut self.state.borrow_mut().current, Some(chat.to_string()));
        let r = self.call(&f, args);
        self.state.borrow_mut().current = prev;
        tool_result(r)
    }

    /// Run the timers that are due; when the next one is.
    pub fn run_timers(&self) -> Option<Instant> {
        let now = Instant::now();
        let due: Vec<Function> = {
            let mut s = self.state.borrow_mut();
            let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut s.timers).into_iter().partition(|(t, _)| *t <= now);
            s.timers = later;
            due.into_iter().map(|(_, f)| f).collect()
        };
        for f in due {
            self.call(&f, &Value::Null);
        }
        self.state.borrow().timers.iter().map(|(t, _)| *t).min()
    }

    /// Forget what retired states were kept for, once nothing of theirs can be running.
    pub fn trim(&mut self) {
        self.retired.clear();
    }
}

/// A tool's answer: a string, or {text, is_error}.
pub fn tool_result(r: Option<Value>) -> serpis_agent::ToolResult {
    match r {
        Some(Value::String(s)) => serpis_agent::ToolResult::text(s),
        Some(Value::Object(o)) => {
            let mut r = serpis_agent::ToolResult::text(o.get("text").and_then(Value::as_str).unwrap_or(""));
            r.is_error = o.get("is_error").and_then(Value::as_bool).unwrap_or(false);
            r
        }
        Some(other) => serpis_agent::ToolResult::text(other.to_string()),
        None => serpis_agent::ToolResult::text("(no output)"),
    }
}

/// Runs a closure with the node's Lua, on the node's thread (tools are called from the agent's).
pub type Post = Arc<dyn Fn(Box<dyn FnOnce(&NodeLua) + Send>) + Send + Sync>;

/// A tool Lua defined, as the agent gets it: run on the node's thread.
pub struct LuaTool {
    pub spec: ToolSpec,
    pub chat: String,
    pub post: Post,
}

impl serpis_agent::AgentTool for LuaTool {
    fn name(&self) -> &str {
        &self.spec.name
    }
    fn description(&self) -> String {
        self.spec.description.clone()
    }
    fn parameters(&self) -> Value {
        self.spec.parameters.clone()
    }
    fn prompt_snippet(&self) -> Option<String> {
        Some(self.spec.description.lines().next().unwrap_or("").to_string())
    }
    fn execute(&self, _id: String, args: Value, _ctx: serpis_agent::ToolContext) -> futures::future::BoxFuture<'static, anyhow::Result<serpis_agent::ToolResult>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let (name, chat) = (self.spec.name.clone(), self.chat.clone());
        (self.post)(Box::new(move |l| {
            let _ = tx.send(l.call_tool(&chat, &name, &args));
        }));
        async move { rx.await.map_err(|_| anyhow::anyhow!("the node stopped")) }.boxed()
    }
}

/// JSON → Lua, with null as nil (`if info.session then` must not see a truthy null).
pub fn lua_of<T: serde::Serialize + ?Sized>(lua: &Lua, v: &T) -> mlua::Result<LuaValue> {
    lua.to_value_with(v, mlua::SerializeOptions::new().serialize_none_to_null(false).serialize_unit_to_null(false))
}

/// Lua → JSON, whole numbers as integers (LuaJIT's numbers are all floats).
pub fn json_of(lua: &Lua, v: LuaValue) -> mlua::Result<Value> {
    fn integral(v: Value) -> Value {
        match v {
            Value::Number(n) => match n.as_f64() {
                Some(f) if f.fract() == 0.0 && f.abs() < 9.0e15 && !n.is_i64() && !n.is_u64() => {
                    if f >= 0.0 { json!(f as u64) } else { json!(f as i64) }
                }
                _ => Value::Number(n),
            },
            Value::Array(a) => Value::Array(a.into_iter().map(integral).collect()),
            Value::Object(o) => Value::Object(o.into_iter().map(|(k, v)| (k, integral(v))).collect()),
            other => other,
        }
    }
    Ok(integral(lua.from_value::<Value>(v)?))
}

/// A table whose missing members do nothing: what the window's API has and the node's doesn't
/// (`serpis.keymap.set(...)`, `serpis.ui.header(...)`) is accepted and ignored.
fn inert(lua: &Lua, t: &Table) -> mlua::Result<()> {
    let mt = lua.create_table()?;
    mt.set("__index", lua.create_function(|lua, (_, _): (LuaValue, LuaValue)| nothing(lua))?)?;
    t.set_metatable(Some(mt))?;
    Ok(())
}

/// Something that can be called or indexed, any depth, and answers nil when called.
fn nothing(lua: &Lua) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    let mt = lua.create_table()?;
    mt.set("__index", lua.create_function(|lua, (_, _): (LuaValue, LuaValue)| nothing(lua))?)?;
    mt.set("__call", lua.create_function(|_, _: mlua::MultiValue| Ok(()))?)?;
    t.set_metatable(Some(mt))?;
    Ok(t)
}

fn api(lua: &Lua, st: Rc<RefCell<State>>, dir: &Path) -> mlua::Result<Table> {
    let s = lua.create_table()?;
    s.set("version", crate::VERSION)?;
    s.set("where", "node")?;
    s.set("config_dir", dir.to_string_lossy().into_owned())?;
    for (name, level) in [("message", Level::Info), ("warn", Level::Warn), ("error", Level::Error)] {
        let st = st.clone();
        s.set(name, lua.create_function(move |_, text: String| {
            st.borrow_mut().say(level, text);
            Ok(())
        })?)?;
    }
    let st2 = st.clone();
    s.set("reload", lua.create_function(move |_, ()| {
        st2.borrow_mut().actions.push(Action::Reload);
        Ok(())
    })?)?;
    let st2 = st.clone();
    s.set("defer", lua.create_function(move |_, (ms, f): (f64, Function)| {
        st2.borrow_mut().timers.push((Instant::now() + Duration::from_millis(ms.max(0.0) as u64), f));
        Ok(())
    })?)?;
    let st2 = st.clone();
    s.set("schedule", lua.create_function(move |_, f: Function| {
        st2.borrow_mut().timers.push((Instant::now(), f));
        Ok(())
    })?)?;

    // Hooks.
    let hook = lua.create_table()?;
    let st2 = st.clone();
    hook.set("add", lua.create_function(move |_, (name, f): (String, Function)| {
        st2.borrow_mut().hooks.entry(name).or_default().push(f);
        Ok(())
    })?)?;
    let st2 = st.clone();
    hook.set("run", lua.create_function(move |lua, (name, payload): (String, Option<LuaValue>)| {
        let p = payload.map(|v| json_of(lua, v)).transpose()?.unwrap_or(Value::Null);
        let handlers = st2.borrow().hooks.get(&name).cloned().unwrap_or_default();
        let out = lua.create_table()?;
        for f in handlers {
            let r: LuaValue = f.call(lua_of(lua, &p)?)?;
            if !r.is_nil() {
                out.push(r)?;
            }
        }
        Ok(out)
    })?)?;
    inert(lua, &hook)?;
    s.set("hook", hook)?;

    // Settings.
    let var = lua.create_table()?;
    let st2 = st.clone();
    var.set("get", lua.create_function(move |lua, name: String| {
        let v = st2.borrow().var(&name);
        v.map(|v| lua_of(lua, &v)).transpose().map(|v| v.unwrap_or(LuaValue::Nil))
    })?)?;
    let st2 = st.clone();
    var.set("define", lua.create_function(move |lua, (name, default, _doc): (String, LuaValue, Option<String>)| {
        // A value already set (settings.json, the window, the config) stays.
        let mut s = st2.borrow_mut();
        if s.var(&name).is_none() {
            let v = json_of(lua, default)?;
            s.lua_vars.insert(name, v);
        }
        Ok(())
    })?)?;
    let st2 = st.clone();
    var.set("set", lua.create_function(move |lua, (name, v): (String, LuaValue)| {
        let v = json_of(lua, v)?;
        st2.borrow_mut().lua_vars.insert(name, v);
        Ok(())
    })?)?;
    inert(lua, &var)?;
    s.set("var", var)?;

    // The agent.
    let agent = lua.create_table()?;
    let st2 = st.clone();
    agent.set("tool", lua.create_function(move |lua, spec: Table| {
        let name: String = spec.get("name")?;
        let description: String = spec.get("description").unwrap_or_default();
        let params: LuaValue = spec.get("parameters")?;
        let parameters = if params.is_nil() { json!({"type": "object", "properties": {}}) } else { json_of(lua, params)? };
        let execute: Function = spec.get("execute")?;
        let tools: Table = match lua.named_registry_value::<Table>("serpis_tools") {
            Ok(t) => t,
            Err(_) => {
                let t = lua.create_table()?;
                lua.set_named_registry_value("serpis_tools", &t)?;
                t
            }
        };
        tools.set(name.as_str(), execute)?;
        let mut s = st2.borrow_mut();
        s.tools.retain(|t| t.name != name);
        s.tools.push(ToolSpec { name, description, parameters });
        Ok(())
    })?)?;
    let st2 = st.clone();
    agent.set("model", lua.create_function(move |_, spec: String| {
        st2.borrow_mut().lua_vars.insert("agent-model".into(), json!(spec));
        Ok(())
    })?)?;
    let st2 = st.clone();
    agent.set("reload_models", lua.create_function(move |_, ()| {
        st2.borrow().registry.reload();
        Ok(())
    })?)?;
    let st2 = st.clone();
    agent.set("models", lua.create_function(move |lua, ()| {
        let s = st2.borrow();
        let configured = s.registry.configured_providers();
        let v: Vec<Value> = s.registry.models().iter().map(|m| model_json(m, configured.contains(&m.provider))).collect();
        lua_of(lua, &v)
    })?)?;
    let st2 = st.clone();
    agent.set("find_model", lua.create_function(move |lua, spec: String| {
        let s = st2.borrow();
        let m = spec.split_once('/').and_then(|(p, id)| s.registry.find(p, id));
        match m {
            Some(m) => lua_of(lua, &model_json(&m, s.registry.configured_providers().contains(&m.provider))),
            None => Ok(LuaValue::Nil),
        }
    })?)?;
    inert(lua, &agent)?;
    s.set("agent", agent)?;

    // Chats, by id (nil: the chat whose hook or tool is running).
    let chat = lua.create_table()?;
    let id_of = {
        let st = st.clone();
        move |id: Option<String>| -> mlua::Result<String> { id.or_else(|| st.borrow().current.clone()).ok_or_else(|| mlua::Error::runtime("which chat? (no chat is running this)")) }
    };
    let read = {
        let st = st.clone();
        move |id: &str, f: &dyn Fn(&Info) -> Value| -> Value { st.borrow().chats.get(id).map(f).unwrap_or(Value::Null) }
    };
    let st2 = st.clone();
    chat.set("list", lua.create_function(move |lua, ()| {
        let v: Vec<Value> = st2.borrow().chats.values().map(|i| i.json.clone()).collect();
        lua_of(lua, &v)
    })?)?;
    macro_rules! reader {
        ($name:literal, $f:expr) => {{
            let (id_of, read) = (id_of.clone(), read.clone());
            chat.set($name, lua.create_function(move |lua, id: Option<String>| {
                let id = id_of(id)?;
                lua_of(lua, &read(&id, &$f))
            })?)?;
        }};
    }
    reader!("info", |i: &Info| i.json.clone());
    reader!("usage", |i: &Info| i.usage.clone());
    reader!("cache", |i: &Info| i.cache.clone());
    reader!("running", |i: &Info| json!(i.running));
    let (id_of2, st2) = (id_of.clone(), st.clone());
    chat.set("entries", lua.create_function(move |lua, (id, kind): (Option<String>, Option<String>)| {
        let id = id_of2(id)?;
        let v: Vec<Value> = st2.borrow().chats.get(&id).map(|i| i.entries.iter().filter(|(k, _)| kind.as_ref().is_none_or(|w| w == k)).map(|(_, d)| d.clone()).collect()).unwrap_or_default();
        lua_of(lua, &v)
    })?)?;
    let (id_of2, st2) = (id_of.clone(), st.clone());
    chat.set("abort", lua.create_function(move |_, id: Option<String>| {
        let id = id_of2(id)?;
        if let Some(a) = st2.borrow().chats.get(&id).and_then(|i| i.agent.as_ref()) {
            a.abort();
        }
        Ok(())
    })?)?;
    let (id_of2, st2) = (id_of.clone(), st.clone());
    chat.set("append", lua.create_function(move |lua, (id, kind, data): (Option<String>, String, LuaValue)| {
        let id = id_of2(id)?;
        let data = json_of(lua, data)?;
        let mut s = st2.borrow_mut();
        // Seen at once by whatever reads the entries next.
        if let Some(i) = s.chats.get_mut(&id) {
            i.entries.push((kind.clone(), data.clone()));
        }
        s.actions.push(Action::Append { chat: id, kind, data });
        Ok(())
    })?)?;
    let (id_of2, st2) = (id_of.clone(), st.clone());
    chat.set("set_model", lua.create_function(move |_, (id, spec): (Option<String>, String)| {
        let id = id_of2(id)?;
        let mut s = st2.borrow_mut();
        let known = spec.split_once('/').and_then(|(p, m)| s.registry.find(p, m));
        match known {
            None => Err(mlua::Error::runtime(format!("Model {spec} not found (see serpis.agent.models())"))),
            Some(m) if !s.registry.configured_providers().contains(&m.provider) => Err(mlua::Error::runtime(format!("No credentials for provider {}", m.provider))),
            Some(_) => {
                s.actions.push(Action::SetModel { chat: id, spec });
                Ok(())
            }
        }
    })?)?;
    let (id_of2, st2) = (id_of.clone(), st.clone());
    chat.set("set_thinking", lua.create_function(move |_, (id, level): (Option<String>, String)| {
        let id = id_of2(id)?;
        if serpis_ai::ThinkingLevel::parse(&level).is_none() {
            return Err(mlua::Error::runtime(format!("Unknown thinking level {level} (off, minimal, low, medium, high, xhigh, max)")));
        }
        st2.borrow_mut().actions.push(Action::SetThinking { chat: id, level });
        Ok(())
    })?)?;
    let (id_of2, st2) = (id_of.clone(), st.clone());
    chat.set("prompt", lua.create_function(move |_, (id, text): (Option<String>, String)| {
        let id = id_of2(id)?;
        st2.borrow_mut().actions.push(Action::Prompt { chat: id, text });
        Ok(())
    })?)?;
    inert(lua, &chat)?;
    s.set("chat", chat)?;

    inert(lua, &s)?;
    Ok(s)
}

fn model_json(m: &serpis_ai::Model, configured: bool) -> Value {
    json!({"spec": format!("{}/{}", m.provider, m.id), "provider": m.provider, "id": m.id, "name": m.name, "reasoning": m.reasoning, "configured": configured})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node_lua(init: &str) -> (NodeLua, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("nlua-{}", uuid::Uuid::now_v7().simple()));
        std::fs::create_dir_all(dir.join("extensions")).unwrap();
        std::fs::write(dir.join("init.lua"), init).unwrap();
        let registry = serpis_ai::Registry::new(serpis_ai::Paths { agent_dir: dir.join("agent") });
        (NodeLua::load(&dir, registry, Default::default()), dir)
    }

    #[test]
    fn a_window_config_loads_here_and_its_window_parts_do_nothing() {
        let (l, dir) = node_lua(
            r#"
            serpis.keymap.set("global", "s-k", function() end)
            serpis.command.define("x", function() end, "doc")
            serpis.ui.header("h", function() return "x" end)
            serpis.font.set({ size = 15 })
            local cur = serpis.buffer.current()
            serpis.var.set("agent-model", "mock/m")
            serpis.var.set("where", serpis.where)
            serpis.var.define("presets", { a = 1 }, "doc")
            serpis.var.define("agent-model", "other/x")
            for _ in pairs(serpis.var.get("presets")) do end
            serpis.hook.add("before-prompt", function(e)
              if e.text == "no" then return { block = "refused" } end
            end)
            serpis.agent.tool({ name = "now", description = "Time.", execute = function(a) return "t:" .. tostring(a.x) end })
            "#,
        );
        assert!(l.errors.is_empty(), "{:?}", l.errors);
        assert_eq!(l.var("agent-model"), Some(json!("mock/m")));
        assert_eq!(l.var("where"), Some(json!("node")));
        assert_eq!(l.run_hook("before-prompt", &json!({"text": "no"})), vec![json!({"block": "refused"})]);
        assert!(l.run_hook("before-prompt", &json!({"text": "yes"})).is_empty());
        assert_eq!(l.tool_specs().iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["now"]);
        assert_eq!(l.call_tool("c", "now", &json!({"x": 2})).text_content(), "t:2");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn errors_are_kept_per_file_and_the_rest_still_loads() {
        let (l, dir) = node_lua("serpis.var.set('a', 1)\nerror('boom')");
        std::fs::write(dir.join("extensions/ok.lua"), "serpis.var.set('b', 2)").unwrap();
        let mut l = l;
        l.reload(&dir);
        assert_eq!((l.var("a"), l.var("b")), (Some(json!(1)), Some(json!(2))));
        assert_eq!(l.errors.len(), 1);
        assert!(l.errors[0].contains("boom"), "{:?}", l.errors);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn chats_are_read_from_their_snapshot_and_acted_on_later() {
        let (l, dir) = node_lua(
            r#"
            serpis.hook.add("assistant-message", function(e)
              serpis.chat.append(e.buffer, "limit", { n = 1 })
              serpis.message(#serpis.chat.entries(e.buffer, "limit") .. " " .. tostring(serpis.chat.running()))
              serpis.chat.prompt(nil, "again")
            end)
            "#,
        );
        l.state.borrow_mut().chats.insert("c1".into(), Info { json: json!({"chat": "c1"}), usage: json!({}), cache: json!({}), running: true, entries: vec![], agent: None });
        l.run_hook_for(Some("c1"), "assistant-message", &json!({"buffer": "c1"}));
        let s = l.state.borrow();
        assert_eq!(s.notices.iter().map(|(c, _, t)| (c.as_deref(), t.as_str())).collect::<Vec<_>>(), [(Some("c1"), "1 true")]);
        assert!(matches!(&s.actions[..], [Action::Append { chat, .. }, Action::Prompt { text, .. }] if chat == "c1" && text == "again"));
        drop(s);
        let _ = std::fs::remove_dir_all(dir);
    }
}
