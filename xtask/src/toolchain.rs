//! `cargo xtask toolchain`: the development toolchain floats on stable, and the MSRV is the agreed
//! one.

use std::path::Path;

use crate::Result;

/// The MSRV calendar-sync supports. Sunrise pins Rust 1.91.1 and consumes these crates.
const MSRV: &str = "1.91";

pub(crate) fn run(root: &Path) -> Result {
    let toolchain = read(&root.join("rust-toolchain.toml"))?;
    let channel = toml_string(&toolchain, "channel")
        .ok_or("rust-toolchain.toml has no `channel = \"...\"` line")?;
    check_channel(channel)?;

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

    println!("toolchain: channel {channel}, rust-version {MSRV}");
    Ok(())
}

fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).map_err(|e| format!("could not read {}: {e}", path.display()))
}

/// The development toolchain is the latest stable release, not a pinned one: Rust is
/// forward-compatible, so what a new release breaks is fixed in the repository when it lands.
/// The MSRV, not the channel, is what consumers rely on.
fn check_channel(channel: &str) -> Result {
    if channel == "stable" {
        Ok(())
    } else {
        Err(format!(
            "rust-toolchain.toml channel is `{channel}`, expected `stable`; the MSRV is \
             `rust-version` in Cargo.toml, not the development toolchain"
        ))
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
        let text = "# channel = \"no\"\n[toolchain]\nchannel = \"stable\" # floating\n";
        assert_eq!(toml_string(text, "channel"), Some("stable"));
        assert_eq!(toml_string(text, "missing"), None);
    }

    #[test]
    fn channel_must_be_stable() {
        assert!(check_channel("stable").is_ok());
        assert!(check_channel("1.99.1").is_err());
        assert!(check_channel("nightly").is_err());
        assert!(check_channel("nightly-2026-09-25").is_err());
    }
}
