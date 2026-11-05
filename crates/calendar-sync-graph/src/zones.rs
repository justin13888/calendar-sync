//! Time zone names: Graph's Windows names and the model's IANA names.
//!
//! Graph names a zone either way: `Pacific Standard Time` or `America/Los_Angeles`. The table
//! between them is generated from the vendored CLDR `windowsZones.xml` by
//! `cargo xtask gen-windows-zones`.

use jiff::tz::{TimeZone, TimeZoneDatabase};

use crate::windows_zones::{IANA_TO_WINDOWS, WINDOWS_TO_IANA};

/// The zones CLDR's `windowsZones.xml` names by an id the IANA database now keeps only as a
/// backward link, with no entry under the current name: `(current IANA name, CLDR id)`. CLDR
/// keeps its ids stable across IANA renames, so its table cannot say this itself.
const RENAMED: [(&str, &str); 18] = [
    ("Africa/Asmara", "Africa/Asmera"),
    ("America/Argentina/Buenos_Aires", "America/Buenos_Aires"),
    ("America/Argentina/Catamarca", "America/Catamarca"),
    ("America/Argentina/Cordoba", "America/Cordoba"),
    ("America/Argentina/Jujuy", "America/Jujuy"),
    ("America/Argentina/Mendoza", "America/Mendoza"),
    ("America/Indiana/Indianapolis", "America/Indianapolis"),
    ("America/Kentucky/Louisville", "America/Louisville"),
    ("America/Nuuk", "America/Godthab"),
    ("Asia/Ho_Chi_Minh", "Asia/Saigon"),
    ("Asia/Kathmandu", "Asia/Katmandu"),
    ("Asia/Kolkata", "Asia/Calcutta"),
    ("Asia/Yangon", "Asia/Rangoon"),
    ("Atlantic/Faroe", "Atlantic/Faeroe"),
    ("Europe/Kyiv", "Europe/Kiev"),
    ("Pacific/Chuuk", "Pacific/Truk"),
    ("Pacific/Kanton", "Pacific/Enderbury"),
    ("Pacific/Pohnpei", "Pacific/Ponape"),
];

/// The id CLDR lists the IANA zone `iana` under: its CLDR id where CLDR keeps an older name.
fn cldr_id(iana: &str) -> &str {
    RENAMED
        .iter()
        .find(|(current, _)| *current == iana)
        .map_or(iana, |(_, cldr)| cldr)
}

/// The current IANA name for the CLDR id `cldr`.
fn current_name(cldr: &'static str) -> &'static str {
    RENAMED
        .iter()
        .find(|(_, old)| *old == cldr)
        .map_or(cldr, |(current, _)| current)
}

/// The IANA zone CLDR gives the Windows zone `windows` for territory `001`, compared without
/// regard to case, under its current IANA name where CLDR keeps an older one. `None` for a name
/// CLDR does not list.
///
/// ```
/// assert_eq!(
///     calendar_sync_graph::iana_zone("Pacific Standard Time"),
///     Some("America/Los_Angeles")
/// );
/// assert_eq!(calendar_sync_graph::iana_zone("India Standard Time"), Some("Asia/Kolkata"));
/// assert_eq!(calendar_sync_graph::iana_zone("America/Los_Angeles"), None);
/// ```
#[must_use]
pub fn iana_zone(windows: &str) -> Option<&'static str> {
    WINDOWS_TO_IANA
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(windows))
        .map(|(_, iana)| current_name(iana))
}

/// The Windows zone CLDR lists the IANA zone `iana` under, by its current name or by the older
/// one CLDR keeps. `None` for a zone CLDR does not list.
///
/// ```
/// assert_eq!(
///     calendar_sync_graph::windows_zone("America/Toronto"),
///     Some("Eastern Standard Time")
/// );
/// assert_eq!(calendar_sync_graph::windows_zone("Europe/Kyiv"), Some("FLE Standard Time"));
/// ```
#[must_use]
pub fn windows_zone(iana: &str) -> Option<&'static str> {
    let iana = cldr_id(iana);
    IANA_TO_WINDOWS
        .binary_search_by(|(name, _)| (*name).cmp(iana))
        .ok()
        .map(|index| IANA_TO_WINDOWS[index].1)
}

/// The IANA zone a Graph zone name stands for, with its rules from the bundled database: the name
/// itself where it is an IANA name, or the CLDR equivalent of a Windows name. jiff's global
/// database may read the filesystem, which this crate never does, so only the bundled one is
/// asked.
pub(crate) fn resolve(name: &str) -> Option<(String, TimeZone)> {
    let database = TimeZoneDatabase::bundled();
    if let Ok(zone) = database.get(name) {
        return Some((name.to_owned(), zone));
    }
    let iana = iana_zone(name)?;
    database.get(iana).ok().map(|zone| (iana.to_owned(), zone))
}

