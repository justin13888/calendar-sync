//! A `CalDAV` calendar object resource (RFC 4791 section 4.1) as one engine record.
//!
//! An object holds one `UID`: a single event, or a recurring series with its overrides, every
//! `VEVENT` in the one resource under one href and one `ETag`. The engine syncs resources, so an
//! object is one [`ProviderEvent`]: its master, or, for an invitation to one instance of someone
//! else's series, its first override. The object's whole text is kept in the record's passthrough
//! as [`OBJECT_FIELD`], so a write rewrites only the properties whose fields changed and every
//! other byte, overrides and unknown components included, goes back as it came. [`series`] reads
//! the whole series back out of a record.

use calendar_sync_ical::{IcalDocument, ReadSeries, Source, WriteError};
use calendar_sync_model::{
    EventSeries, InstanceKey, Loss, Override, Passthrough, ProviderEvent, ProviderRevision,
};
use jiff::Timestamp;

/// The passthrough field holding the object's text, exactly as the server returned it.
pub const OBJECT_FIELD: &str = "caldav.object";

/// The passthrough field holding the object's `Schedule-Tag` (RFC 6638 section 3.2.10), where the
/// server returned one.
pub const SCHEDULE_TAG_FIELD: &str = "caldav.schedule-tag";

/// The `PRODID` of an object this crate creates.
pub const PRODUCT_ID: &str = "-//calendar-sync//calendar-sync-caldav//EN";

/// The prefix of every passthrough field this crate adds. No iCalendar property name holds a `.`,
/// so none collides with a property the iCalendar mapping keeps.
const OWN_PREFIX: &str = "caldav.";

/// Reads the object at `href` into its record: the master, or the first override where the object
/// holds no master. `None` where it holds no `VEVENT`: a task, a journal entry, or nothing
/// readable. The record keeps the object's text as [`OBJECT_FIELD`] and `schedule_tag` as
/// [`SCHEDULE_TAG_FIELD`].
#[must_use]
pub fn read(
    href: &str,
    etag: Option<&str>,
    schedule_tag: Option<&str>,
    bytes: &[u8],
) -> Option<ProviderEvent> {
    let text = String::from_utf8_lossy(bytes);
    let document = IcalDocument::parse(text.as_bytes());
    let source = Source::caldav(href, etag.map(ProviderRevision::new));
    let found = document.read(&source).series.into_iter().next()?;
    let mut event = representative(found.series)?;
    event.passthrough.push(OBJECT_FIELD, text.into_owned());
    if let Some(tag) = schedule_tag {
        event.passthrough.push(SCHEDULE_TAG_FIELD, tag);
    }
    Some(event)
}

/// The whole series the object behind `event` holds, read from the text [`read`] kept, with the
/// losses reading it found. `None` where `event` keeps no object, or the object no longer holds
/// its `UID`.
#[must_use]
pub fn series(event: &ProviderEvent) -> Option<ReadSeries> {
    let text = object_text(event)?;
    let source = Source::caldav(event.ids.id.clone(), event.revision.clone());
    let uid = event.ids.ical_uid.as_deref();
    IcalDocument::parse(text.as_bytes())
        .read(&source)
        .series
        .into_iter()
        .find(|found| uid.is_none_or(|uid| found.uid == uid))
}

/// The object text kept in `event`'s passthrough.
#[must_use]
pub fn object_text(event: &ProviderEvent) -> Option<&str> {
    event.passthrough.get(OBJECT_FIELD).next()
}

/// The `Schedule-Tag` kept in `event`'s passthrough.
#[must_use]
pub fn schedule_tag(event: &ProviderEvent) -> Option<&str> {
    event.passthrough.get(SCHEDULE_TAG_FIELD).next()
}

