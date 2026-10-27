//! The test ssh server as a program, for the phone app's UI tests (ios/test-machine.sh): lets in
//! the keys in <authorized-keys-file> (read at every login, so keys added later get in) and runs
//! what it is asked to here, in this environment. Writes `<port> <host key fingerprint>` to
//! <port-file> once it listens, and its host key to <port-file>.hostkey.
//!
//!   serpis-test-sshd <authorized-key-file> <port-file>

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let [_, key_file, port_file] = &args[..] else {
        eprintln!("usage: serpis-test-sshd <authorized-key-file> <port-file>");
        std::process::exit(2);
    };
    let sshd = serpis_testkit::sshd::start_for(serpis_testkit::sshd::Authorized::File(key_file.into())).await?;
    std::fs::write(format!("{port_file}.hostkey"), sshd.host_key.to_openssh()?)?;
    std::fs::write(port_file, format!("{} {}", sshd.port, sshd.host_key.fingerprint(Default::default())))?;
    std::future::pending::<()>().await;
    Ok(())
}