/// The IANA zone named `name`, from the bundled database.
pub(crate) fn iana(name: &str) -> Option<TimeZone> {
    TimeZoneDatabase::bundled().get(name).ok()
}

/// Whether the IANA zone `name` is UTC under another name. Graph writes an instant as a time in
/// `UTC`, so such a time reads as an instant.
pub(crate) fn is_utc(name: &str) -> bool {
    const UTC: [&str; 10] = [
        "UTC",
        "Etc/UTC",
        "Etc/UCT",
        "Etc/GMT",
        "Etc/Universal",
        "Etc/Zulu",
        "GMT",
        "UCT",
        "Universal",
        "Zulu",
    ];
    UTC.iter().any(|utc| utc.eq_ignore_ascii_case(name))
}

/// The name a zone is written to Graph under: its Windows name where CLDR lists one, which every
/// Graph client understands, and otherwise the IANA name.
pub(crate) fn graph_name(iana: &str) -> &str {
    windows_zone(iana).unwrap_or(iana)
}

#[cfg(test)]
mod tests {
    use super::{RENAMED, current_name, graph_name, iana_zone, is_utc, resolve, windows_zone};
    use crate::windows_zones::{IANA_TO_WINDOWS, WINDOWS_TO_IANA};

    #[test]
    fn tables_are_sorted_and_resolvable() {
        assert!(IANA_TO_WINDOWS.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert!(WINDOWS_TO_IANA.windows(2).all(|pair| pair[0].0 < pair[1].0));
        // Every golden zone is in the bundled database, so every Windows name resolves.
        for (windows, iana) in WINDOWS_TO_IANA {
            let (name, _) = resolve(windows).unwrap_or_else(|| panic!("{windows}"));
            // `UTC` is a Windows name and an IANA name both.
            assert!(
                name == current_name(iana) || name == windows,
                "{windows}: {name}"
            );
            assert!(super::iana(iana).is_some(), "{iana}");
            assert_eq!(windows_zone(iana), Some(windows), "{iana}");
        }
    }

    #[test]
    fn renamed_zones_map_both_ways() {
        assert!(RENAMED.windows(2).all(|pair| pair[0].0 < pair[1].0));
        for (current, cldr) in RENAMED {
            // CLDR lists the old id and not the current one, and both are in the database.
            assert!(windows_zone(cldr).is_some(), "{cldr}");
            assert!(
                IANA_TO_WINDOWS.iter().all(|(name, _)| *name != current),
                "{current}"
            );
            assert!(super::iana(current).is_some(), "{current}");
            assert_eq!(windows_zone(current), windows_zone(cldr), "{current}");
            assert_eq!(
                graph_name(current),
                windows_zone(cldr).unwrap(),
                "{current}"
            );
        }
        assert_eq!(graph_name("Asia/Kolkata"), "India Standard Time");
        assert_eq!(graph_name("Asia/Calcutta"), "India Standard Time");
        assert_eq!(iana_zone("India Standard Time"), Some("Asia/Kolkata"));
        assert_eq!(iana_zone("FLE Standard Time"), Some("Europe/Kyiv"));
        assert_eq!(resolve("Nepal Standard Time").unwrap().0, "Asia/Kathmandu");
        assert_eq!(resolve("SE Asia Standard Time").unwrap().0, "Asia/Bangkok");
    }

    #[test]
    fn names_resolve_both_ways() {
        assert_eq!(iana_zone("eastern standard time"), Some("America/New_York"));
        assert_eq!(iana_zone("Mars Standard Time"), None);
        assert_eq!(
            windows_zone("Europe/Berlin"),
            Some("W. Europe Standard Time")
        );
        assert_eq!(windows_zone("Mars/Olympus"), None);
        assert_eq!(graph_name("America/Toronto"), "Eastern Standard Time");
        assert_eq!(graph_name("Mars/Olympus"), "Mars/Olympus");
        assert_eq!(resolve("Europe/Berlin").unwrap().0, "Europe/Berlin");
        assert_eq!(resolve("Tokyo Standard Time").unwrap().0, "Asia/Tokyo");
        assert!(resolve("tzone://Microsoft/Custom").is_none());
        assert!(is_utc("UTC") && is_utc("Etc/UTC") && !is_utc("Europe/London"));
    }
}
