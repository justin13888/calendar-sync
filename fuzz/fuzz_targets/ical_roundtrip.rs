//! Parses arbitrary bytes as iCalendar and writes them back.
//!
//! Beyond not panicking, the document must keep every byte, and writing back every series it
//! read must change none of them. Retitling the first titled master, a real edit, must not panic
//! either.

#![no_main]

use calendar_sync_ical::{IcalDocument, Source};
use jiff::Timestamp;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|bytes: &[u8]| {
    let stamp = Timestamp::UNIX_EPOCH;

    let mut document = IcalDocument::parse(bytes);
    assert_eq!(document.to_bytes(), bytes, "parsing lost or changed bytes");

    let read = document.read(&Source::feed());
    for series in &read.series {
        // A series with no event left to carry a UID cannot be written; that is not a change.
        let _ = document.write(&series.series, stamp);
    }
    assert_eq!(
        document.to_bytes(),
        bytes,
        "writing back what was read changed the bytes"
    );

    let titled = read.series.into_iter().find_map(|series| {
        let mut series = series.series;
        let master = series.master.as_mut()?;
        master.fields.title.as_ref()?;
        master.fields.title = Some("Retitled".to_owned());
        Some(series)
    });
    if let Some(series) = titled {
        let _ = document.write(&series, stamp);
        let _ = document.to_bytes();
    }
});
