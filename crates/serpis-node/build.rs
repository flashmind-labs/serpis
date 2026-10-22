// The workspace links std dynamically (`-C prefer-dynamic` in .cargo/config.toml, for the
// desktop's Rust plugins), so a dev build of the node finds std in the toolchain. Release
// builds for boxes link it statically instead (scripts/build-node.sh), and need none of this.
fn main() {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    if let Ok(out) = std::process::Command::new(rustc).args(["--print", "target-libdir"]).output() {
        let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
        println!("cargo:rustc-link-arg=-Wl,-rpath,{dir}");
    }
}
