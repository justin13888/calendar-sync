//! `cargo xtask toolchain`: the toolchain in use is the pinned one, and the MSRV is the agreed one.

use std::path::Path;
use std::process::Command;

use crate::{Result, output};

/// The MSRV calendar-sync supports. Sunrise pins Rust 1.91.1 and consumes these crates.
const MSRV: &str = "1.91";

pub(crate) fn run(root: &Path) -> Result {
    let toolchain = read(&root.join("rust-toolchain.toml"))?;
    let channel = toml_string(&toolchain, "channel")
        .ok_or("rust-toolchain.toml has no `channel = \"...\"` line")?;

    let rustc = output(Command::new("rustc").arg("-V"), root)?;
    let version = String::from_utf8_lossy(&rustc.stdout);
    check_rustc(&version, channel)?;

    let manifest = read(&root.join("Cargo.toml"))?;
    let rust_version =
        toml_string(&manifest, "rust-version").ok_or("Cargo.toml has no `rust-version` line")?;
    if rust_version != MSRV {
        return Err(format!(
            "Cargo.toml rust-version is {rust_version}, expected the MSRV {MSRV}"
        ));
    }

    let mise = read(&root.join("mise.toml"))?;
    let mise_msrv = toml_string(&mise, "MSRV").ok_or("mise.toml has no `MSRV` env entry")?;
    if mise_msrv != MSRV {
        return Err(format!(
            "mise.toml MSRV is {mise_msrv}, expected the MSRV {MSRV}"
        ));
    }

    println!("toolchain: rustc {channel}, rust-version {MSRV}");
    Ok(())
}

fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).map_err(|e| format!("could not read {}: {e}", path.display()))
}

/// `rustc -V` prints `rustc 1.99.1 (hash date)`; the version must equal the pinned channel.
fn check_rustc(version: &str, channel: &str) -> Result {
    match version.split_whitespace().nth(1) {
        Some(found) if found == channel => Ok(()),
        _ => Err(format!(
            "rustc is `{}`, but rust-toolchain.toml pins {channel}",
            version.trim()
        )),
    }
}

/// The value of the first `key = "value"` line, ignoring comments. Enough for the flat keys read
/// here, without a TOML dependency.
fn toml_string<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|line| {
        let (k, v) = line.split_once('=')?;
        if k.trim() != key {
            return None;
        }
        v.trim().strip_prefix('"')?.split('"').next()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_flat_string_keys() {
        let text = "# channel = \"no\"\n[toolchain]\nchannel = \"1.99.1\" # pinned\n";
        assert_eq!(toml_string(text, "channel"), Some("1.99.1"));
        assert_eq!(toml_string(text, "missing"), None);
    }

    #[test]
    fn rustc_must_match_channel_exactly() {
        assert!(check_rustc("rustc 1.99.1 (abc 2026-08-27)\n", "1.99.1").is_ok());
        assert!(check_rustc("rustc 1.99.10 (abc 2026-08-27)\n", "1.99.1").is_err());
        assert!(check_rustc("rustc 1.98.0 (abc 2026-08-27)\n", "1.99.1").is_err());
    }
}
