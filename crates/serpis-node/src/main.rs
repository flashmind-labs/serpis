//! `serpis-node`: run chats on this machine for serpis elsewhere.
//!
//! - `serpis-node serve`: run the node here, in the foreground.
//! - `serpis-node agent`: join stdin and stdout to the node (what `ssh <host> serpis-node agent`
//!   runs), starting it if none is running.
//! - `serpis-node stop`: stop the node running here, if any (its open chats come back when it
//!   starts again).
//! - `serpis-node terminals`: run this machine's terminal host in the foreground.
//! - `serpis-node stop-terminals`: stop it, ending every terminal in it.
//! - `serpis-node install-service` / `uninstall-service`: start the node at boot and after
//!   crashes (systemd user unit, launchd agent), or stop doing that.
//! - `serpis-node version`.

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().thread_name("serpis-node-rt").build()?;
    match args.first().map(String::as_str) {
        Some("serve") => {
            tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
                .init();
            let node = match serpis_node::server::start(serpis_node::server::Config::default_paths(), rt.handle().clone()) {
                Ok(n) => n,
                // Nothing to do (a service starting one at login finds it running): not a failure,
                // or the service would start it again and again.
                Err(e) if format!("{e:#}").contains("already running") => {
                    tracing::info!("{e:#}");
                    return Ok(());
                }
                Err(e) => return Err(e),
            };
            tracing::info!("serpis-node {} listening on {}", serpis_node::VERSION, node.socket.display());
            if let Err(e) = std::env::current_exe().map_err(anyhow::Error::from).and_then(|exe| serpis_node::service::note_current(&exe)) {
                tracing::warn!("noting this build as the current one: {e:#}");
            }
            node.wait();
            Ok(())
        }
        Some("terminals") => {
            tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
                .init();
            let socket = serpis_node::terminals::socket();
            tracing::info!("serpis-node {} holding terminals at {}", serpis_node::VERSION, socket.display());
            rt.block_on(serpis_node::terminals::serve(&socket))
        }
        // What remote-install-service does, for a machine without serpis's window.
        Some("install-service") => {
            let r = serpis_node::service::install(&std::env::current_exe()?, &serpis_node::socket_path())?;
            println!("serpis-node now starts at boot and after crashes ({:?}: {})", r.installed.manager, r.installed.file);
            if let Some(n) = r.note {
                println!("{n}");
            }
            Ok(())
        }
        Some("uninstall-service") => {
            match serpis_node::service::uninstall()? {
                Some(i) => println!("removed {}", i.file),
                None => println!("serpis-node isn't installed as a service"),
            }
            Ok(())
        }
        Some("stop-terminals") => rt.block_on(serpis_node::terminals::stop(&serpis_node::terminals::socket())),
        // stdout is the protocol: logs go to stderr, which ssh shows the client.
        Some("agent") => rt.block_on(serpis_node::bridge::agent(&serpis_node::socket_path())),
        Some("stop") => rt.block_on(async {
            let target = serpis_node::client::Target::Local(serpis_node::socket_path());
            match serpis_node::client::Client::connect(&target, "serpis-node stop").await {
                Ok((c, _)) => c.call(serpis_proto::wire::Request::Stop).await.map(|_| ()),
                // None running: nothing to stop.
                Err(_) => Ok(()),
            }
        }),
        // What node builds of these sources are kept under (scripts/build-node.sh --store).
        Some("stamp") => {
            println!("{}", serpis_node::STAMP);
            Ok(())
        }
        Some("version" | "--version" | "-V") => {
            println!("serpis-node {} (protocol {})", serpis_node::VERSION, serpis_proto::wire::VERSION);
            Ok(())
        }
        _ => {
            eprintln!("usage: serpis-node serve | agent | stop | install-service | uninstall-service | terminals | stop-terminals | version | stamp");
            std::process::exit(2);
        }
    }
}
