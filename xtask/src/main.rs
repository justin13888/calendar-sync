//! Repository automation for calendar-sync. Run it as `cargo xtask <command>`.
//!
//! Commands:
//!
//! - `toolchain`: rustc matches `rust-toolchain.toml`, and `rust-version` matches the MSRV.
//! - `purity`: the pure crates' normal dependency graphs and sources hold no I/O, clock or RNG.
//! - `fixture purity`: clippy rejects the purity fixture's clock and filesystem reads.
//! - `fixture deny`: cargo-deny rejects the deny fixture's copyleft dependency.
//! - `references verify`: the vendored specs match `references/SHA256SUMS` and its README, offline.
//! - `references upstream`: the vendored specs still match their sources, downloaded again.
//! - `gen-windows-zones [--check]`: the Graph crate's Windows time zone table, generated from the
//!   vendored CLDR data; `--check` fails if the checked-in table differs.

mod fixture;
mod gen_windows_zones;
mod purity;
mod references;
mod toolchain;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Output};

const USAGE: &str = "usage: cargo xtask <toolchain | purity | fixture purity | fixture deny | \
                     references verify | references upstream | gen-windows-zones [--check]>";

/// An error message for the user. Every check reports failure through this.
type Result<T = ()> = std::result::Result<T, String>;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let root = workspace_root();
    let outcome = match args.as_slice() {
        ["toolchain"] => toolchain::run(&root),
        ["purity"] => purity::run(&root),
        ["fixture", "purity"] => fixture::purity(&root),
        ["fixture", "deny"] => fixture::deny(&root),
        ["references", "verify"] => references::verify(&root),
        ["references", "upstream"] => references::upstream(&root),
        ["gen-windows-zones"] => gen_windows_zones::run(&root, false),
        ["gen-windows-zones", "--check"] => gen_windows_zones::run(&root, true),
        _ => Err(USAGE.to_owned()),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("xtask: {message}");
            ExitCode::FAILURE
        }
    }
}

/// The workspace root: the parent of this crate's manifest directory.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root")
        .to_path_buf()
}

/// The cargo that invoked us, so nested runs use the same toolchain.
fn cargo() -> Command {
    Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
}

/// Runs `command` in `dir` and captures its output.
fn output(command: &mut Command, dir: &Path) -> Result<Output> {
    command
        .current_dir(dir)
        .output()
        .map_err(|e| format!("could not run {command:?}: {e}"))
}
