// Plugin ABI identity: compiler version + a hash of this crate's sources. Rust has no
// stable ABI, so a plugin must be rebuilt against the exact serpis build that loads it;
// the hash lets the loader refuse stale plugins instead of crashing.
use std::hash::{Hash, Hasher};

fn hash_dir(dir: &std::path::Path, h: &mut std::collections::hash_map::DefaultHasher) {
    let mut entries: Vec<_> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            hash_dir(&p, h);
        } else if let Ok(bytes) = std::fs::read(&p) {
            p.to_string_lossy().hash(h);
            bytes.hash(h);
        }
    }
}

fn main() {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let out = std::process::Command::new(rustc).arg("-V").output().ok();
    let v = out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    hash_dir(std::path::Path::new("src"), &mut h);
    std::fs::read("Cargo.toml").unwrap_or_default().hash(&mut h);
    println!("cargo:rustc-env=SERPIS_RUSTC_VERSION={v}");
    println!("cargo:rustc-env=SERPIS_BUILD_ID={:016x}", h.finish());
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
}
