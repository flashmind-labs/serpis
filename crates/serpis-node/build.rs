// The workspace links std dynamically (`-C prefer-dynamic` in .cargo/config.toml, for the
// desktop's Rust plugins), so a dev build of the node finds std in the toolchain. Release
// builds for boxes link it statically instead (scripts/build-node.sh), and need none of this.
//
// SERPIS_NODE_STAMP: a hash of the sources a node is built from (this crate and those it runs
// chats with). The desktop files node builds under it, so a change to any of them means a new
// build, whatever the version number says.
fn main() {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    if let Ok(out) = std::process::Command::new(rustc).args(["--print", "target-libdir"]).output() {
        let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
        println!("cargo:rustc-link-arg=-Wl,-rpath,{dir}");
    }
    let here = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let crates = here.parent().unwrap();
    let mut files = Vec::new();
    for c in ["serpis-node", "serpis-proto", "serpis-chat", "serpis-agent", "serpis-ai"] {
        let dir = crates.join(c);
        println!("cargo:rerun-if-changed={}", dir.join("src").display());
        println!("cargo:rerun-if-changed={}", dir.join("Cargo.toml").display());
        files.push(dir.join("Cargo.toml"));
        walk(&dir.join("src"), &mut files);
    }
    files.sort();
    // FNV-1a over paths (relative) and contents: stable across machines and Rust versions.
    let mut h: u64 = 0xcbf29ce484222325;
    let mut eat = |bytes: &[u8]| {
        for b in bytes {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    };
    for f in files {
        eat(f.strip_prefix(crates).unwrap_or(&f).to_string_lossy().as_bytes());
        eat(&std::fs::read(&f).unwrap_or_default());
    }
    println!("cargo:rustc-env=SERPIS_NODE_STAMP={}-{h:016x}", std::env::var("CARGO_PKG_VERSION").unwrap());
}

fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}
