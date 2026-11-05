//! `cargo xtask gen-windows-zones [--check]`: the Graph crate's Windows time zone table, generated
//! from the vendored CLDR `windowsZones.xml`.
//!
//! Microsoft Graph names time zones by their Windows names (`Pacific Standard Time`), and the
//! event model by IANA names (`America/Los_Angeles`). CLDR's `windowsZones.xml` maps one to the
//! other. This command writes that mapping as Rust source, so the crate reads no file at run time
//! and the table changes only when the vendored CLDR release does. With `--check` it writes
//! nothing, and fails if the checked-in table differs from what it would write.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use crate::Result;

/// The vendored CLDR file the table is generated from.
const SOURCE: &str = "references/cldr/release-49/windowsZones.xml";

/// The README whose Unicode licence notice the generated file reproduces.
const NOTICE_SOURCE: &str = "references/README.md";

/// The generated file.
const TARGET: &str = "crates/calendar-sync-graph/src/windows_zones.rs";

/// The territory CLDR marks as a Windows zone's canonical IANA zone.
const GOLDEN: &str = "001";

/// Generates the table, then writes it, or with `check` compares it with the checked-in file.
pub(crate) fn run(root: &Path, check: bool) -> Result {
    let xml = read(&root.join(SOURCE))?;
    let readme = read(&root.join(NOTICE_SOURCE))?;
    let text = generate(&xml, &notice(&readme)?)?;
    let target = root.join(TARGET);
    if check {
        let current = read(&target).unwrap_or_default();
        if current != text {
            return Err(format!(
                "{TARGET} differs from what {SOURCE} generates; run `cargo xtask gen-windows-zones`"
            ));
        }
        println!("{TARGET} matches {SOURCE}");
    } else {
        std::fs::write(&target, text)
            .map_err(|e| format!("could not write {}: {e}", target.display()))?;
        println!("wrote {TARGET} from {SOURCE}");
    }
    Ok(())
}

fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).map_err(|e| format!("could not read {}: {e}", path.display()))
}

/// One `<mapZone>` element.
#[derive(Debug, PartialEq, Eq)]
struct MapZone {
    windows: String,
    territory: String,
    iana: Vec<String>,
}

/// The `<mapZone>` elements of `xml`, in document order. Each sits on a line of its own.
fn parse(xml: &str) -> Result<Vec<MapZone>> {
    let mut zones = Vec::new();
    for (number, line) in xml.lines().enumerate() {
        let line = line.trim();
        if !line.starts_with("<mapZone ") {
            continue;
        }
        let attribute = |name: &str| {
            attribute(line, name)
                .ok_or_else(|| format!("{SOURCE}:{}: no {name} attribute", number + 1))
        };
        let iana: Vec<String> = attribute("type")?
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        if iana.is_empty() {
            return Err(format!("{SOURCE}:{}: an empty type", number + 1));
        }
        zones.push(MapZone {
            windows: attribute("other")?,
            territory: attribute("territory")?,
            iana,
        });
    }
    if zones.is_empty() {
        return Err(format!("{SOURCE} holds no mapZone element"));
    }
    Ok(zones)
}

/// The value of the attribute `name` in the element on `line`, with XML's predefined entities
/// replaced.
fn attribute(line: &str, name: &str) -> Option<String> {
    let start = line.find(&format!(" {name}=\""))? + name.len() + 3;
    let length = line[start..].find('"')?;
    Some(
        line[start..start + length]
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&"),
    )
}

/// The Unicode licence notice in `readme`: the text block after its `## Unicode Licence V3`
/// heading.
fn notice(readme: &str) -> Result<String> {
    let section = readme
        .split_once("## Unicode Licence V3")
        .map(|(_, rest)| rest)
        .ok_or_else(|| format!("{NOTICE_SOURCE} has no Unicode Licence V3 section"))?;
    let block = section
        .split_once("```text\n")
        .and_then(|(_, rest)| rest.split_once("\n```"))
        .map(|(block, _)| block)
        .ok_or_else(|| {
            format!("{NOTICE_SOURCE}: the Unicode licence notice is not a text block")
        })?;
    Ok(block.to_owned())
}

/// One zone name to another, sorted by the first.
type Table<'a> = BTreeMap<&'a str, &'a str>;

