//! `cargo xtask references verify|upstream`: the vendored specs under `references/` are the ones
//! `references/README.md` documents, byte for byte.
//!
//! `references/README.md` holds one table per group. Each row's `File` cell names a vendored file
//! by its path below `references/`, its `Source` cell the URL it was downloaded from, and its
//! `Check` cell how `upstream` treats a difference: `immutable` fails, `snapshot` only reports.
//! `references/SHA256SUMS` lists every vendored file in `sha256sum -c` format.
//!
//! - `verify` is offline. Every checksum matches, every file under `references/` is listed in
//!   `SHA256SUMS`, and the README and `SHA256SUMS` name the same set of files.
//! - `upstream` downloads every file from its source again and compares it with its checksum.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use crate::{Result, output};

/// The two files in `references/` that describe the vendored files rather than being one.
const README: &str = "README.md";
const SUMS: &str = "SHA256SUMS";

/// `cargo xtask references verify`.
pub(crate) fn verify(root: &Path) -> Result {
    let count = verify_dir(&root.join("references"))?;
    println!("references: {count} vendored files verified");
    Ok(())
}

/// `cargo xtask references upstream`.
pub(crate) fn upstream(root: &Path) -> Result {
    let dir = root.join("references");
    let entries = parse_readme(&read_text(&dir.join(README))?)?;
    let sums = parse_sums(&read_text(&dir.join(SUMS))?)?;
    let mut failures = Vec::new();
    for entry in &entries {
        let Some(expected) = sums.get(&entry.path) else {
            failures.push(format!("{}: not listed in {SUMS}", entry.path));
            continue;
        };
        let fresh = match download(&entry.source, root) {
            Ok(bytes) => bytes,
            Err(e) => {
                failures.push(format!("{}: {e}", entry.path));
                continue;
            }
        };
        if &sha256_hex(&fresh) == expected {
            println!("upstream: {} unchanged", entry.path);
            continue;
        }
        match entry.check {
            Check::Immutable => failures.push(format!(
                "{}: {} no longer matches its checksum, but the source is pinned",
                entry.path, entry.source
            )),
            Check::Snapshot => {
                let vendored = std::fs::read(dir.join(&entry.path))
                    .map_err(|e| format!("could not read {}: {e}", entry.path))?;
                println!(
                    "upstream: {}",
                    snapshot_drift(&entry.path, &vendored, &fresh)
                );
            }
        }
    }
    if failures.is_empty() {
        println!("upstream: {} sources checked", entries.len());
        Ok(())
    } else {
        Err(format!("upstream mismatches:\n  {}", failures.join("\n  ")))
    }
}

/// How `upstream` treats a vendored file that no longer matches its source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Check {
    /// The source is pinned (a version, tag or commit); any difference fails.
    Immutable,
    /// The source is a moving document vendored as a dated snapshot; a difference is reported.
    Snapshot,
}

/// One row of a `references/README.md` table.
#[derive(Debug, PartialEq, Eq)]
struct Entry {
    path: String,
    source: String,
    check: Check,
}

/// Checks `dir` (a `references/` directory) and returns how many vendored files it holds.
fn verify_dir(dir: &Path) -> Result<usize> {
    let entries = parse_readme(&read_text(&dir.join(README))?)?;
    let sums = parse_sums(&read_text(&dir.join(SUMS))?)?;
    let files = vendored_files(dir)?;
    let mut problems = Vec::new();

    for (path, expected) in &sums {
        match std::fs::read(dir.join(path)) {
            Ok(bytes) if &sha256_hex(&bytes) == expected => {}
            Ok(_) => problems.push(format!("{path}: checksum does not match {SUMS}")),
            Err(e) => problems.push(format!("{path}: listed in {SUMS}, but unreadable: {e}")),
        }
    }
    for path in &files {
        if !sums.contains_key(path) {
            problems.push(format!("{path}: vendored, but not listed in {SUMS}"));
        }
    }

    let documented: BTreeSet<&str> = entries.iter().map(|e| e.path.as_str()).collect();
    for path in sums.keys() {
        if !documented.contains(path.as_str()) {
            problems.push(format!(
                "{path}: listed in {SUMS}, but not documented in {README}"
            ));
        }
    }
    for path in &documented {
        if !sums.contains_key(*path) {
            problems.push(format!(
                "{path}: documented in {README}, but not listed in {SUMS}"
            ));
        }
    }

    if problems.is_empty() {
        Ok(sums.len())
    } else {
        Err(format!(
            "references/ does not match its manifest:\n  {}",
            problems.join("\n  ")
        ))
    }
}