/// The object text to `PUT` for `event`: the object it was read from with `event`'s fields
/// written into it, or a new object holding only `event`. `stamp` is the `DTSTAMP` of a `VEVENT`
/// the write adds.
///
/// # Errors
///
/// [`WriteError::NoUid`] when `event` carries no iCalendar `UID`.
pub fn write(event: &ProviderEvent, stamp: Timestamp) -> Result<(String, Vec<Loss>), WriteError> {
    let (mut document, kept) = match object_text(event) {
        Some(text) => {
            let document = IcalDocument::parse(text.as_bytes());
            let source = Source::caldav(event.ids.id.clone(), None);
            let uid = event.ids.ical_uid.as_deref();
            let kept = document
                .read(&source)
                .series
                .into_iter()
                .find(|found| Some(found.uid.as_str()) == uid)
                .map(|found| found.series);
            (document, kept)
        }
        None => (IcalDocument::new(PRODUCT_ID), None),
    };
    let series = replaced(kept, own_fields_removed(event));
    let losses = document.write(&series, stamp)?;
    Ok((
        String::from_utf8_lossy(&document.to_bytes()).into_owned(),
        losses,
    ))
}

/// The record that stands for `series`: its master, else its first changed override.
fn representative(series: EventSeries) -> Option<ProviderEvent> {
    if let Some(master) = series.master {
        return Some(master);
    }
    series.overrides.into_values().find_map(|kept| match kept {
        Override::Modified { event, .. } => Some(*event),
        Override::Cancelled { .. } => None,
    })
}

/// `kept` with the record that stands for it replaced by `event`, as [`representative`] chose it.
fn replaced(kept: Option<EventSeries>, event: ProviderEvent) -> EventSeries {
    let Some(mut series) = kept else {
        return EventSeries::single(event);
    };
    if series.master.is_some() {
        series.master = Some(event);
        return series;
    }
    let first: Option<InstanceKey> = series
        .overrides
        .iter()
        .find(|(_, kept)| matches!(kept, Override::Modified { .. }))
        .map(|(key, _)| key.clone());
    match first {
        Some(key) => {
            if let Some(Override::Modified { event: slot, .. }) = series.overrides.get_mut(&key) {
                let mut event = event;
                event.fields.recurrence = None;
                **slot = event;
            }
            series
        }
        None => {
            series.master = Some(event);
            series
        }
    }
}

