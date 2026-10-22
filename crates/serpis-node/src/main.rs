//! `serpis-node`: run chats on this machine for serpis elsewhere.
//!
//! - `serpis-node serve`: run the node here, in the foreground.
//! - `serpis-node agent`: join stdin and stdout to the node (what `ssh <host> serpis-node agent`
//!   runs), starting it if none is running.
//! - `serpis-node version`.

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().thread_name("serpis-node-rt").build()?;
    match args.first().map(String::as_str) {
        Some("serve") => {
            tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
                .init();
            let node = serpis_node::server::start(serpis_node::server::Config::default_paths(), rt.handle().clone())?;
            tracing::info!("serpis-node {} listening on {}", serpis_node::VERSION, node.socket.display());
            node.wait();
            Ok(())
        }
        // stdout is the protocol: logs go to stderr, which ssh shows the client.
        Some("agent") => rt.block_on(serpis_node::bridge::agent(&serpis_node::socket_path())),
        Some("version" | "--version" | "-V") => {
            println!("serpis-node {} (protocol {})", serpis_node::VERSION, serpis_proto::wire::VERSION);
            Ok(())
        }
        _ => {
            eprintln!("usage: serpis-node serve | agent | version");
            std::process::exit(2);
        }
    }
}