/// Every file below `dir`, as a `/`-separated path relative to it, except the README and sums.
fn vendored_files(dir: &Path) -> Result<BTreeSet<String>> {
    let mut files = BTreeSet::new();
    let mut pending = vec![(dir.to_path_buf(), String::new())];
    while let Some((path, prefix)) = pending.pop() {
        let listing = std::fs::read_dir(&path)
            .map_err(|e| format!("could not list {}: {e}", path.display()))?;
        for item in listing {
            let item = item.map_err(|e| format!("could not list {}: {e}", path.display()))?;
            let name = item.file_name().to_string_lossy().into_owned();
            let relative = format!("{prefix}{name}");
            let kind = item
                .file_type()
                .map_err(|e| format!("could not stat {relative}: {e}"))?;
            if kind.is_dir() {
                pending.push((item.path(), format!("{relative}/")));
            } else if relative != README && relative != SUMS {
                files.insert(relative);
            }
        }
    }
    Ok(files)
}

/// Parses `sha256sum` output: `<64 hex digits>  <path>`, or ` *<path>` in binary mode.
fn parse_sums(text: &str) -> Result<BTreeMap<String, String>> {
    let mut sums = BTreeMap::new();
    for (number, line) in text.lines().enumerate().map(|(i, l)| (i + 1, l)) {
        if line.trim().is_empty() {
            continue;
        }
        let bad = || format!("{SUMS} line {number} is not `<sha256>  <path>`: {line}");
        let (hash, rest) = line.split_at_checked(64).ok_or_else(bad)?;
        let path = rest
            .strip_prefix("  ")
            .or_else(|| rest.strip_prefix(" *"))
            .ok_or_else(bad)?;
        if !hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) || path.is_empty() {
            return Err(bad());
        }
        if sums.insert(path.to_owned(), hash.to_owned()).is_some() {
            return Err(format!("{SUMS} lists {path} twice"));
        }
    }
    Ok(sums)
}

/// Reads every table in the README whose header row has `File`, `Source` and `Check` columns.
fn parse_readme(text: &str) -> Result<Vec<Entry>> {
    let mut entries: Vec<Entry> = Vec::new();
    let mut columns: Option<(usize, usize, usize)> = None;
    for line in text.lines() {
        let Some(cells) = table_cells(line) else {
            columns = None;
            continue;
        };
        if cells
            .iter()
            .all(|c| c.chars().all(|ch| matches!(ch, '-' | ':')))
        {
            continue;
        }
        let Some((file, source, check)) = columns else {
            let find = |name: &str| cells.iter().position(|c| *c == name);
            columns = find("File")
                .zip(find("Source"))
                .zip(find("Check"))
                .map(|((f, s), c)| (f, s, c));
            continue;
        };
        let cell = |i: usize| cells.get(i).copied().unwrap_or_default();
        let path = unwrap_code(cell(file))
            .ok_or_else(|| format!("{README}: a File cell is not a `path`: {line}"))?;
        let source = cell(source)
            .strip_prefix('<')
            .and_then(|s| s.strip_suffix('>'))
            .ok_or_else(|| format!("{README}: {path}'s Source is not a <url>: {line}"))?;
        let check = match cell(check) {
            "immutable" => Check::Immutable,
            "snapshot" => Check::Snapshot,
            other => {
                return Err(format!(
                    "{README}: {path}'s Check is `{other}`, not immutable or snapshot"
                ));
            }
        };
        if entries.iter().any(|e| e.path == path) {
            return Err(format!("{README} documents {path} twice"));
        }
        entries.push(Entry {
            path: path.to_owned(),
            source: source.to_owned(),
            check,
        });
    }
    if entries.is_empty() {
        return Err(format!("{README} documents no files"));
    }
    Ok(entries)
}

