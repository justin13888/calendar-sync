//! `cargo xtask fixture purity|deny`: the gates reject what they exist to reject.
//!
//! Each fixture is an excluded crate under `tests/fixtures/` that must fail its gate. A gate that
//! stops failing on its fixture has stopped enforcing its rule, so these commands fail then.

use std::path::Path;

use crate::{Result, cargo, output};

/// `tests/fixtures/purity` reads the clock and the filesystem; clippy, with the workspace
/// `clippy.toml`, must reject both with `disallowed_methods`.
pub(crate) fn purity(root: &Path) -> Result {
    let fixture = root.join("tests/fixtures/purity");
    let mut clippy = cargo();
    clippy.args(["clippy", "--quiet", "--manifest-path"]);
    clippy.arg(fixture.join("Cargo.toml"));
    clippy.args(["--", "-D", "clippy::disallowed_methods"]);
    let out = output(&mut clippy, root)?;
    expect_rejection(
        "clippy on the purity fixture",
        out.status.success(),
        &String::from_utf8_lossy(&out.stderr),
        &[
            "#disallowed_methods",
            "use of a disallowed method `std::time::SystemTime::now`",
            "use of a disallowed method `std::fs::read_to_string`",
        ],
    )
}

/// `tests/fixtures/deny` depends on a GPL-3.0 crate; `cargo deny check licenses`, with the
/// workspace `deny.toml`, must reject it.
pub(crate) fn deny(root: &Path) -> Result {
    let fixture = root.join("tests/fixtures/deny");
    let mut deny = cargo();
    deny.args(["deny", "--manifest-path"]);
    deny.arg(fixture.join("Cargo.toml"));
    deny.arg("--config");
    deny.arg(root.join("deny.toml"));
    deny.args(["check", "licenses"]);
    let out = output(&mut deny, root)?;
    expect_rejection(
        "cargo deny on the deny fixture",
        out.status.success(),
        &String::from_utf8_lossy(&out.stderr),
        &["rejected", "GPL-3.0-only"],
    )
}

/// The gate must have failed, and its output must name every expected fragment, so a failure for
/// an unrelated reason (a build error, a missing tool) does not pass for a rejection.
fn expect_rejection(gate: &str, succeeded: bool, stderr: &str, expected: &[&str]) -> Result {
    if succeeded {
        return Err(format!("{gate} passed, but it must fail:\n{stderr}"));
    }
    let missing: Vec<&str> = expected
        .iter()
        .copied()
        .filter(|fragment| !stderr.contains(fragment))
        .collect();
    if missing.is_empty() {
        println!("fixture: {gate} failed as required");
        Ok(())
    } else {
        Err(format!(
            "{gate} failed, but its output lacks {missing:?}:\n{stderr}"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_passing_gate_is_an_error() {
        assert!(expect_rejection("g", true, "rejected", &["rejected"]).is_err());
    }

    #[test]
    fn a_failure_for_another_reason_is_an_error() {
        assert!(expect_rejection("g", false, "error: could not compile", &["rejected"]).is_err());
    }

    #[test]
    fn the_expected_rejection_passes() {
        assert!(expect_rejection("g", false, "error: GPL rejected", &["rejected", "GPL"]).is_ok());
    }
}
