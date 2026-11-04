//! Time zones: resolving a `TZID` to an IANA name, and the `VTIMEZONE` a written `TZID` needs.
//!
//! Zones come from the IANA database `jiff` bundles (through `ical`'s `tzdb` feature), which is
//! compiled in: no file, environment or system zone is read.

use std::collections::BTreeMap;

use calendar_sync_model::TimeZoneId;
use ical::tree::cst::{IcalCst, IcalItem};

use crate::line;

/// The `VTIMEZONE`s one calendar defines, by `TZID`, each with the IANA name it names, if any.
#[derive(Debug, Default)]
pub(crate) struct Zones {
    /// `TZID` as written to the IANA location its `X-LIC-LOCATION` names.
    locations: BTreeMap<String, String>,
}

impl Zones {
    /// The zones `calendar` defines.
    pub(crate) fn of(calendar: &IcalCst<'_>) -> Self {
        let mut locations = BTreeMap::new();
        for item in &calendar.items {
            let IcalItem::Component(component) = item else {
                continue;
            };
            if !line::component_is(component, "VTIMEZONE") {
                continue;
            }
            let Some(tzid) = line::first(component, "TZID").map(line::text) else {
                continue;
            };
            if let Some(location) = line::first(component, "X-LIC-LOCATION").map(line::text) {
                locations.insert(tzid, location);
            }
        }
        Self { locations }
    }

    /// The IANA zone a `TZID` names, or `Err` with the `TZID` as written when it names none this
    /// crate knows.
    ///
    /// In order: the name itself; the longest `/`-separated suffix that is a zone, which reads the
    /// prefixed names Mozilla and libical write (`/mozilla.org/20050126_1/America/New_York`); and
    /// the `X-LIC-LOCATION` of the calendar's `VTIMEZONE` for it.
    pub(crate) fn resolve(&self, tzid: &str) -> Result<TimeZoneId, TimeZoneId> {
        if known(tzid) {
            return Ok(TimeZoneId::new(tzid));
        }
        let mut rest = tzid;
        while let Some((_, suffix)) = rest.split_once('/') {
            if known(suffix) {
                return Ok(TimeZoneId::new(suffix));
            }
            rest = suffix;
        }
        if let Some(location) = self.locations.get(tzid).filter(|name| known(name)) {
            return Ok(TimeZoneId::new(location.as_str()));
        }
        Err(TimeZoneId::new(tzid))
    }
}

/// Whether the bundled database knows `name`.
pub(crate) fn known(name: &str) -> bool {
    !name.is_empty() && ical::tzdb::is_known(name)
}

/// Whether `calendar` has a `VTIMEZONE` whose `TZID` is `tzid`.
pub(crate) fn defines(calendar: &IcalCst<'_>, tzid: &str) -> bool {
    calendar.items.iter().any(|item| match item {
        IcalItem::Component(component) => {
            line::component_is(component, "VTIMEZONE")
                && line::first(component, "TZID").is_some_and(|line| line::text(line) == tzid)
        }
        _ => false,
    })
}

/// The `VTIMEZONE` for the IANA zone `tzid`, describing the era around `anchor` (Unix seconds).
pub(crate) fn vtimezone(tzid: &str, anchor: i64) -> Option<IcalCst<'static>> {
    ical::tzdb::vtimezone(tzid, anchor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_iana_names_prefixed_names_and_locations() {
        let calendar = IcalCst::parse(concat!(
            "BEGIN:VCALENDAR\r\n",
            "BEGIN:VTIMEZONE\r\n",
            "TZID:Custom Berlin\r\n",
            "X-LIC-LOCATION:Europe/Berlin\r\n",
            "END:VTIMEZONE\r\n",
            "END:VCALENDAR\r\n",
        ))
        .unwrap();
        let zones = Zones::of(&calendar);
        assert_eq!(
            zones.resolve("America/New_York"),
            Ok(TimeZoneId::new("America/New_York"))
        );
        assert_eq!(
            zones.resolve("/mozilla.org/20050126_1/America/New_York"),
            Ok(TimeZoneId::new("America/New_York"))
        );
        assert_eq!(
            zones.resolve("Custom Berlin"),
            Ok(TimeZoneId::new("Europe/Berlin"))
        );
        assert_eq!(
            zones.resolve("America-SanJose"),
            Err(TimeZoneId::new("America-SanJose"))
        );
        assert!(defines(&calendar, "Custom Berlin"));
        assert!(!defines(&calendar, "Europe/Berlin"));
    }
}
