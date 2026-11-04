//! The corpus check docs/adr/0001-icalendar-crate.md asks of this crate, over corpora kept out of
//! the tree.
//!
//! libical's `test-data` and ical.js's `samples` and `test/parser` are copyleft (`LGPL-2.1-only OR
//! MPL-2.0`, `MPL-2.0`), so they are never vendored: this test reads them from the directory
//! `CALENDAR_SYNC_ICAL_CORPORA` names, and is ignored unless asked for. To run it:
//!
//! ```sh
//! git clone https://github.com/libical/libical target/corpora/libical
//! git -C target/corpora/libical checkout 1fa3287ac9009b7447b341ff1dabbd769965a1cf
//! git clone https://github.com/kewisch/ical.js target/corpora/ical.js
//! git -C target/corpora/ical.js checkout 7bc16367692451cea30b2751f4ed5532d709d402
//! CALENDAR_SYNC_ICAL_CORPORA=$PWD/target/corpora \
//!   cargo test -p calendar-sync-ical --test corpora -- --ignored --nocapture
//! ```
//!
//! For every `*.ics` file it checks three things:
//!
//! 1. Parsing and serializing returns the file byte for byte.
//! 2. Writing back every series read changes no byte.
//! 3. Retitling the first series that has a titled master changes that one property and nothing
//!    else.

// This test reads files and its environment: it is the one place in the crate that does.
#![allow(clippy::disallowed_methods)]

use std::path::{Path, PathBuf};

use calendar_sync_ical::{IcalDocument, Source};
use jiff::Timestamp;

fn ics_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            ics_files(&path, out);
        } else if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("ics"))
        {
            out.push(path);
        }
    }
}

/// The logical lines of a calendar: folds joined, line endings dropped.
fn logical_lines(bytes: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes)
        .replace("\r\n ", "")
        .replace("\r\n\t", "")
        .replace("\n ", "")
        .replace("\n\t", "");
    text.lines().map(str::to_owned).collect()
}

/// Retitles the first titled master, and returns whether there was one.
fn retitle(document: &mut IcalDocument) -> bool {
    let read = document.read(&Source::feed());
    let Some(mut series) = read
        .series
        .into_iter()
        .map(|read| read.series)
        .find(|series| {
            series
                .master
                .as_ref()
                .is_some_and(|master| master.fields.title.is_some())
        })
    else {
        return false;
    };
    series.master.as_mut().unwrap().fields.title = Some("Retitled by the corpus check".to_owned());
    document
        .write(&series, Timestamp::UNIX_EPOCH)
        .expect("a series read has a UID");
    true
}

#[test]
#[ignore = "needs the corpora checked out; see the module documentation"]
fn corpora_round_trip_and_edit_locally() {
    let root = std::env::var_os("CALENDAR_SYNC_ICAL_CORPORA")
        .expect("CALENDAR_SYNC_ICAL_CORPORA names the directory holding the corpora");
    let mut files = Vec::new();
    ics_files(Path::new(&root), &mut files);
    files.sort();
    assert!(!files.is_empty(), "no .ics files under {root:?}");

    let mut failures = Vec::new();
    let mut edited = 0;
    for file in &files {
        let bytes = std::fs::read(file).unwrap();
        let mut document = IcalDocument::parse(&bytes);
        if document.to_bytes() != bytes {
            failures.push(format!("{}: parse does not round trip", file.display()));
            continue;
        }
        for series in document.read(&Source::feed()).series {
            let _ = document.write(&series.series, Timestamp::UNIX_EPOCH);
        }
        if document.to_bytes() != bytes {
            failures.push(format!("{}: writing back changed bytes", file.display()));
            continue;
        }
        if retitle(&mut document) {
            edited += 1;
            let before = logical_lines(&bytes);
            let after = logical_lines(&document.to_bytes());
            let changed: Vec<(&String, &String)> = before
                .iter()
                .zip(&after)
                .filter(|(before, after)| before != after)
                .collect();
            let local = before.len() == after.len()
                && changed.len() == 1
                && changed[0]
                    .1
                    .to_ascii_uppercase()
                    .split([';', ':'])
                    .next()
                    .is_some_and(|name| name.rsplit('.').next() == Some("SUMMARY"));
            if !local {
                failures.push(format!(
                    "{}: the edit is not local: {changed:?}",
                    file.display()
                ));
            }
        }
    }
    println!(
        "{} files, {} edited, {} failures",
        files.len(),
        edited,
        failures.len()
    );
    assert!(failures.is_empty(), "{failures:#?}");
}
