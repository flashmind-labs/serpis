//! The account's `~/.ssh/authorized_keys`, as pairing a phone changes it: a pairing key that
//! can only propose a key (`restrict`, the forced `serpis-node pair <id>`, an `expiry-time`), and
//! the phone's own key once the desktop lets it in. Nothing else in the file is touched.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// What marks a pairing key's line (`serpis-pairing <id>` at its end).
const PAIRING: &str = "serpis-pairing";

pub fn authorized_keys() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".ssh/authorized_keys")
}

/// A public key as `type base64`, checked: one line, a key type ssh knows, base64 only.
pub fn public_key(text: &str) -> Result<String> {
    let mut parts = text.split_whitespace();
    let (Some(kind), Some(data)) = (parts.next(), parts.next()) else { bail!("not a public key") };
    let kinds = ["ssh-ed25519", "ecdsa-sha2-nistp256", "ecdsa-sha2-nistp384", "ecdsa-sha2-nistp521", "sk-ssh-ed25519@openssh.com", "sk-ecdsa-sha2-nistp256@openssh.com", "ssh-rsa"];
    if !kinds.contains(&kind) {
        bail!("not a key type ssh takes: {kind}");
    }
    if data.len() > 2048 || !data.bytes().all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b)) {
        bail!("not a public key");
    }
    Ok(format!("{kind} {data}"))
}

/// A pairing id or a comment: letters, digits and a few marks, nothing a line could break on.
fn plain(s: &str, what: &str) -> Result<()> {
    if s.is_empty() || s.len() > 64 || !s.chars().all(|c| c.is_alphanumeric() || " -_.'’@".contains(c)) {
        bail!("not a usable {what}: {s:?}");
    }
    Ok(())
}

pub fn check_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 64 || !id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        bail!("not a pairing id: {id:?}");
    }
    Ok(())
}

/// The key in a line of authorized_keys (`type base64`), past its options.
fn key_of(line: &str) -> Option<String> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    fields.windows(2).find_map(|w| public_key(&format!("{} {}", w[0], w[1])).ok())
}

fn read(file: &Path) -> Result<String> {
    match std::fs::read_to_string(file) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", file.display())),
    }
}

/// Write the file again with `text`, owner-only, through a file beside it (a reader never sees
/// half of it).
fn write(file: &Path, text: &str) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let dir = file.parent().context("no folder")?;
    if !dir.exists() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    let tmp = dir.join(format!(".authorized_keys.serpis-{}", std::process::id()));
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, file).with_context(|| format!("writing {}", file.display()))
}

