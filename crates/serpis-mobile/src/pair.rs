//! Pairing with a desktop: its `serpis://pair#…` link (the code the camera scans) carries the
//! machines it shares, the host keys it trusts for them, and a pairing key they let in for a few
//! minutes, only to propose a key. This device trusts those host keys, logs in to one machine
//! with the pairing key, offers its own key, and waits for the desktop's yes; the desktop then
//! lets it in on every machine shared.

use anyhow::{Context, Result, bail};
use base64::Engine;
use russh::keys::{PrivateKey, PublicKey};
use serde_json::Value;

/// A machine the desktop shares.
#[derive(Debug, Clone, PartialEq)]
pub struct SharedMachine {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    /// Its host keys, as the desktop trusts them (`type base64`).
    pub keys: Vec<String>,
}

impl SharedMachine {
    pub fn address(&self) -> String {
        format!("{}@{}:{}", self.user, self.host, self.port)
    }

    /// The fingerprint to trust for it: of its ed25519 key when it has one (what this device's
    /// ssh asks for first), else its first.
    pub fn fingerprint(&self) -> Option<String> {
        let keys: Vec<PublicKey> = self.keys.iter().filter_map(|k| PublicKey::from_openssh(k).ok()).collect();
        let key = keys.iter().find(|k| k.algorithm() == russh::keys::Algorithm::Ed25519).or(keys.first())?;
        Some(crate::ssh::fingerprint(key))
    }
}

/// What a pairing link holds.
#[derive(Debug)]
pub struct Link {
    pub id: String,
    pub key: PrivateKey,
    /// When it stops working (seconds since 1970).
    pub until: u64,
    pub machines: Vec<SharedMachine>,
}

/// Read a `serpis://pair#…` link.
pub fn parse(link: &str) -> Result<Link> {
    let data = link.trim().strip_prefix("serpis://pair#").context("not a Serpis pairing code")?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(data.trim_end_matches('=')).context("a damaged pairing code")?;
    let v: Value = serde_json::from_slice(&bytes).context("a damaged pairing code")?;
    if v["v"].as_u64() != Some(1) {
        bail!("this pairing code is from a newer serpis: update the app");
    }
    let id = v["p"].as_str().filter(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric())).context("a damaged pairing code")?.to_string();
    let body = v["k"].as_str().context("a damaged pairing code")?;
    let lines: Vec<&str> = body.as_bytes().chunks(70).map(|c| std::str::from_utf8(c).unwrap_or("")).collect();
    let pem = format!("-----BEGIN OPENSSH PRIVATE KEY-----\n{}\n-----END OPENSSH PRIVATE KEY-----\n", lines.join("\n"));
    let key = PrivateKey::from_openssh(&pem).context("a damaged pairing code")?;
    let machines = v["m"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            Some(SharedMachine {
                name: m["n"].as_str()?.to_string(),
                host: m["h"].as_str()?.to_string(),
                port: m["p"].as_u64().and_then(|p| u16::try_from(p).ok()).unwrap_or(22),
                user: m["u"].as_str()?.to_string(),
                keys: m["k"].as_array()?.iter().filter_map(|k| k.as_str().map(str::to_string)).collect(),
            })
        })
        .filter(|m| !m.keys.is_empty())
        .collect::<Vec<_>>();
    if machines.is_empty() {
        bail!("this pairing code shares no machine");
    }
    Ok(Link { id, key, until: v["x"].as_u64().unwrap_or(0), machines })
}

/// What the machine's `serpis-node pair` said: approved, or why not.
pub fn answer(line: &str) -> Result<bool> {
    let v: Value = serde_json::from_str(line.trim()).with_context(|| format!("an answer the app can't read: {}", line.trim()))?;
    if let Some(e) = v["error"].as_str() {
        bail!("{e}");
    }
    v["approved"].as_bool().context("an answer the app can't read")
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::keys::ssh_key::rand_core::UnwrapErr;

    fn link_for(machines: Value, version: u64) -> String {
        let key = PrivateKey::random(&mut UnwrapErr(getrandom::SysRng), russh::keys::Algorithm::Ed25519).unwrap();
        let pem = key.to_openssh(russh::keys::ssh_key::LineEnding::LF).unwrap();
        let body: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
        let v = serde_json::json!({"v": version, "p": "abc123", "k": body, "x": 1_800_000_000u64, "m": machines});
        format!("serpis://pair#{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string()))
    }

    #[test]
    fn a_link_gives_machines_their_keys_and_the_pairing_key() {
        let host = PrivateKey::random(&mut UnwrapErr(getrandom::SysRng), russh::keys::Algorithm::Ed25519).unwrap();
        let hk = host.public_key().to_openssh().unwrap();
        let l = parse(&link_for(serde_json::json!([{"n": "box", "h": "box.local", "p": 2222, "u": "me", "k": [hk]}, {"n": "nokeys", "h": "x", "p": 22, "u": "me", "k": []}]), 1)).unwrap();
        assert_eq!((l.id.as_str(), l.until), ("abc123", 1_800_000_000));
        assert_eq!(l.machines.len(), 1, "a machine without a trusted host key isn't taken");
        assert_eq!(l.machines[0].address(), "me@box.local:2222");
        assert_eq!(l.machines[0].fingerprint().unwrap(), crate::ssh::fingerprint(host.public_key()));
        assert!(parse("https://example.com").is_err());
        assert!(parse(&link_for(serde_json::json!([]), 1)).is_err());
        assert!(parse(&link_for(serde_json::json!([]), 2)).unwrap_err().to_string().contains("newer"));
    }

    #[test]
    fn answers() {
        assert!(answer("{\"approved\":true}\n").unwrap());
        assert!(!answer("{\"approved\":false}").unwrap());
        assert_eq!(answer("{\"error\":\"this pairing is over: scan a new code\"}").unwrap_err().to_string(), "this pairing is over: scan a new code");
    }
}
