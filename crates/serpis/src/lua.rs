//! Lua configuration and extensions.
//!
//! `~/.config/serpis/init.lua` (plus modules under `lua/`) runs in a Lua 5.4 state with a
//! `serpis` table that exposes the editor API: commands, keymaps, hooks, variables, buffers,
//! windows, agent tools. Everything Lua registers is owned by `lua`, so a reload removes it
//! exactly before the files run again in a fresh state. Edits are picked up automatically.

use std::cell::Cell;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use futures::FutureExt;
use mlua::{Function, Lua, LuaSerdeExt, Table, Value as LuaValue};
use serde_json::{Value, json};

use crate::editor::{Args, Editor, buffer_id_from_u64, buffer_id_to_u64};

pub const OWNER: &str = "lua";

pub const DEFAULT_INIT: &str = r#"-- Serpis configuration. Saved changes are applied immediately (hot reload).
-- Everything here uses the same API as the built-in packages: see `describe-command`,
-- `describe-keymap` and `describe-variable` (M-x) to explore.

-- Keybinding scheme: "regular" (default), "emacs" or "vim".
-- serpis.keys.scheme("vim")

-- Default model and thinking level for new chats ("provider/model-id").
-- serpis.var.set("agent-model", "anthropic/claude-sonnet-5-5")
-- serpis.var.set("agent-thinking", "medium")

-- Bind keys in any keymap: "global", a major mode ("chat", "terminal", "text"),
-- a minor mode ("vim-normal", "vim-insert") or "<minor>:<major>" ("vim-normal:chat").
serpis.keymap.set("global", "s-k", function()
  serpis.message("Hello from init.lua — edit me with s-, (open-config)")
end)

-- Define commands (they show up in M-x).
serpis.command.define("insert-date", function()
  serpis.buffer.insert(os.date("%Y-%m-%d"))
end, "Insert today's date.")

-- Give the agent a new tool, written in Lua.
serpis.agent.tool({
  name = "current_time",
  description = "Return the current local date and time.",
  parameters = { type = "object", properties = {} },
  execute = function(args)
    return os.date("%Y-%m-%d %H:%M:%S")
  end,
})

-- React to editor events.
serpis.hook.add("agent-finished", function()
  -- serpis.message("Agent finished")
end)

-- Guard tool calls: return { block = "reason" } to refuse, or { args = {...} } to rewrite.
serpis.hook.add("before-tool-call", function(e)
  if e.name == "bash" and e.args.command and e.args.command:match("rm %-rf /") then
    return { block = "Refusing to run rm -rf /" }
  end
end)
"#;

thread_local! {
    static CURRENT: Cell<*mut Editor> = const { Cell::new(std::ptr::null_mut()) };
    static ANON: Cell<u64> = const { Cell::new(0) };
}

/// Run `f` with the editor pointer published for Lua API functions.
pub fn with_editor_ptr<R>(ed: &mut Editor, f: impl FnOnce() -> R) -> R {
    let prev = CURRENT.with(|c| c.replace(ed as *mut Editor));
    let r = f();
    CURRENT.with(|c| c.set(prev));
    r
}

/// Access the editor from inside a Lua callback.
fn ed<R>(f: impl FnOnce(&mut Editor) -> R) -> mlua::Result<R> {
    let p = CURRENT.with(Cell::get);
    if p.is_null() {
        return Err(mlua::Error::runtime("serpis API used outside of an editor callback"));
    }
    // SAFETY: the pointer is set by `with_editor_ptr` from a live `&mut Editor` whose
    // borrow is suspended for the duration of the Lua call (single-threaded UI).
    Ok(f(unsafe { &mut *p }))
}

pub struct LuaRuntime {
    pub lua: Lua,
    /// Previous states stay alive so stale callbacks (timers, scripts) never touch a
    /// destroyed Lua; their registrations were already removed by `remove_owner`.
    retired: Vec<Lua>,
    rx: mpsc::Receiver<()>,
    _watcher: Option<notify::RecommendedWatcher>,
    changed_at: Option<Instant>,
    /// Content hash of the config's .lua files when last loaded.
    stamp: u64,
}

