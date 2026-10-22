//! Rust plugins: `dylib` crates that link the shared `serpis` library and receive
//! `&mut Editor` — full access to the internals, like Emacs packages.
//!
//! Plugins live in `~/.config/serpis/plugins/` (or are listed in the `plugin-paths` var).
//! Each is copied to a unique temporary file before loading, so a rebuilt plugin is picked
//! up as a fresh library: on change, everything the plugin registered is removed, its
//! buffers are killed, `serpis_plugin_deinit` runs, and the new build is loaded.
//!
//! A plugin is a `crate-type = ["dylib"]` crate depending on `serpis`:
//!
//! ```ignore
//! fn init(ed: &mut serpis::Editor, owner: &str) {
//!     ed.define_command("hello", "Say hello.", owner, |ed, _| { ed.message("hi"); Ok(()) });
//! }
//! serpis::plugin!(init);
//! ```

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::editor::Editor;

pub type InitFn = fn(&mut Editor, &str);
pub type DeinitFn = fn(&mut Editor);
pub type AbiFn = fn() -> &'static str;

/// Must match between host and plugin: serpis version, source build id and compiler.
pub const ABI_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "+", env!("SERPIS_BUILD_ID"), "/", env!("SERPIS_RUSTC_VERSION"));

/// Export a plugin's entry point.
#[macro_export]
macro_rules! plugin {
    ($init:path) => {
        #[unsafe(no_mangle)]
        pub fn serpis_plugin_abi() -> &'static str {
            $crate::plugins::ABI_VERSION
        }
        #[unsafe(no_mangle)]
        pub fn serpis_plugin_init(ed: &mut $crate::Editor, owner: &str) {
            $init(ed, owner)
        }
    };
    ($init:path, $deinit:path) => {
        $crate::plugin!($init);
        #[unsafe(no_mangle)]
        pub fn serpis_plugin_deinit(ed: &mut $crate::Editor) {
            $deinit(ed)
        }
    };
}

struct Loaded {
    path: PathBuf,
    owner: String,
    mtime: SystemTime,
    lib: Option<libloading::Library>,
}

#[derive(Default)]
pub struct PluginManager {
    loaded: Vec<Loaded>,
    last_scan: Option<Instant>,
}

fn lib_ext() -> &'static str {
    if cfg!(target_os = "macos") {
        "dylib"
    } else if cfg!(windows) {
        "dll"
    } else {
        "so"
    }
}

fn plugin_files(ed: &Editor) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let dir = crate::config_dir().join("plugins");
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == lib_ext()) {
                out.push(p);
            }
        }
    }
    if let Some(serde_json::Value::Array(extra)) = ed.var("plugin-paths") {
        for p in extra.iter().filter_map(|v| v.as_str()) {
            out.push(crate::expand_home(p));
        }
    }
    out.sort();
    out
}

