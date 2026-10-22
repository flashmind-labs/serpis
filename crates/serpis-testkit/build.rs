// The workspace links std dynamically (see .cargo/config.toml): point serpis-mock-provider at the
// toolchain's std, as serpis-bin does.
fn main() {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    if let Ok(out) = std::process::Command::new(rustc).args(["--print", "target-libdir"]).output() {
        let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
        println!("cargo:rustc-link-arg-bins=-Wl,-rpath,{dir}");
    }
}