/// Hash of every .lua file under the config dir (paths and contents).
fn config_stamp(dir: &std::path::Path) -> u64 {
    use std::hash::{Hash, Hasher};
    fn walk(d: &std::path::Path, h: &mut std::collections::hash_map::DefaultHasher) {
        let mut entries: Vec<_> = std::fs::read_dir(d).into_iter().flatten().flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                walk(&p, h);
            } else if p.extension().is_some_and(|e| e == "lua") {
                p.hash(h);
                std::fs::read(&p).unwrap_or_default().hash(h);
            }
        }
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    walk(dir, &mut h);
    h.finish()
}

/// Call a Lua function with the editor published, converting JSON args and result.
/// `lua` must be the state that owns `f`.
pub fn call(ed: &mut Editor, lua: &Lua, f: &Function, arg: &Value) -> Option<Value> {
    let result = with_editor_ptr(ed, || -> mlua::Result<Value> {
        let a = lua.to_value(arg)?;
        let r: LuaValue = f.call(a)?;
        Ok(match r {
            LuaValue::Nil => Value::Null,
            other => lua.from_value(other).unwrap_or(Value::Null),
        })
    });
    match result {
        Ok(v) => (!v.is_null()).then_some(v),
        Err(e) => {
            ed.error(format!("lua: {e}"));
            None
        }
    }
}

fn anon_command(ed: &mut Editor, lua: &Lua, f: Function) -> String {
    let n = ANON.with(|c| {
        c.set(c.get() + 1);
        c.get()
    });
    let name = format!("lua-anonymous-{n}");
    define_lua_command(ed, lua, &name, f, "Anonymous Lua command.");
    name
}

fn define_lua_command(ed: &mut Editor, lua: &Lua, name: &str, f: Function, doc: &str) {
    let lua = lua.clone();
    ed.define_command(name, doc, OWNER, move |ed, a: &Args| {
        let arg = json!({"prefix": a.prefix, "params": a.params, "keys": crate::keys::format_seq(&a.keys)});
        call(ed, &lua, &f, &arg);
        Ok(())
    });
}

/// An agent tool implemented by a Lua function (run on the UI thread).
struct LuaTool {
    name: String,
    description: String,
    parameters: Value,
    main: crate::editor::MainSender,
}

impl serpis_agent::AgentTool for LuaTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> String {
        self.description.clone()
    }
    fn parameters(&self) -> Value {
        self.parameters.clone()
    }
    fn prompt_snippet(&self) -> Option<String> {
        Some(self.description.lines().next().unwrap_or("").to_string())
    }
    fn execute(&self, _id: String, args: Value, _ctx: serpis_agent::ToolContext) -> futures::future::BoxFuture<'static, anyhow::Result<serpis_agent::ToolResult>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let name = self.name.clone();
        self.main.send(move |ed| {
            let lua = ed.lua.as_ref().map(|l| l.lua.clone());
            let f = lua.as_ref().and_then(|l| {
                let tools: Table = l.named_registry_value("serpis_tools").ok()?;
                tools.get::<Function>(name.as_str()).ok()
            });
            let r = match (f, lua) {
                (Some(f), Some(lua)) => call(ed, &lua, &f, &args),
                _ => Some(json!({"text": format!("Lua tool {name} is no longer defined"), "is_error": true})),
            };
            let _ = tx.send(r);
        });
        async move {
            let r = rx.await.map_err(|_| anyhow::anyhow!("tool cancelled"))?;
            Ok(match r {
                Some(Value::String(s)) => serpis_agent::ToolResult::text(s),
                Some(Value::Object(o)) => {
                    let text = o.get("text").and_then(Value::as_str).unwrap_or("").to_string();
                    let mut r = serpis_agent::ToolResult::text(text);
                    r.is_error = o.get("is_error").and_then(Value::as_bool).unwrap_or(false);
                    r
                }
                Some(other) => serpis_agent::ToolResult::text(other.to_string()),
                None => serpis_agent::ToolResult::text("(no output)"),
            })
        }
        .boxed()
    }
}