fn owner_for(path: &Path) -> String {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    format!("plugin:{}", stem.trim_start_matches("lib"))
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

fn load(ed: &mut Editor, path: &Path) -> anyhow::Result<Loaded> {
    let owner = owner_for(path);
    let tmp_dir = std::env::temp_dir().join("serpis-plugins");
    std::fs::create_dir_all(&tmp_dir)?;
    let tmp = tmp_dir.join(format!("{}-{}.{}", owner.replace(':', "-"), uuid::Uuid::new_v4().simple(), lib_ext()));
    std::fs::copy(path, &tmp)?;
    // SAFETY: loading Rust code built against this exact serpis (checked below) is the
    // contract of Rust plugins.
    let lib = unsafe { libloading::Library::new(&tmp)? };
    let _ = std::fs::remove_file(&tmp);
    let abi: libloading::Symbol<AbiFn> = unsafe { lib.get(b"serpis_plugin_abi")? };
    let theirs = abi();
    if theirs != ABI_VERSION {
        anyhow::bail!("built against a different serpis ({theirs}; running {ABI_VERSION}). Rebuild the plugin against this serpis.");
    }
    let init: libloading::Symbol<InitFn> = unsafe { lib.get(b"serpis_plugin_init")? };
    init(ed, &owner);
    Ok(Loaded {
        path: path.to_path_buf(),
        mtime: mtime(path).unwrap_or(SystemTime::UNIX_EPOCH),
        owner,
        lib: Some(lib),
    })
}

fn unload(ed: &mut Editor, mut p: Loaded) {
    if let Some(lib) = &p.lib {
        if let Ok(deinit) = unsafe { lib.get::<DeinitFn>(b"serpis_plugin_deinit") } {
            deinit(ed);
        }
    }
    // Buffers created from the plugin's kinds run plugin code: kill them first.
    let kinds: Vec<String> = ed.buffer_kinds.iter().filter(|(_, k)| k.owner == p.owner).map(|(n, _)| n.clone()).collect();
    let doomed: Vec<_> = ed.buffers.iter().filter(|(_, s)| kinds.contains(&s.meta.major)).map(|(id, _)| id).collect();
    for id in doomed {
        ed.kill_buffer(id);
    }
    ed.remove_owner(&p.owner);
    // Dropping the library unmaps it (or leaves it resident if the platform refuses).
    drop(p.lib.take());
}

pub fn reload_all(ed: &mut Editor) {
    let mut mgr = std::mem::take(&mut ed.plugins);
    for p in mgr.loaded.drain(..) {
        unload(ed, p);
    }
    for path in plugin_files(ed) {
        match load(ed, &path) {
            Ok(l) => {
                ed.message(format!("Loaded plugin {}", l.owner));
                mgr.loaded.push(l);
            }
            Err(e) => {
                ed.error(format!("plugin {}: {e:#}", path.display()));
                // Retry only when the file changes.
                mgr.loaded.push(Loaded { owner: owner_for(&path), mtime: mtime(&path).unwrap_or(SystemTime::UNIX_EPOCH), path, lib: None });
            }
        }
    }
    mgr.last_scan = Some(Instant::now());
    ed.plugins = mgr;
}

/// Reload plugins whose files changed, and load new ones (checked once a second), with
/// config-auto-reload on; off (the default), reload-config does it.
pub fn poll_reload(ed: &mut Editor) {
    if !ed.var("config-auto-reload").and_then(serde_json::Value::as_bool).unwrap_or(false) {
        return;
    }
    if ed.plugins.last_scan.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
        return;
    }
    let mut mgr = std::mem::take(&mut ed.plugins);
    mgr.last_scan = Some(Instant::now());
    let files = plugin_files(ed);
    let mut i = 0;
    while i < mgr.loaded.len() {
        let changed = mtime(&mgr.loaded[i].path) != Some(mgr.loaded[i].mtime);
        if changed {
            let old = mgr.loaded.remove(i);
            let path = old.path.clone();
            unload(ed, old);
            if path.exists() {
                // Give the compiler a moment to finish writing.
                std::thread::sleep(Duration::from_millis(100));
                match load(ed, &path) {
                    Ok(l) => {
                        ed.message(format!("Reloaded plugin {}", l.owner));
                        mgr.loaded.insert(i, l);
                        i += 1;
                    }
                    Err(e) => ed.error(format!("plugin {}: {e:#}", path.display())),
                }
            }
        } else {
            i += 1;
        }
    }
    for f in files {
        if !mgr.loaded.iter().any(|l| l.path == f) {
            match load(ed, &f) {
                Ok(l) => {
                    ed.message(format!("Loaded plugin {}", l.owner));
                    mgr.loaded.push(l);
                }
                Err(e) => {
                    ed.error(format!("plugin {}: {e:#}", f.display()));
                    // Do not retry a broken file until it changes.
                    mgr.loaded.push(Loaded { path: f.clone(), owner: owner_for(&f), mtime: mtime(&f).unwrap_or(SystemTime::UNIX_EPOCH), lib: None });
                }
            }
        }
    }
    ed.plugins = mgr;
}
