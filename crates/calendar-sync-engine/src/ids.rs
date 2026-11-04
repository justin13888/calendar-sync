//! Provider ids derived from one edit id, so every device that pushes an edit names the same
//! resource.
//!
//! An application mints an [`EditId`] once, when the user makes the edit, and every id a create
//! needs is derived from it: the Google event id, the `CalDAV` object's href and its iCalendar
//! `UID`, and the Graph idempotency key. A retry of the create, on this device or another, names
//! the same resource, so the provider can refuse it or return the event already made.
//!
//! The derivation is base32hex (RFC 4648, section 7), lower case and unpadded, of the edit id's
//! UTF-8 bytes, after the prefix [`PREFIX`]. It is injective, so two edit ids never share a
//! derived id, and its alphabet (`0`-`9`, `a`-`v`) is the one Google allows in an event id and
//! is safe in a URL path segment.
//!
//! ```
//! use calendar_sync_engine::{DerivedIds, EditId};
//!
//! let ids = DerivedIds::of(&EditId::new("e1"));
//! assert_eq!(ids.key(), "cseditckog");
//! assert_eq!(ids.google_id().as_str(), "cseditckog");
//! assert_eq!(ids.caldav_href("/cal/").as_str(), "/cal/cseditckog.ics");
//! assert_eq!(ids.ical_uid(), "cseditckog@calendar-sync");
//! ```

use crate::seam::ResourceId;
use crate::sync::EditId;

/// What every derived id starts with. It is itself base32hex, so a derived id is one throughout.
pub const PREFIX: &str = "csedit";

/// The longest derived key Google accepts as an event id.
pub const GOOGLE_ID_MAX: usize = 1024;

const ALPHABET: &[u8; 32] = b"0123456789abcdefghijklmnopqrstuv";

/// The provider ids a create derives from its [`EditId`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DerivedIds {
    key: String,
}

impl DerivedIds {
    /// The ids derived from `edit`.
    #[must_use]
    pub fn of(edit: &EditId) -> Self {
        let bytes = edit.as_str().as_bytes();
        let mut key = String::with_capacity(PREFIX.len() + bytes.len().div_ceil(5) * 8);
        key.push_str(PREFIX);
        let mut buffer: u16 = 0;
        let mut bits = 0;
        for &byte in bytes {
            buffer = (buffer << 8) | u16::from(byte);
            bits += 8;
            while bits >= 5 {
                bits -= 5;
                key.push(char::from(ALPHABET[usize::from((buffer >> bits) & 0x1f)]));
            }
            buffer &= (1 << bits) - 1;
        }
        if bits > 0 {
            key.push(char::from(
                ALPHABET[usize::from((buffer << (5 - bits)) & 0x1f)],
            ));
        }
        Self { key }
    }

    /// The derived key: [`PREFIX`] and the base32hex of the edit id.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The Google event id to insert under. Google accepts it when it is at most
    /// [`GOOGLE_ID_MAX`] characters, which an edit id of up to 636 bytes guarantees.
    #[must_use]
    pub fn google_id(&self) -> ResourceId {
        ResourceId::new(self.key.clone())
    }

    /// The `CalDAV` object's href: the key, with an `.ics` suffix, inside `collection`, which
    /// ends with `/`.
    #[must_use]
    pub fn caldav_href(&self, collection: &str) -> ResourceId {
        ResourceId::new(format!("{collection}{}.ics", self.key))
    }

    /// The iCalendar `UID` of the created event.
    #[must_use]
    pub fn ical_uid(&self) -> String {
        format!("{}@calendar-sync", self.key)
    }

    /// The idempotency key of a create on a provider that assigns its own ids: Graph's
    /// `transactionId`, and the value of the extended property a later
    /// [`Request::Lookup`](crate::Request::Lookup) filters on. It is also the [`ResourceId`] the
    /// create is queued under until the provider names the event; see
    /// [`Notice::Remapped`](crate::Notice::Remapped).
    #[must_use]
    pub fn idempotency_key(&self) -> ResourceId {
        ResourceId::new(self.key.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::{ALPHABET, DerivedIds, GOOGLE_ID_MAX, PREFIX};
    use crate::sync::EditId;

    fn key(id: &str) -> String {
        DerivedIds::of(&EditId::new(id)).key().to_owned()
    }

    #[test]
    fn matches_rfc_4648_base32hex_vectors() {
        // RFC 4648, section 10, lower-cased and unpadded: each prefix of "foobar".
        let encoded = [
            "",
            "co",
            "cpng",
            "cpnmu",
            "cpnmuog",
            "cpnmuoj1",
            "cpnmuoj1e8",
        ];
        for (length, encoded) in encoded.into_iter().enumerate() {
            let input = &"foobar"[..length];
            assert_eq!(key(input), format!("{PREFIX}{encoded}"), "{input:?}");
        }
    }

    #[test]
    fn distinct_edits_derive_distinct_ids() {
        // Strings that differ only after a padding boundary, or in length.
        let ids = ["a", "a\0", "b", "ab", "bb", "aaaaa", "aaaaaa", "\u{e9}"];
        let keys: std::collections::BTreeSet<String> = ids.iter().map(|id| key(id)).collect();
        assert_eq!(keys.len(), ids.len());
    }

    #[test]
    fn a_key_is_a_valid_google_event_id() {
        let long = "x".repeat(636);
        for id in [
            "",
            "e",
            "01HZY3J5K8",
            "édit with spaces/and:colons",
            long.as_str(),
        ] {
            let key = key(id);
            assert!((5..=GOOGLE_ID_MAX).contains(&key.len()), "{id:?}");
            assert!(key.bytes().all(|byte| ALPHABET.contains(&byte)), "{id:?}");
        }
    }
}