fn api(lua: &Lua) -> mlua::Result<Table> {
    let s = lua.create_table()?;
    s.set("version", env!("CARGO_PKG_VERSION"))?;
    s.set("config_dir", crate::config_dir().to_string_lossy().into_owned())?;

    s.set("message", lua.create_function(|_, msg: String| ed(|e| e.message(msg)))?)?;
    s.set("error", lua.create_function(|_, msg: String| ed(|e| e.error(msg)))?)?;
    s.set("reload", lua.create_function(|_, ()| ed(|e| e.call("reload-config")))?)?;
    s.set(
        "schedule",
        lua.create_function(|lua, f: Function| {
            ed(|e| {
                let ptr = SendPtr(Box::new((lua.clone(), f)));
                e.main_sender().send(move |e| {
                    let ptr = ptr;
                    let (lua, f) = *ptr.0;
                    call(e, &lua, &f, &Value::Null);
                });
            })
        })?,
    )?;

    // Commands.
    let cmd = lua.create_table()?;
    cmd.set(
        "define",
        lua.create_function(|lua, (name, f, doc): (String, Function, Option<String>)| ed(|e| define_lua_command(e, lua, &name, f, doc.as_deref().unwrap_or(""))))?,
    )?;
    cmd.set(
        "run",
        lua.create_function(|lua, (name, params): (String, mlua::Variadic<LuaValue>)| {
            let params: Vec<Value> = params.into_iter().map(|v| lua.from_value(v).unwrap_or(Value::Null)).collect();
            ed(|e| {
                let args = Args { params, ..Default::default() };
                e.run_command(&name, &args).map_err(|err| mlua::Error::runtime(format!("{err:#}")))
            })?
        })?,
    )?;
    cmd.set(
        "list",
        lua.create_function(|_, ()| ed(|e| {
            let mut v: Vec<String> = e.commands.keys().cloned().collect();
            v.sort();
            v
        }))?,
    )?;
    s.set("command", cmd)?;

    // Keymaps.
    let km = lua.create_table()?;
    km.set(
        "set",
        lua.create_function(|lua, (map, keys, target): (String, String, LuaValue)| {
            ed(|e| {
                let name = match target {
                    LuaValue::String(s) => s.to_string_lossy().to_string(),
                    LuaValue::Function(f) => anon_command(e, lua, f),
                    _ => return Err(mlua::Error::runtime("keymap.set: target must be a command name or function")),
                };
                e.bind(&map, &keys, &name, OWNER);
                Ok(())
            })?
        })?,
    )?;
    km.set("unset", lua.create_function(|_, (map, keys): (String, String)| ed(|e| e.unbind(&map, &keys)))?)?;
    km.set(
        "list",
        lua.create_function(|lua, map: String| {
            let pairs = ed(|e| e.keymap(&map).map(|m| m.flatten()).unwrap_or_default())?;
            let t = lua.create_table()?;
            for (k, c) in pairs {
                t.set(k, c)?;
            }
            Ok(t)
        })?,
    )?;
    s.set("keymap", km)?;

    // Hooks.
    let hk = lua.create_table()?;
    hk.set(
        "add",
        lua.create_function(|lua, (name, f): (String, Function)| {
            let lua = lua.clone();
            ed(|e| e.add_hook(&name, OWNER, move |ed, payload| call(ed, &lua, &f, payload)))
        })?,
    )?;
    hk.set("run", lua.create_function(|lua, (name, payload): (String, Option<LuaValue>)| {
        let p: Value = payload.map(|v| lua.from_value(v).unwrap_or(Value::Null)).unwrap_or(Value::Null);
        ed(|e| { e.run_hook(&name, &p); })
    })?)?;
    s.set("hook", hk)?;

    // Variables.
    let var = lua.create_table()?;
    var.set(
        "set",
        lua.create_function(|lua, (name, v): (String, LuaValue)| {
            let v: Value = lua.from_value(v)?;
            ed(|e| e.set_var(&name, v))
        })?,
    )?;
    var.set(
        "get",
        lua.create_function(|lua, name: String| {
            let v = ed(|e| e.var(&name).cloned())?;
            match v {
                Some(v) => lua.to_value(&v),
                None => Ok(LuaValue::Nil),
            }
        })?,
    )?;
    var.set(
        "define",
        lua.create_function(|lua, (name, v, doc): (String, LuaValue, Option<String>)| {
            let v: Value = lua.from_value(v)?;
            ed(|e| e.defvar(&name, v, doc.as_deref().unwrap_or("")))
        })?,
    )?;
    s.set("var", var)?;

    // Buffers.
    let buf = lua.create_table()?;
    buf.set("current", lua.create_function(|_, ()| ed(|e| buffer_id_to_u64(e.current_buffer())))?)?;
    buf.set("name", lua.create_function(|_, id: Option<u64>| ed(|e| {
        let id = id.map(buffer_id_from_u64).unwrap_or(e.current_buffer());
        e.meta(id).map(|m| m.name.clone())
    }))?)?;
    buf.set("kind", lua.create_function(|_, id: Option<u64>| ed(|e| {
        let id = id.map(buffer_id_from_u64).unwrap_or(e.current_buffer());
        e.meta(id).map(|m| m.major.clone())
    }))?)?;
    buf.set("modes", lua.create_function(|_, id: Option<u64>| ed(|e| {
        let id = id.map(buffer_id_from_u64).unwrap_or(e.current_buffer());
        e.meta(id).map(|m| m.minor_modes.clone()).unwrap_or_default()
    }))?)?;
    buf.set("list", lua.create_function(|lua, ()| {
        let list = ed(|e| e.buffers.iter().map(|(id, s)| json!({"id": buffer_id_to_u64(id), "name": s.meta.name, "kind": s.meta.major})).collect::<Vec<_>>())?;
        lua.to_value(&list)
    })?)?;
    buf.set("create", lua.create_function(|lua, (kind, name, params): (String, Option<String>, Option<LuaValue>)| {
        let p: Value = params.map(|v| lua.from_value(v).unwrap_or(Value::Null)).unwrap_or(Value::Null);
        ed(|e| e.create_buffer(&kind, name.as_deref(), &p).map(buffer_id_to_u64).map_err(|err| mlua::Error::runtime(format!("{err:#}"))))?
    })?)?;
    buf.set("show", lua.create_function(|_, id: u64| ed(|e| e.show_buffer(buffer_id_from_u64(id))))?)?;
    buf.set("kill", lua.create_function(|_, id: Option<u64>| ed(|e| {
        let id = id.map(buffer_id_from_u64).unwrap_or(e.current_buffer());
        e.kill_buffer(id)
    }))?)?;
    buf.set("by_name", lua.create_function(|_, name: String| ed(|e| e.buffer_by_name(&name).map(buffer_id_to_u64)))?)?;
    buf.set("scratch", lua.create_function(|_, ()| ed(|e| buffer_id_to_u64(e.scratch_buffer())))?)?;
    buf.set("exists", lua.create_function(|_, id: u64| ed(|e| e.buffers.contains_key(buffer_id_from_u64(id))))?)?;
    buf.set("directory", lua.create_function(|_, id: Option<u64>| ed(|e| {
        let id = id.map(buffer_id_from_u64).unwrap_or(e.current_buffer());
        e.buffer_directory(id).map(|d| d.to_string_lossy().into_owned())
    }))?)?;
    // Highlight a range for a moment: start/end are {line, col} (1-based, end exclusive).
    buf.set("highlight", lua.create_function(|_, (start, end, opts): (Vec<usize>, Vec<usize>, Option<Table>)| {
        let duration = opts.as_ref().and_then(|o| o.get::<u64>("duration").ok()).unwrap_or(150);
        let color = opts.as_ref().and_then(|o| o.get::<String>("color").ok()).and_then(|c| parse_hex(&c));
        let buffer: Option<u64> = opts.as_ref().and_then(|o| o.get::<u64>("buffer").ok());
        let pos = |v: &[usize]| crate::text::Pos::new(v.first().copied().unwrap_or(1).saturating_sub(1), v.get(1).copied().unwrap_or(1).saturating_sub(1));
        ed(|e| {
            let id = buffer.map(buffer_id_from_u64).unwrap_or(e.current_buffer());
            e.flash_region(id, pos(&start), pos(&end), Duration::from_millis(duration), color);
        })
    })?)?;
    buf.set("text", lua.create_function(|_, ()| ed(|e| e.with_surface(|s, _| s.all_text())))?)?;
    buf.set("line", lua.create_function(|_, n: usize| ed(|e| e.with_surface(|s, _| s.line(n.saturating_sub(1)))))?)?;
    buf.set("insert", lua.create_function(|_, text: String| ed(|e| e.self_insert(&text)))?)?;
    buf.set("cursor", lua.create_function(|_, ()| ed(|e| {
        let c = e.with_surface(|s, _| s.cursor());
        (c.map(|c| c.line + 1), c.map(|c| c.col + 1))
    }))?)?;
    buf.set("set_cursor", lua.create_function(|_, (line, col): (usize, usize)| ed(|e| {
        e.with_surface(|s, _| s.set_cursor(crate::text::Pos::new(line.saturating_sub(1), col.saturating_sub(1))));
    }))?)?;
    buf.set("set_local", lua.create_function(|lua, (key, v): (String, LuaValue)| {
        let v: Value = lua.from_value(v)?;
        ed(|e| {
            let id = e.current_buffer();
            if let Some(m) = e.meta_mut(id) {
                m.vars.insert(key, v);
            }
        })
    })?)?;
    buf.set("add_mode", lua.create_function(|_, (mode, id): (String, Option<u64>)| ed(|e| {
        let id = id.map(buffer_id_from_u64).unwrap_or(e.current_buffer());
        if let Some(m) = e.meta_mut(id) {
            if !m.has_mode(&mode) {
                m.minor_modes.insert(0, mode);
            }
        }
    }))?)?;
    buf.set("remove_mode", lua.create_function(|_, (mode, id): (String, Option<u64>)| ed(|e| {
        let id = id.map(buffer_id_from_u64).unwrap_or(e.current_buffer());
        if let Some(m) = e.meta_mut(id) {
            m.minor_modes.retain(|x| *x != mode);
        }
    }))?)?;
    s.set("buffer", buf)?;

    // Windows.
    let win = lua.create_table()?;
    win.set("split", lua.create_function(|_, dir: Option<String>| ed(|e| {
        e.split_window(dir.as_deref() == Some("below"));
    }))?)?;
    win.set("delete", lua.create_function(|_, ()| ed(|e| e.delete_window(e.focused)))?)?;
    win.set("only", lua.create_function(|_, ()| ed(|e| e.delete_other_windows()))?)?;
    win.set("other", lua.create_function(|_, n: Option<isize>| ed(|e| e.other_window(n.unwrap_or(1))))?)?;
    win.set("move", lua.create_function(|_, dir: String| ed(|e| e.windmove(&dir)))?)?;
    win.set("count", lua.create_function(|_, ()| ed(|e| e.layout.leaves().len()))?)?;
    win.set("configuration", lua.create_function(|lua, ()| {
        let c = ed(|e| e.window_configuration())?;
        lua.to_value(&c)
    })?)?;
    win.set("set_configuration", lua.create_function(|lua, conf: LuaValue| {
        let conf: Value = lua.from_value(conf)?;
        ed(|e| e.set_window_configuration(&conf).map_err(|err| mlua::Error::runtime(format!("{err:#}"))))?
    })?)?;
    win.set("buffers", lua.create_function(|_, ()| ed(|e| e.visible_buffers().into_iter().map(buffer_id_to_u64).collect::<Vec<_>>()))?)?;
    s.set("window", win)?;

    // Projects.
    let project = lua.create_table()?;
    project.set("root", lua.create_function(|_, dir: String| Ok(crate::project::root(std::path::Path::new(&dir)).map(|p| p.to_string_lossy().into_owned())))?)?;
    project.set("name", lua.create_function(|_, dir: String| Ok(crate::project::name(std::path::Path::new(&dir))))?)?;
    project.set("of_buffer", lua.create_function(|_, id: Option<u64>| ed(|e| {
        let id = id.map(buffer_id_from_u64).unwrap_or(e.current_buffer());
        e.buffer_directory(id).and_then(|d| crate::project::name(&d))
    }))?)?;
    s.set("project", project)?;

    // `serpis.highlight.on_yank(event, opts)`: flash the yanked region, like Neovim's
    // `vim.highlight.on_yank()` (call it from a `text-yanked` hook).
    let hl = lua.create_table()?;
    hl.set("on_yank", lua.create_function(|lua, (e, opts): (Table, Option<Table>)| {
        let serpis: Table = lua.globals().get("serpis")?;
        let buffer: Table = serpis.get("buffer")?;
        let highlight: Function = buffer.get("highlight")?;
        let o = match opts {
            Some(o) => o,
            None => lua.create_table()?,
        };
        if o.get::<LuaValue>("buffer")?.is_nil() {
            o.set("buffer", e.get::<LuaValue>("buffer")?)?;
        }
        highlight.call::<()>((e.get::<LuaValue>("start")?, e.get::<LuaValue>("end")?, o))
    })?)?;
    s.set("highlight", hl)?;

    // UI segments.
    let ui = lua.create_table()?;
    for (key, header) in [("status", false), ("header", true)] {
        ui.set(key, lua.create_function(move |lua, (name, f): (String, Function)| {
            let lua = lua.clone();
            ed(|e| e.add_segment(header, &name, OWNER, move |e, buf| {
                let arg = buf.map(|b| json!(buffer_id_to_u64(b))).unwrap_or(Value::Null);
                call(e, &lua, &f, &arg).and_then(|v| v.as_str().map(str::to_string))
            }))
        })?)?;
    }
    s.set("ui", ui)?;

    // Prompts with completion and optional live preview.
    let mb = lua.create_table()?;
    mb.set("read", lua.create_function(|lua, spec: Table| {
        let prompt: String = spec.get("prompt").unwrap_or_else(|_| "> ".into());
        let mut cands = Vec::new();
        if let Ok(list) = spec.get::<Table>("candidates") {
            for v in list.sequence_values::<LuaValue>() {
                match v? {
                    LuaValue::String(s) => cands.push(crate::minibuffer::Candidate::new(s.to_string_lossy().to_string())),
                    LuaValue::Table(t) => {
                        let label: String = t.get("label")?;
                        let value: Option<String> = t.get("value")?;
                        let detail: Option<String> = t.get("detail")?;
                        cands.push(crate::minibuffer::Candidate::new(label.clone()).value(value.unwrap_or(label)).detail(detail.unwrap_or_default()));
                    }
                    _ => {}
                }
            }
        }
        let require_match: bool = spec.get("require_match").unwrap_or(false);
        let keep_order = !spec.get::<bool>("sort").unwrap_or(true);
        let initial: Option<String> = spec.get("initial").ok();
        let on_accept: Option<Function> = spec.get("on_accept").ok();
        let on_select: Option<Function> = spec.get("on_select").ok();
        let on_cancel: Option<Function> = spec.get("on_cancel").ok();
        let l = lua.clone();
        ed(|e| {
            let la = l.clone();
            e.completing_read(&prompt, cands, require_match, move |e, c, input| {
                if let Some(f) = on_accept {
                    call(e, &la, &f, &json!({"value": c.map(|c| c.value), "input": input}));
                }
            });
            if let (Some(i), Some(m)) = (initial, e.minibuffer.as_mut()) {
                m.input.set_text(&i);
                m.input.move_to_end();
            }
            let ls = l.clone();
            let select = on_select.map(|f| -> crate::minibuffer::SelectFn {
                std::rc::Rc::new(move |e: &mut Editor, c: Option<&crate::minibuffer::Candidate>| {
                    call(e, &ls, &f, &json!(c.map(|c| c.value.clone())));
                })
            });
            let lc = l.clone();
            let cancel = on_cancel.map(|f| -> crate::minibuffer::CancelFn {
                Box::new(move |e: &mut Editor| {
                    call(e, &lc, &f, &Value::Null);
                })
            });
            e.minibuffer_preview(select, cancel, keep_order);
        })
    })?)?;
    s.set("minibuffer", mb)?;

    // Keys.
    let keys = lua.create_table()?;
    keys.set("scheme", lua.create_function(|_, name: Option<String>| ed(|e| {
        // Re-running a config that names the active scheme must not reset buffer states.
        if let Some(n) = name.filter(|n| e.var_str("keybinding-scheme").as_deref() != Some(n)) {
            crate::packages::set_scheme(e, &n);
        }
        e.var_str("keybinding-scheme")
    }))?)?;
    keys.set("feed", lua.create_function(|_, seq: String| ed(|e| {
        match crate::keys::parse_seq(&seq) {
            Ok(chords) => {
                for c in chords {
                    e.handle_chord(c);
                }
                Ok(())
            }
            Err(err) => Err(mlua::Error::runtime(err)),
        }
    })?)?)?;
    s.set("keys", keys)?;

    // Agent.
    let agent = lua.create_table()?;
    agent.set(
        "tool",
        lua.create_function(|lua, spec: Table| {
            let name: String = spec.get("name")?;
            let description: String = spec.get("description").unwrap_or_default();
            let params: LuaValue = spec.get("parameters")?;
            let parameters: Value = if params.is_nil() { json!({"type": "object", "properties": {}}) } else { lua.from_value(params)? };
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
            ed(|e| {
                let main = e.main_sender();
                e.register_tool(OWNER, std::sync::Arc::new(LuaTool { name, description, parameters, main }));
            })
        })?,
    )?;
    agent.set(
        "prompt",
        lua.create_function(|_, text: String| {
            ed(|e| {
                let id = e.current_buffer();
                let is_chat = e.meta(id).is_some_and(|m| m.major == "chat");
                let id = if is_chat {
                    id
                } else {
                    match e.create_buffer("chat", Some("*chat*"), &Value::Null) {
                        Ok(id) => {
                            e.show_buffer(id);
                            id
                        }
                        Err(err) => return Err(mlua::Error::runtime(format!("{err:#}"))),
                    }
                };
                e.with_buffer_as::<crate::buffers::chat::ChatBuffer, _>(id, |c, e| c.submit(e, text, false));
                Ok(())
            })?
        })?,
    )?;
    agent.set("model", lua.create_function(|_, spec: String| ed(|e| e.set_var("agent-model", Value::String(spec))))?)?;
    s.set("agent", agent)?;

    // Theme.
    let theme = lua.create_table()?;
    theme.set("toggle", lua.create_function(|_, ()| ed(|e| e.call("toggle-theme")))?)?;
    theme.set(
        "set",
        lua.create_function(|_, t: Table| {
            ed(|e| {
                if let Ok(v) = t.get::<f32>("font_size") {
                    e.theme.font_size = v;
                }
                if let Ok(v) = t.get::<f32>("mono_size") {
                    e.theme.mono_size = v;
                }
                if let Ok(v) = t.get::<String>("accent") {
                    if let Some(c) = parse_hex(&v) {
                        e.theme.accent = c;
                    }
                }
                if let Ok(v) = t.get::<String>("bg") {
                    if let Some(c) = parse_hex(&v) {
                        e.theme.bg = c;
                    }
                }
                if let Ok(v) = t.get::<String>("cursor") {
                    if let Some(c) = parse_hex(&v) {
                        e.theme.cursor = c;
                    }
                }
                e.theme.apply(&e.ctx);
            })
        })?,
    )?;
    s.set("theme", theme)?;

    // Timers.
    s.set(
        "defer",
        lua.create_function(|lua, (ms, f): (u64, Function)| {
            let id = DEFERRED.with(|d| {
                let mut d = d.borrow_mut();
                d.1 += 1;
                let id = d.1;
                d.0.insert(id, (lua.clone(), f));
                id
            });
            ed(|e| {
                let main = e.main_sender();
                e.rt.spawn(async move {
                    tokio::time::sleep(Duration::from_millis(ms)).await;
                    main.send(move |e| {
                        if let Some((lua, f)) = DEFERRED.with(|d| d.borrow_mut().0.remove(&id)) {
                            call(e, &lua, &f, &Value::Null);
                        }
                    });
                });
            })
        })?,
    )?;

    // Debug helpers (used by automated UI tests).
    let debug = lua.create_table()?;
    debug.set(
        "screenshot",
        lua.create_function(|_, path: String| {
            ed(|e| {
                let main = e.main_sender();
                e.ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |img| {
                    let [w, h] = img.size;
                    let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
                    let r = image::RgbaImage::from_raw(w as u32, h as u32, bytes).map(|i| i.save(&path));
                    main.send(move |e| match r {
                        Some(Ok(())) => e.message(format!("Screenshot saved to {path}")),
                        _ => e.error(format!("Screenshot failed: {path}")),
                    });
                })));
                e.ctx.request_repaint();
            })
        })?,
    )?;
    // Inject raw platform input: key name as egui names it ("A", "Enter", "Backslash"),
    // modifiers as a list ("cmd", "ctrl", "alt", "shift").
    debug.set(
        "key",
        lua.create_function(|_, (name, mods): (String, Option<Vec<String>>)| {
            let key = egui::Key::from_name(&name).ok_or_else(|| mlua::Error::runtime(format!("unknown key {name}")))?;
            let mut m = egui::Modifiers::NONE;
            for x in mods.unwrap_or_default() {
                match x.as_str() {
                    "cmd" => {
                        m.mac_cmd = cfg!(target_os = "macos");
                        m.command = true;
                    }
                    "ctrl" => m.ctrl = true,
                    "alt" => m.alt = true,
                    "shift" => m.shift = true,
                    _ => {}
                }
            }
            ed(|e| {
                e.injected_events.push(egui::Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers: m });
                e.injected_events.push(egui::Event::Key { key, physical_key: Some(key), pressed: false, repeat: false, modifiers: m });
                e.ctx.request_repaint();
            })
        })?,
    )?;
    debug.set(
        "text",
        lua.create_function(|_, t: String| {
            ed(|e| {
                e.injected_events.push(egui::Event::Text(t));
                e.ctx.request_repaint();
            })
        })?,
    )?;
    debug.set("quit", lua.create_function(|_, ()| ed(|e| e.quit_requested = true))?)?;
    debug.set("messages", lua.create_function(|_, ()| ed(|e| e.messages.clone()))?)?;
    s.set("debug", debug)?;
    Ok(s)
}

