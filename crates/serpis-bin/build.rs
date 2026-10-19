// The binary links std and libserpis dynamically (shared with Rust plugins). Point the
// runtime search path at the toolchain's std and at the binary's own directory.
fn main() {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    if let Ok(out) = std::process::Command::new(rustc).args(["--print", "target-libdir"]).output() {
        let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
        println!("cargo:rustc-link-arg=-Wl,-rpath,{dir}");
    }
    if cfg!(target_os = "macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path");
    } else {
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
    }
}