/// `event` without the passthrough fields this crate added, which are not iCalendar lines.
fn own_fields_removed(event: &ProviderEvent) -> ProviderEvent {
    let mut passthrough = Passthrough::new();
    for field in event.passthrough.iter() {
        if !field.name.starts_with(OWN_PREFIX) {
            passthrough.push(field.name.clone(), field.raw.clone());
        }
    }
    ProviderEvent {
        passthrough,
        ..event.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::{OBJECT_FIELD, SCHEDULE_TAG_FIELD, object_text, read, schedule_tag, series, write};

    const SERIES: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\n\
BEGIN:VEVENT\r\nUID:standup@example.com\r\nDTSTAMP:20261101T120000Z\r\n\
DTSTART:20261102T140000Z\r\nDTEND:20261102T143000Z\r\nRRULE:FREQ=DAILY;COUNT=5\r\n\
SUMMARY:Standup\r\nX-EXAMPLE-KEPT:yes\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:standup@example.com\r\nDTSTAMP:20261101T120000Z\r\n\
RECURRENCE-ID:20261104T140000Z\r\nDTSTART:20261104T150000Z\r\nDTEND:20261104T153000Z\r\n\
SUMMARY:Standup, moved\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    fn stamp() -> jiff::Timestamp {
        "2026-11-20T00:00:00Z".parse().unwrap()
    }

    #[test]
    fn an_object_reads_as_its_master_and_keeps_its_text() {
        let event = read(
            "/cal/standup.ics",
            Some("\"1\""),
            Some("\"s1\""),
            SERIES.as_bytes(),
        )
        .expect("an event");
        assert_eq!(event.ids.id, "/cal/standup.ics");
        assert_eq!(event.ids.ical_uid.as_deref(), Some("standup@example.com"));
        assert_eq!(event.revision.as_ref().unwrap().as_str(), "\"1\"");
        assert_eq!(event.fields.title.as_deref(), Some("Standup"));
        assert!(event.fields.recurrence.is_some());
        assert_eq!(object_text(&event), Some(SERIES));
        assert_eq!(schedule_tag(&event), Some("\"s1\""));
        let whole = series(&event).expect("the series");
        assert_eq!(whole.series.overrides.len(), 1);
    }

    #[test]
    fn a_task_is_not_an_event() {
        let todo = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\n\
BEGIN:VTODO\r\nUID:t@example.com\r\nDTSTAMP:20261101T120000Z\r\nSUMMARY:Task\r\n\
END:VTODO\r\nEND:VCALENDAR\r\n";
        assert!(read("/cal/t.ics", None, None, todo.as_bytes()).is_none());
    }

    #[test]
    fn a_write_changes_only_the_master_fields_that_changed() {
        let mut event = read("/cal/standup.ics", Some("\"1\""), None, SERIES.as_bytes()).unwrap();
        event.fields.title = Some("Daily standup".to_owned());
        let (text, losses) = write(&event, stamp()).unwrap();
        assert!(losses.is_empty(), "{losses:?}");
        assert_eq!(
            text,
            SERIES.replacen("SUMMARY:Standup\r\n", "SUMMARY:Daily standup\r\n", 1)
        );
        assert!(!text.contains(OBJECT_FIELD));
        assert!(!text.contains(SCHEDULE_TAG_FIELD));
    }

    #[test]
    fn an_unchanged_record_writes_its_object_byte_for_byte() {
        let event = read("/cal/standup.ics", None, Some("\"s\""), SERIES.as_bytes()).unwrap();
        assert_eq!(write(&event, stamp()).unwrap().0, SERIES);
    }

    #[test]
    fn an_override_only_object_reads_and_writes_its_override() {
        let invitation = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\n\
BEGIN:VEVENT\r\nUID:review@example.com\r\nDTSTAMP:20261101T120000Z\r\n\
RECURRENCE-ID:20261104T140000Z\r\nDTSTART:20261104T150000Z\r\nDTEND:20261104T153000Z\r\n\
SUMMARY:Review\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let mut event = read("/cal/review.ics", None, None, invitation.as_bytes()).unwrap();
        assert_eq!(event.fields.title.as_deref(), Some("Review"));
        event.fields.title = Some("Review prepared".to_owned());
        let (text, _) = write(&event, stamp()).unwrap();
        assert_eq!(
            text,
            invitation.replace("SUMMARY:Review\r\n", "SUMMARY:Review prepared\r\n")
        );
    }

    #[test]
    fn a_new_record_writes_a_new_object() {
        let mut event = read("/cal/standup.ics", None, None, SERIES.as_bytes()).unwrap();
        event.passthrough = calendar_sync_model::Passthrough::new();
        event.ids.ical_uid = Some("new@example.com".to_owned());
        let (text, _) = write(&event, stamp()).unwrap();
        assert!(text.starts_with("BEGIN:VCALENDAR\r\n"));
        assert!(text.contains("PRODID:-//calendar-sync//calendar-sync-caldav//EN\r\n"));
        assert!(text.contains("UID:new@example.com\r\n"));
        assert!(text.contains("DTSTAMP:20261120T000000Z\r\n"));
        let again = read("/cal/new.ics", None, None, text.as_bytes()).unwrap();
        assert_eq!(again.fields, event.fields);
    }

    #[test]
    fn a_record_without_a_uid_cannot_be_written() {
        let mut event = read("/cal/standup.ics", None, None, SERIES.as_bytes()).unwrap();
        event.passthrough = calendar_sync_model::Passthrough::new();
        event.ids.ical_uid = None;
        assert!(write(&event, stamp()).is_err());
    }
}