thread_local! {
    static DEFERRED: std::cell::RefCell<(std::collections::HashMap<u64, (Lua, Function)>, u64)> = std::cell::RefCell::new((Default::default(), 0));
}

/// Run a Lua file once (e.g. `SERPIS_TEST_SCRIPT`).
pub fn run_file(ed: &mut Editor, path: &std::path::Path) {
    let Some(lua) = ed.lua.as_ref().map(|l| l.lua.clone()) else { return };
    match std::fs::read_to_string(path) {
        Ok(code) => {
            if let Err(e) = with_editor_ptr(ed, || lua.load(&code).set_name(path.to_string_lossy()).exec()) {
                ed.error(format!("{}: {e}", path.display()));
            }
        }
        Err(e) => ed.error(format!("{}: {e}", path.display())),
    }
}

/// Lua functions are not `Send`; scheduled callbacks only ever run back on the UI thread.
struct SendPtr(Box<(Lua, Function)>);
unsafe impl Send for SendPtr {}

fn parse_hex(s: &str) -> Option<egui::Color32> {
    let s = s.trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some(egui::Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

fn watch(dir: &std::path::Path) -> (Option<notify::RecommendedWatcher>, mpsc::Receiver<()>) {
    use notify::Watcher;
    let (tx, rx) = mpsc::channel();
    let w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res {
            if ev.paths.iter().any(|p| p.extension().is_some_and(|e| e == "lua")) && !matches!(ev.kind, notify::EventKind::Access(_)) {
                let _ = tx.send(());
            }
        }
    })
    .ok()
    .and_then(|mut w| w.watch(dir, notify::RecursiveMode::Recursive).ok().map(|_| w));
    (w, rx)
}

