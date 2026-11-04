//! `cargo xtask purity`: the pure crates pull in no I/O, clock or RNG, by dependency or by source.
//!
//! This is the second of three layers. The first is `clippy.toml`'s `disallowed-methods` and
//! `disallowed-types`; the third is the fixture crates `cargo xtask fixture` proves are rejected.

use std::path::{Path, PathBuf};

use crate::{Result, cargo, output};

/// Crates that read no clock, filesystem, environment or ambient randomness, and build for wasm.
pub(crate) const PURE_CRATES: &[&str] = &[
    "calendar-sync-model",
    "calendar-sync-recurrence",
    "calendar-sync-engine",
    "calendar-sync-ical",
];

/// Crates that must not appear in a pure crate's normal dependency graph on any target.
const BANNED_DEPENDENCIES: &[&str] = &["tokio", "hyper", "reqwest", "libdav", "getrandom", "rand"];

/// Source fragments that read the clock, filesystem, environment or ambient randomness.
const BANNED_SOURCE: &[&str] = &[
    "SystemTime::now",
    "Instant::now",
    "Timestamp::now",
    "Zoned::now",
    "TimeZone::system",
    "TimeZone::try_system",
    "std::env",
    "std::fs",
    "thread_rng",
    "rand::",
    "getrandom",
    "OsRng",
];

pub(crate) fn run(root: &Path) -> Result {
    let mut violations = Vec::new();
    for krate in PURE_CRATES {
        violations.extend(dependency_violations(root, krate)?);
        violations.extend(source_violations(
            &root.join("crates").join(krate).join("src"),
        )?);
    }
    if violations.is_empty() {
        println!("purity: {} pure crates clean", PURE_CRATES.len());
        Ok(())
    } else {
        Err(format!("purity violations:\n  {}", violations.join("\n  ")))
    }
}

fn dependency_violations(root: &Path, krate: &str) -> Result<Vec<String>> {
    let mut tree = cargo();
    tree.args([
        "tree",
        "--package",
        krate,
        "--edges",
        "normal",
        "--target",
        "all",
        "--prefix",
        "none",
        "--format",
        "{p}",
    ]);
    let out = output(&mut tree, root)?;
    if !out.status.success() {
        return Err(format!(
            "cargo tree for {krate} failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(banned_dependencies(&String::from_utf8_lossy(&out.stdout))
        .into_iter()
        .map(|dep| format!("{krate} depends on {dep}"))
        .collect())
}

/// The banned crate names in `cargo tree --prefix none --format {p}` output, one per line, each
/// line starting with the crate name.
fn banned_dependencies(tree: &str) -> Vec<&str> {
    let mut found: Vec<&str> = tree
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| BANNED_DEPENDENCIES.contains(name))
        .collect();
    found.sort_unstable();
    found.dedup();
    found
}

fn source_violations(dir: &Path) -> Result<Vec<String>> {
    let mut violations = Vec::new();
    for file in rust_files(dir)? {
        let text = std::fs::read_to_string(&file)
            .map_err(|e| format!("could not read {}: {e}", file.display()))?;
        for (number, line) in text.lines().enumerate() {
            if let Some(fragment) = banned_fragment(line) {
                violations.push(format!(
                    "{}:{}: uses `{fragment}`",
                    file.display(),
                    number + 1
                ));
            }
        }
    }
    Ok(violations)
}

fn rust_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let entries = std::fs::read_dir(&dir)
            .map_err(|e| format!("could not list {}: {e}", dir.display()))?;
        for entry in entries {
            let path = entry
                .map_err(|e| format!("could not list {}: {e}", dir.display()))?
                .path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

/// The first banned fragment on a line of code. Comment lines are skipped, and a fragment only
/// matches at an identifier boundary, so `operand::` is not `rand::`.
fn banned_fragment(line: &str) -> Option<&'static str> {
    if line.trim_start().starts_with("//") {
        return None;
    }
    BANNED_SOURCE.iter().copied().find(|fragment| {
        line.match_indices(fragment).any(|(at, _)| {
            !line[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_banned_dependencies_by_exact_name() {
        let tree =
            "calendar-sync-model v0.1.0 (/w)\nrand_core v0.9.0\ntokio v1.0.0\ntokio v1.0.0 (*)\n";
        assert_eq!(banned_dependencies(tree), vec!["tokio"]);
        assert!(banned_dependencies("calendar-sync-model v0.1.0 (/w)\n").is_empty());
    }

    #[test]
    fn flags_clock_fs_env_and_rng_reads() {
        assert_eq!(
            banned_fragment("let t = jiff::Timestamp::now();"),
            Some("Timestamp::now")
        );
        assert_eq!(
            banned_fragment("    let t = SystemTime::now();"),
            Some("SystemTime::now")
        );
        assert_eq!(banned_fragment("use std::fs::read;"), Some("std::fs"));
        assert_eq!(banned_fragment("std::env::var(\"X\")"), Some("std::env"));
        assert_eq!(
            banned_fragment("let x: u8 = rand::random();"),
            Some("rand::")
        );
        assert_eq!(
            banned_fragment("let tz = TimeZone::system();"),
            Some("TimeZone::system")
        );
    }

    #[test]
    fn ignores_comments_and_partial_identifiers() {
        assert_eq!(banned_fragment("/// Never calls `SystemTime::now`."), None);
        assert_eq!(banned_fragment("// std::fs is banned here"), None);
        assert_eq!(banned_fragment("let x = operand::value();"), None);
        assert_eq!(banned_fragment("let t = event.start_instant();"), None);
    }
}