/// Let `key` in with `options` (empty: none) and `comment`, unless it is in already.
pub fn add(file: &Path, options: &str, key: &str, comment: &str) -> Result<bool> {
    let key = public_key(key)?;
    plain(comment, "comment")?;
    let mut text = read(file)?;
    if text.lines().any(|l| key_of(l).as_deref() == Some(&key) && !l.contains(PAIRING)) && !comment.starts_with(PAIRING) {
        return Ok(false);
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    let options = if options.is_empty() { String::new() } else { format!("{options} ") };
    text.push_str(&format!("{options}{key} {comment}\n"));
    write(file, &text)?;
    Ok(true)
}

/// Drop the lines of pairing `id` (None: of every pairing).
pub fn remove_pairing(file: &Path, id: Option<&str>) -> Result<()> {
    let text = read(file)?;
    let keep: Vec<&str> = text
        .lines()
        .filter(|l| {
            let mine = l.trim_end().rsplit_once(' ').is_some_and(|(rest, last)| rest.ends_with(PAIRING) && id.is_none_or(|id| last == id));
            !mine
        })
        .collect();
    if keep.len() != text.lines().count() {
        let mut out = keep.join("\n");
        if !out.is_empty() {
            out.push('\n');
        }
        write(file, &out)?;
    }
    Ok(())
}

/// The line that lets pairing `id`'s key in for `minutes`: only to run `serpis-node pair <id>`.
pub fn pairing_options(exe: &Path, id: &str, minutes: u32) -> Result<String> {
    check_id(id)?;
    let exe = exe.to_string_lossy();
    if exe.contains(['"', '\'', '\\', '\n']) {
        bail!("serpis-node's path can't go in authorized_keys: {exe}");
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    let until = utc_minute(now + 60 * minutes.clamp(1, 60) as u64);
    Ok(format!("restrict,command=\"'{exe}' pair {id}\",expiry-time=\"{until}Z\""))
}

/// `YYYYMMDDHHMM` in UTC for seconds since 1970 (days to a civil date: Howard Hinnant's
/// algorithm).
fn utc_minute(secs: u64) -> String {
    let (days, rest) = ((secs / 86400) as i64, secs % 86400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}{month:02}{day:02}{:02}{:02}", rest / 3600, rest % 3600 / 60)
}

/// Pairing `id` lets `key` in for `minutes`.
pub fn start_pairing(file: &Path, exe: &Path, id: &str, key: &str, minutes: u32) -> Result<()> {
    let options = pairing_options(exe, id, minutes)?;
    remove_pairing(file, Some(id))?;
    add(file, &options, key, &format!("{PAIRING} {id}"))?;
    Ok(())
}

/// The node to name in a pairing key's line: the link every node start points at its build, so
/// the line still works after an upgrade; else this one.
pub fn node_exe() -> PathBuf {
    let current = dirs::home_dir().unwrap_or_default().join(".serpis/bin/serpis-node-current");
    if current.exists() { current } else { std::env::current_exe().unwrap_or_default() }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAINm9mnRQLkdBuv6CDOCflHSvvxq6w1MpcbvPddiMhtV1";
    const OTHER: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGq3vDqLX3Yv1n0OQ9wq3zvJ6S7xq8hj1yYb2m8Y3cXx";

    #[test]
    fn pairing_lines_come_and_go_and_nothing_else_moves() {
        let dir = std::env::temp_dir().join(format!("ak-{}", uuid::Uuid::now_v7().simple()));
        let file = dir.join(".ssh/authorized_keys");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "# mine\nssh-rsa AAAAB3Nza me@laptop").unwrap();
        start_pairing(&file, Path::new("/home/me/.serpis/bin/serpis-node-current"), "abc123", OTHER, 5).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.starts_with("# mine\nssh-rsa AAAAB3Nza me@laptop\n"), "{text}");
        let line = text.lines().last().unwrap();
        assert!(line.starts_with("restrict,command=\"'/home/me/.serpis/bin/serpis-node-current' pair abc123\",expiry-time=\""), "{line}");
        assert!(line.ends_with(&format!("Z\" {OTHER} serpis-pairing abc123")), "{line}");

        // The phone's key, once (asked twice).
        assert!(add(&file, "", KEY, "serpis phone iPhone").unwrap());
        assert!(!add(&file, "", &format!("{KEY} again"), "serpis phone iPhone").unwrap());
        remove_pairing(&file, Some("abc123")).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert_eq!(text, format!("# mine\nssh-rsa AAAAB3Nza me@laptop\n{KEY} serpis phone iPhone\n"));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn utc_minutes() {
        assert_eq!(utc_minute(0), "197001010000");
        assert_eq!(utc_minute(1_793_068_830), "202610270240");
        assert_eq!(utc_minute(951_782_400), "200002290000");
    }

    #[test]
    fn only_keys_and_plain_names_get_in() {
        assert!(public_key(KEY).is_ok());
        assert!(public_key("ssh-ed25519 AAAA\"x").is_err());
        assert!(public_key("command=\"x\" ssh-ed25519 AAAA").is_err());
        assert!(public_key("ssh-ed25519").is_err());
        assert!(check_id("abc123").is_ok());
        assert!(check_id("a b").is_err());
        assert!(add(Path::new("/nonexistent/x"), "", KEY, "evil\nline").is_err());
    }
}