/// (Re)create the Lua state and run `init.lua`.
pub fn reload(ed: &mut Editor) {
    ed.remove_owner(OWNER);
    let dir = crate::config_dir();
    let init: PathBuf = dir.join("init.lua");
    if !init.exists() {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(&init, DEFAULT_INIT);
    }
    let lua = Lua::new();
    let setup = || -> mlua::Result<()> {
        let package: Table = lua.globals().get("package")?;
        let path: String = package.get("path")?;
        package.set("path", format!("{}/lua/?.lua;{}/lua/?/init.lua;{path}", dir.display(), dir.display()))?;
        lua.globals().set("serpis", api(&lua)?)?;
        Ok(())
    };
    if let Err(e) = setup() {
        ed.error(format!("lua setup: {e}"));
        return;
    }
    let (watcher, rx, retired) = match ed.lua.take() {
        Some(mut old) => {
            old.retired.push(old.lua);
            (old._watcher, old.rx, old.retired)
        }
        None => {
            let (w, rx) = watch(&dir);
            (w, rx, Vec::new())
        }
    };
    ed.lua = Some(LuaRuntime {
        lua: lua.clone(),
        retired,
        rx,
        _watcher: watcher,
        changed_at: None,
        stamp: config_stamp(&dir),
    });
    let code = match std::fs::read_to_string(&init) {
        Ok(c) => c,
        Err(e) => {
            ed.error(format!("reading {}: {e}", init.display()));
            return;
        }
    };
    let r = with_editor_ptr(ed, || lua.load(&code).set_name(init.to_string_lossy()).exec());
    match r {
        Ok(()) => {
            ed.run_hook("config-loaded", &Value::Null);
            ed.message(format!("Loaded {}", init.display()));
        }
        Err(e) => ed.error(format!("init.lua: {e}")),
    }
}

/// Debounced hot reload when config files change.
pub fn poll_reload(ed: &mut Editor) {
    let Some(rt) = ed.lua.as_mut() else { return };
    while rt.rx.try_recv().is_ok() {
        rt.changed_at = Some(Instant::now());
    }
    if rt.changed_at.is_some_and(|t| t.elapsed() > Duration::from_millis(150)) {
        rt.changed_at = None;
        // Ignore events that do not change what would be loaded (late or duplicate events).
        if config_stamp(&crate::config_dir()) != rt.stamp {
            reload(ed);
        }
    } else if rt.changed_at.is_some() {
        ed.ctx.request_repaint_after(Duration::from_millis(160));
    }
}