/// The two tables: each Windows zone's golden IANA zone, and each IANA zone's Windows zone.
///
/// An IANA zone listed under several Windows zones takes the one it is golden for, and otherwise
/// the first in document order.
fn tables(zones: &[MapZone]) -> Result<(Table<'_>, Table<'_>)> {
    let mut to_iana = BTreeMap::new();
    for zone in zones.iter().filter(|zone| zone.territory == GOLDEN) {
        let [iana] = zone.iana.as_slice() else {
            return Err(format!(
                "{SOURCE}: the golden zone of {:?} is not one zone: {:?}",
                zone.windows, zone.iana
            ));
        };
        if to_iana
            .insert(zone.windows.as_str(), iana.as_str())
            .is_some()
        {
            return Err(format!("{SOURCE}: {:?} has two golden zones", zone.windows));
        }
    }
    let mut to_windows = BTreeMap::new();
    for zone in zones {
        if !to_iana.contains_key(zone.windows.as_str()) {
            return Err(format!("{SOURCE}: {:?} has no golden zone", zone.windows));
        }
        for iana in &zone.iana {
            to_windows
                .entry(iana.as_str())
                .or_insert(zone.windows.as_str());
        }
    }
    for (windows, iana) in &to_iana {
        to_windows.insert(iana, windows);
    }
    Ok((to_iana, to_windows))
}

/// The generated Rust source.
fn generate(xml: &str, notice: &str) -> Result<String> {
    let zones = parse(xml)?;
    let (to_iana, to_windows) = tables(&zones)?;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "//! Windows time zone names and their IANA equivalents, from the Unicode CLDR.\n\
         //!\n\
         //! Generated by `cargo xtask gen-windows-zones` from `{SOURCE}`. Do not edit it: CI\n\
         //! generates it again and fails on any difference.\n\
         //!\n\
         //! The data is derived from the Unicode CLDR, under this notice:\n\
         //!\n\
         //! ```text"
    );
    for line in notice.lines() {
        if line.is_empty() {
            out.push_str("//!\n");
        } else {
            let _ = writeln!(out, "//! {line}");
        }
    }
    out.push_str("//! ```\n\n");
    let _ = writeln!(
        out,
        "/// Each Windows zone and the IANA zone CLDR gives it for territory `{GOLDEN}`, by Windows\n\
         /// name.\n\
         pub(crate) const WINDOWS_TO_IANA: [(&str, &str); {}] = [",
        to_iana.len()
    );
    for (windows, iana) in &to_iana {
        let _ = writeln!(out, "    ({windows:?}, {iana:?}),");
    }
    out.push_str("];\n\n");
    let _ = writeln!(
        out,
        "/// Each IANA zone CLDR lists and its Windows zone, by IANA name: the Windows zone it is the\n\
         /// territory `{GOLDEN}` zone of, or else the first that lists it.\n\
         pub(crate) const IANA_TO_WINDOWS: [(&str, &str); {}] = [",
        to_windows.len()
    );
    for (iana, windows) in &to_windows {
        let _ = writeln!(out, "    ({iana:?}, {windows:?}),");
    }
    out.push_str("];\n");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{generate, parse, tables};

    const XML: &str = r#"<supplementalData>
		<mapTimezones otherVersion="7e11800" typeVersion="2021a">
			<!-- (UTC-08:00) Pacific Time (US & Canada) -->
			<mapZone other="Pacific Standard Time" territory="001" type="America/Los_Angeles"/>
			<mapZone other="Pacific Standard Time" territory="CA" type="America/Vancouver"/>
			<mapZone other="Pacific Standard Time" territory="US" type="America/Los_Angeles"/>
			<mapZone other="UTC" territory="001" type="Etc/UTC"/>
			<mapZone other="UTC" territory="ZZ" type="Etc/UTC Etc/GMT"/>
			<mapZone other="A &amp; B" territory="001" type="Etc/GMT"/>
		</mapTimezones>
</supplementalData>
"#;

    #[test]
    fn parses_every_map_zone() {
        let zones = parse(XML).unwrap();
        assert_eq!(zones.len(), 6);
        assert_eq!(zones[4].iana, ["Etc/UTC", "Etc/GMT"]);
        assert_eq!(zones[5].windows, "A & B");
    }

    #[test]
    fn golden_zones_win_both_ways() {
        let zones = parse(XML).unwrap();
        let (to_iana, to_windows) = tables(&zones).unwrap();
        assert_eq!(to_iana["Pacific Standard Time"], "America/Los_Angeles");
        assert_eq!(to_iana["UTC"], "Etc/UTC");
        assert_eq!(to_windows["America/Vancouver"], "Pacific Standard Time");
        // Listed first under UTC, but golden for "A & B".
        assert_eq!(to_windows["Etc/GMT"], "A & B");
    }

    #[test]
    fn a_windows_zone_without_a_golden_zone_is_refused() {
        let xml = r#"<mapZone other="Nowhere" territory="US" type="Etc/UTC"/>"#;
        assert!(tables(&parse(xml).unwrap()).is_err());
    }

    #[test]
    fn the_source_is_valid_rust_text() {
        let text = generate(XML, "NOTICE\n\nLINE").unwrap();
        assert!(text.contains("pub(crate) const WINDOWS_TO_IANA: [(&str, &str); 3] = ["));
        assert!(text.contains("    (\"A & B\", \"Etc/GMT\"),\n"));
        assert!(text.contains("//! NOTICE\n//!\n//! LINE\n"));
    }
}