/// The trimmed cells of a Markdown table row, or `None` for any other line.
fn table_cells(line: &str) -> Option<Vec<&str>> {
    let inner = line.trim().strip_prefix('|')?.strip_suffix('|')?;
    Some(inner.split('|').map(str::trim).collect())
}

/// `` `text` `` to `text`.
fn unwrap_code(cell: &str) -> Option<&str> {
    let inner = cell.strip_prefix('`')?.strip_suffix('`')?;
    (!inner.is_empty() && !inner.contains('`')).then_some(inner)
}

/// Describes how a snapshot differs from its source. Where both carry a JSON `"revision"` (the
/// Google discovery document does), the revisions are the drift: Google serves the document with
/// its keys in a varying order, so its bytes differ on every download.
fn snapshot_drift(path: &str, vendored: &[u8], fresh: &[u8]) -> String {
    let old = json_revision(&String::from_utf8_lossy(vendored));
    let new = json_revision(&String::from_utf8_lossy(fresh));
    match (old, new) {
        (Some(old), Some(new)) if old == new => {
            format!("{path} snapshot is current (revision {old}; only the bytes differ)")
        }
        (Some(old), Some(new)) => {
            format!("{path} snapshot drifted: vendored revision {old}, upstream revision {new}")
        }
        _ => format!("{path} snapshot drifted: upstream bytes differ"),
    }
}

/// The value of the first `"revision": "..."` member, without a JSON dependency.
fn json_revision(text: &str) -> Option<String> {
    let after = &text[text.find("\"revision\"")? + "\"revision\"".len()..];
    let value = after
        .trim_start()
        .strip_prefix(':')?
        .trim_start()
        .strip_prefix('"')?;
    Some(value[..value.find('"')?].to_owned())
}

