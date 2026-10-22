//! The mock provider as a program, for scripts/ui-test.sh: listens on a free port (written to
//! <port-file>) and logs each request's cache verdict to <log-file>.
//!
//!   serpis-mock-provider <port-file> <log-file>

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, port_file, log_file] = &args[..] else {
        eprintln!("usage: serpis-mock-provider <port-file> <log-file>");
        std::process::exit(2);
    };
    let mock = serpis_testkit::Mock::start(serpis_testkit::commands()).await;
    mock.log_to(log_file.into());
    let port = mock.base.rsplit(':').next().unwrap().to_string();
    std::fs::write(port_file, port).expect("write port file");
    std::future::pending::<()>().await;
}
