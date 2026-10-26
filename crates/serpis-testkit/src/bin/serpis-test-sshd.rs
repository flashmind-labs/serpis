//! The test ssh server as a program, for the phone app's UI tests (ios/test-machine.sh): lets in
//! the key in <authorized-key-file> and runs what it is asked to here, in this environment.
//! Writes `<port> <host key fingerprint>` to <port-file> once it listens.
//!
//!   serpis-test-sshd <authorized-key-file> <port-file>

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let [_, key_file, port_file] = &args[..] else {
        eprintln!("usage: serpis-test-sshd <authorized-key-file> <port-file>");
        std::process::exit(2);
    };
    let key = russh::keys::PublicKey::from_openssh(std::fs::read_to_string(key_file)?.trim())?;
    let sshd = serpis_testkit::sshd::start(key).await?;
    std::fs::write(port_file, format!("{} {}", sshd.port, sshd.host_key.fingerprint(Default::default())))?;
    std::future::pending::<()>().await;
    Ok(())
}