/// Downloads `url` with curl, failing on an HTTP error.
fn download(url: &str, root: &Path) -> Result<Vec<u8>> {
    let mut curl = Command::new("curl");
    curl.args([
        "--fail",
        "--silent",
        "--show-error",
        "--location",
        "--retry",
        "3",
    ]);
    curl.arg(url);
    let out = output(&mut curl, root)?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(format!(
            "could not download {url}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

fn read_text(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).map_err(|e| format!("could not read {}: {e}", path.display()))
}

/// The SHA-256 digest of `data` as lowercase hex (FIPS 180-4). Implemented here so xtask stays
/// free of dependencies; the tests pin it to the standard's test vectors.
fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());

    // The padding above makes the message a whole number of blocks, so no remainder is dropped.
    for block in message.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (word, bytes) in w.iter_mut().zip(block.as_chunks::<4>().0) {
            *word = u32::from_be_bytes(*bytes);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for (k, w) in K.iter().zip(w) {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(*k)
                .wrapping_add(w);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (state, add) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *state = state.wrapping_add(add);
        }
    }

    h.iter().map(|word| format!("{word:08x}")).collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn sha256_matches_the_standard_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            sha256_hex(&vec![b'a'; 1_000_000]),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn parses_sha256sum_output() {
        let a = "a".repeat(64);
        let b = "b".repeat(64);
        let sums = parse_sums(&format!("{a}  rfc/x.txt\n{b} *y.bin\n\n")).unwrap();
        assert_eq!(sums.get("rfc/x.txt"), Some(&a));
        assert_eq!(sums.get("y.bin"), Some(&b));
    }

    #[test]
    fn rejects_malformed_or_duplicate_sums() {
        let a = "a".repeat(64);
        assert!(parse_sums("abc  x\n").is_err());
        assert!(parse_sums(&format!("{a} x\n")).is_err());
        assert!(parse_sums(&format!("{}  x\n", "A".repeat(64))).is_err());
        assert!(parse_sums(&format!("{a}  x\n{a}  x\n")).is_err());
    }

    const README_TEXT: &str = "\
# References

Prose | with a pipe is not a table.

| File | Pin | Source | Check | Why |
|---|---|---|:-:|---|
| `rfc/a.txt` | RFC 1 | <https://example.org/a.txt> | immutable | because |
| `google/b.json` | rev 1 | <https://example.org/b> | snapshot | drift |

| Other | Table |
|---|---|
| `not/a/file` | ignored |
";

    #[test]
    fn reads_the_readme_tables() {
        let entries = parse_readme(README_TEXT).unwrap();
        assert_eq!(
            entries,
            vec![
                Entry {
                    path: "rfc/a.txt".into(),
                    source: "https://example.org/a.txt".into(),
                    check: Check::Immutable,
                },
                Entry {
                    path: "google/b.json".into(),
                    source: "https://example.org/b".into(),
                    check: Check::Snapshot,
                },
            ]
        );
    }

    #[test]
    fn rejects_a_malformed_readme_row() {
        let bad_check = README_TEXT.replace("| immutable |", "| sometimes |");
        assert!(parse_readme(&bad_check).is_err());
        let bad_source = README_TEXT.replace("<https://example.org/a.txt>", "example.org");
        assert!(parse_readme(&bad_source).is_err());
        assert!(parse_readme("# Nothing here\n").is_err());
    }

    #[test]
    fn snapshot_drift_compares_json_revisions() {
        let old = br#"{"kind": "x", "revision": "20261030", "id": "a"}"#;
        let reordered = br#"{"revision":"20261030","id":"a","kind":"x"}"#;
        let newer = br#"{"revision": "20261101"}"#;
        assert!(snapshot_drift("g", old, reordered).contains("is current"));
        assert!(snapshot_drift("g", old, newer).contains("20261101"));
        assert!(snapshot_drift("g", b"<html>", b"<html >").contains("bytes differ"));
    }

    /// A scratch `references/` directory, removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("xtask-references-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("rfc")).unwrap();
            std::fs::create_dir_all(dir.join("google")).unwrap();
            std::fs::write(dir.join("rfc/a.txt"), "RFC text\n").unwrap();
            std::fs::write(dir.join("google/b.json"), "{\"revision\": \"1\"}").unwrap();
            std::fs::write(dir.join(README), README_TEXT).unwrap();
            let sums = format!(
                "{}  google/b.json\n{}  rfc/a.txt\n",
                sha256_hex(b"{\"revision\": \"1\"}"),
                sha256_hex(b"RFC text\n")
            );
            std::fs::write(dir.join(SUMS), sums).unwrap();
            Self(dir)
        }

        fn write(&self, path: &str, contents: &str) {
            std::fs::write(self.0.join(path), contents).unwrap();
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_consistent_tree_verifies() {
        let scratch = Scratch::new("consistent");
        assert_eq!(verify_dir(&scratch.0), Ok(2));
    }

    #[test]
    fn one_changed_byte_fails() {
        let scratch = Scratch::new("byte");
        scratch.write("rfc/a.txt", "RFC texT\n");
        let error = verify_dir(&scratch.0).unwrap_err();
        assert!(
            error.contains("rfc/a.txt: checksum does not match"),
            "{error}"
        );
    }

    #[test]
    fn an_unlisted_file_fails() {
        let scratch = Scratch::new("unlisted");
        scratch.write("rfc/extra.txt", "stray\n");
        let error = verify_dir(&scratch.0).unwrap_err();
        assert!(
            error.contains("rfc/extra.txt: vendored, but not listed"),
            "{error}"
        );
    }

    #[test]
    fn a_missing_file_fails() {
        let scratch = Scratch::new("missing");
        std::fs::remove_file(scratch.0.join("rfc/a.txt")).unwrap();
        let error = verify_dir(&scratch.0).unwrap_err();
        assert!(
            error.contains("rfc/a.txt: listed in SHA256SUMS, but unreadable"),
            "{error}"
        );
    }

    #[test]
    fn readme_and_sums_must_name_the_same_files() {
        let scratch = Scratch::new("disagree");
        let readme = README_TEXT.replace("`google/b.json`", "`google/c.json`");
        scratch.write(README, &readme);
        let error = verify_dir(&scratch.0).unwrap_err();
        assert!(
            error.contains("google/b.json: listed in SHA256SUMS, but not documented"),
            "{error}"
        );
        assert!(
            error.contains("google/c.json: documented in README.md, but not listed"),
            "{error}"
        );
    }
}
