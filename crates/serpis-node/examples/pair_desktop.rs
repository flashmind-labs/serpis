//! A desktop pairing a phone, for the phone app's UI tests (ios/test-machine.sh): starts a
//! pairing on the node here (this environment's), writes the link `phone-pair` would show to
//! <dir>/pair.link, lets in the first phone that asks, as you would say yes, and writes
//! <dir>/pair.done.
//!
//!   pair_desktop <dir> <host> <port> <user> <host-key-file>

use base64::Engine;
use serpis_node::client::{Client, Target};
use serpis_proto::wire::{Event, Request};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let [_, dir, host, port, user, host_key] = &args[..] else {
        anyhow::bail!("usage: pair_desktop <dir> <host> <port> <user> <host-key-file>");
    };
    let dir = std::path::Path::new(dir);
    let node = std::env::current_exe()?.parent().and_then(|d| d.parent()).map(|d| d.join("serpis-node")).filter(|p| p.is_file()).unwrap_or_else(|| "serpis-node".into());
    let (desktop, mut events) = Client::connect(&Target::Command(vec![node.to_string_lossy().into(), "agent".into()]), "desktop").await?;

    let key = dir.join("pairing");
    let _ = std::fs::remove_file(&key);
    let _ = std::fs::remove_file(key.with_extension("pub"));
    anyhow::ensure!(std::process::Command::new("ssh-keygen").args(["-q", "-t", "ed25519", "-N", "", "-f"]).arg(&key).status()?.success());
    let public = std::fs::read_to_string(key.with_extension("pub"))?;
    desktop.call(Request::StartPairing { pairing: "uitest".into(), key: public.split_whitespace().take(2).collect::<Vec<_>>().join(" "), minutes: 30 }).await?;
    let body: String = std::fs::read_to_string(&key)?.lines().filter(|l| !l.starts_with("-----")).collect();
    let until = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs() + 30 * 60;
    let hk = std::fs::read_to_string(host_key)?.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
    let payload = serde_json::json!({"v": 1, "p": "uitest", "k": body, "x": until, "m": [{"n": "box", "h": host, "p": port.parse::<u16>()?, "u": user, "k": [hk]}]});
    std::fs::write(dir.join("pair.link"), format!("serpis://pair#{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.to_string())))?;

    while let Some(ev) = events.recv().await {
        if let Event::PairRequested { pairing, key, label } = ev {
            desktop.call(Request::AuthorizeKey { key, comment: format!("serpis phone {label}") }).await?;
            desktop.call(Request::PairAnswer { pairing: pairing.clone(), approved: true }).await?;
            desktop.call(Request::EndPairing { pairing }).await?;
            std::fs::write(dir.join("pair.done"), label)?;
            break;
        }
    }
    Ok(())
}
