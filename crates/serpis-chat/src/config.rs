//! Where the Lua config lives, shared by the window and the node: both run the same `init.lua`
//! and extensions, each doing its own part of them.

use std::path::{Path, PathBuf};

/// `$SERPIS_CONFIG_DIR`, else `~/.config/serpis`.
pub fn config_dir() -> PathBuf {
    std::env::var_os("SERPIS_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".config").join("serpis"))
}

/// Lua's `package.path` for `dir`: its `lua/` modules and extensions, then `rest`.
pub fn package_path(dir: &Path, rest: &str) -> String {
    let d = dir.display();
    format!("{d}/lua/?.lua;{d}/lua/?/init.lua;{d}/extensions/?.lua;{d}/extensions/?/init.lua;{rest}")
}

/// The extensions in `<config>/extensions`, by name: `name.lua` files and `name/init.lua`
/// directories (which can `require` their own modules), sorted.
pub fn extensions(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = std::fs::read_dir(dir.join("extensions"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let name = p.file_stem()?.to_string_lossy().into_owned();
            if p.is_dir() {
                let init = p.join("init.lua");
                init.exists().then_some((name, init))
            } else {
                (p.extension()? == "lua").then_some((name, p))
            }
        })
        .collect();
    out.sort();
    out
}
