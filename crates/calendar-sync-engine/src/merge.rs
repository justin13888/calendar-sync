//! Three-way merge of one event at push time, field by field.
//!
//! An edit carries the values it was made against, its *base*, beside its own values, *ours*. At
//! push the executor reads the provider's current record, *theirs*, and merges each field:
//!
//! - theirs equals base: nobody else changed it, so ours is applied;
//! - theirs equals ours: the provider already holds the edit, so nothing is written;
//! - all three differ: a [`Conflict`], and the provider's value is kept.
//!
//! Because the base travels with the edit, every device merging the same edit against the same
//! provider record reaches the same result.
//!
//! # Units
//!
//! Every field of [`NormalizedEvent`] is one unit, except the attendees: each attendee is its own
//! unit, keyed by its address (compared ASCII case-insensitively) and by which occurrence of that
//! address it is. Two devices changing different attendees therefore do not conflict.
//!
//! # Ownership
//!
//! The [`Role`] says which parts of the event the writer owns. An [`Role::Owner`] owns every
//! field. An [`Role::Attendee`] owns only its own reply (the `participation` of its own attendee
//! entry) and its own reminders; a change it makes to anything else is [`Refused`], and the
//! provider's value is kept. This follows iTIP (RFC 5546, section 2.1.4): the organizer owns the
//! event, and an attendee changes only its own participation status.
//!
//! # Unknown fields
//!
//! The merged record starts from theirs, so its ids, revision, mapper version and
//! [`Passthrough`](calendar_sync_model::Passthrough) are the provider's, unchanged. A write sends
//! back every field the model has no place for.

use calendar_sync_model::{
    Attendee, EventStatus, EventTimes, Field, NormalizedEvent, Organizer, Participation,
    ProviderEvent, Recurrence, Reminders, Transparency, Visibility,
};

/// Which parts of an event the writer owns.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    /// The organizer, or the only user of an event with no organizer. Owns every field.
    Owner,
    /// An invitee. Owns its own reply and its own reminders, nothing else.
    Attendee {
        /// The writer's calendar address, as in its attendee entry. Compared ASCII
        /// case-insensitively.
        address: String,
    },
}

/// The result of merging an edit into the provider's current record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeOutcome {
    /// The record to write: theirs, with every field merged. Its ids, revision, mapper version
    /// and passthrough are theirs, unchanged.
    pub event: ProviderEvent,
    /// Whether `event` differs from theirs, so a write is needed.
    pub changed: bool,
    /// The units all three sides disagree on. Each keeps the provider's value in `event`.
    pub conflicts: Vec<Conflict>,
    /// The units the writer changed without owning them. Each keeps the provider's value in
    /// `event`.
    pub refused: Vec<Refused>,
}

/// A unit that base, ours and theirs all disagree on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    /// The field.
    pub field: Field,
    /// For [`Field::Attendees`], the address of the attendee entry in conflict.
    pub attendee: Option<String>,
    /// The value the edit was made against.
    pub base: FieldValue,
    /// The edit's value.
    pub ours: FieldValue,
    /// The provider's current value, which the merged record keeps.
    pub theirs: FieldValue,
}

/// A unit the writer changed without owning it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Refused {
    /// The field.
    pub field: Field,
    /// For [`Field::Attendees`], the address of the attendee entry the writer changed.
    pub attendee: Option<String>,
}

/// One side's value of a merge unit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldValue {
    /// [`NormalizedEvent::title`].
    Title(Option<String>),
    /// [`NormalizedEvent::times`].
    Times(EventTimes),
    /// [`NormalizedEvent::recurrence`].
    Recurrence(Option<Recurrence>),
    /// [`NormalizedEvent::organizer`].
    Organizer(Option<Organizer>),
    /// One entry of [`NormalizedEvent::attendees`]; `None` where that side does not have it.
    Attendee(Option<Attendee>),
    /// [`NormalizedEvent::location`].
    Location(Option<String>),
    /// [`NormalizedEvent::description`].
    Description(Option<String>),
    /// [`NormalizedEvent::status`].
    Status(EventStatus),
    /// [`NormalizedEvent::transparency`].
    Transparency(Transparency),
    /// [`NormalizedEvent::visibility`].
    Visibility(Visibility),
    /// [`NormalizedEvent::reminders`].
    Reminders(Reminders),
}

/// Merges an edit into the provider's current record.
///
/// `base` is the event the edit was made against, `ours` the edited event, and `theirs` the
/// provider's record as read just before the write. `role` decides which units the writer may
/// change. The function is pure and total: the same inputs give the same outcome on every device.
#[must_use]
pub fn merge(
    base: &NormalizedEvent,
    ours: &NormalizedEvent,
    theirs: &ProviderEvent,
    role: &Role,
) -> MergeOutcome {
    let mut merger = Merger {
        owner: matches!(role, Role::Owner),
        conflicts: Vec::new(),
        refused: Vec::new(),
    };
    let t = &theirs.fields;
    let fields = NormalizedEvent {
        title: merger.unit(
            Field::Title,
            &base.title,
            &ours.title,
            &t.title,
            FieldValue::Title,
        ),
        times: merger.unit(
            Field::Times,
            &base.times,
            &ours.times,
            &t.times,
            FieldValue::Times,
        ),
        recurrence: merger.unit(
            Field::Recurrence,
            &base.recurrence,
            &ours.recurrence,
            &t.recurrence,
            FieldValue::Recurrence,
        ),
        organizer: merger.unit(
            Field::Organizer,
            &base.organizer,
            &ours.organizer,
            &t.organizer,
            FieldValue::Organizer,
        ),
        attendees: merger.attendees(&base.attendees, &ours.attendees, &t.attendees, role),
        location: merger.unit(
            Field::Location,
            &base.location,
            &ours.location,
            &t.location,
            FieldValue::Location,
        ),
        description: merger.unit(
            Field::Description,
            &base.description,
            &ours.description,
            &t.description,
            FieldValue::Description,
        ),
        status: merger.unit(
            Field::Status,
            &base.status,
            &ours.status,
            &t.status,
            FieldValue::Status,
        ),
        transparency: merger.unit(
            Field::Transparency,
            &base.transparency,
            &ours.transparency,
            &t.transparency,
            FieldValue::Transparency,
        ),
        visibility: merger.unit(
            Field::Visibility,
            &base.visibility,
            &ours.visibility,
            &t.visibility,
            FieldValue::Visibility,
        ),
        // Reminders are the writer's own, whatever its role.
        reminders: merger.owned_unit(
            Field::Reminders,
            &base.reminders,
            &ours.reminders,
            &t.reminders,
            FieldValue::Reminders,
        ),
    };
    let changed = fields != theirs.fields;
    MergeOutcome {
        event: ProviderEvent {
            fields,
            ..theirs.clone()
        },
        changed,
        conflicts: merger.conflicts,
        refused: merger.refused,
    }
}

/// The verdict for one unit.
enum Verdict {
    /// Keep theirs: ours did not change it, or theirs already holds ours.
    Theirs,
    /// Apply ours.
    Ours,
    /// All three differ.
    Conflict,
    /// Ours changed it, and the writer does not own it.
    Refused,
}

fn verdict<T: PartialEq>(base: &T, ours: &T, theirs: &T, owned: bool) -> Verdict {
    if ours == base || ours == theirs {
        Verdict::Theirs
    } else if !owned {
        Verdict::Refused
    } else if theirs == base {
        Verdict::Ours
    } else {
        Verdict::Conflict
    }
}

struct Merger {
    owner: bool,
    conflicts: Vec<Conflict>,
    refused: Vec<Refused>,
}

impl Merger {
    /// Merges a unit the writer owns only as [`Role::Owner`].
    fn unit<T: PartialEq + Clone>(
        &mut self,
        field: Field,
        base: &T,
        ours: &T,
        theirs: &T,
        value: fn(T) -> FieldValue,
    ) -> T {
        let owned = self.owner;
        self.merge(field, None, base, ours, theirs, owned, value)
    }

    /// Merges a unit every writer owns.
    fn owned_unit<T: PartialEq + Clone>(
        &mut self,
        field: Field,
        base: &T,
        ours: &T,
        theirs: &T,
        value: fn(T) -> FieldValue,
    ) -> T {
        self.merge(field, None, base, ours, theirs, true, value)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one unit's identity, its three sides, its ownership and its reporting"
    )]
    fn merge<T: PartialEq + Clone>(
        &mut self,
        field: Field,
        attendee: Option<&str>,
        base: &T,
        ours: &T,
        theirs: &T,
        owned: bool,
        value: fn(T) -> FieldValue,
    ) -> T {
        match verdict(base, ours, theirs, owned) {
            Verdict::Theirs => theirs.clone(),
            Verdict::Ours => ours.clone(),
            Verdict::Conflict => {
                self.conflicts.push(Conflict {
                    field,
                    attendee: attendee.map(str::to_owned),
                    base: value(base.clone()),
                    ours: value(ours.clone()),
                    theirs: value(theirs.clone()),
                });
                theirs.clone()
            }
            Verdict::Refused => {
                self.refused.push(Refused {
                    field,
                    attendee: attendee.map(str::to_owned),
                });
                theirs.clone()
            }
        }
    }

    /// Merges the attendees entry by entry.
    ///
    /// The result keeps theirs' order, drops the entries the merge removes, and appends the
    /// entries only ours added, in ours' order.
    fn attendees(
        &mut self,
        base: &[Attendee],
        ours: &[Attendee],
        theirs: &[Attendee],
        role: &Role,
    ) -> Vec<Attendee> {
        let base = keyed(base);
        let ours = keyed(ours);
        let theirs = keyed(theirs);
        let own = match role {
            Role::Owner => None,
            Role::Attendee { address } => Some(address.to_ascii_lowercase()),
        };

        let mut order: Vec<&Key> = theirs.iter().map(|(key, _)| key).collect();
        order.extend(
            ours.iter()
                .map(|(key, _)| key)
                .filter(|key| find(&theirs, key).is_none()),
        );
        // An entry only base holds was removed on both sides: nothing to merge.

        let mut merged = Vec::new();
        for key in order {
            let b = find(&base, key);
            let o = find(&ours, key);
            let t = find(&theirs, key);
            let is_own = own.as_deref() == Some(key.address.as_str());
            let entry = if is_own {
                self.own_entry(b, o, t)
            } else {
                let owned = self.owner;
                self.merge(
                    Field::Attendees,
                    Some(&display_address(b, o, t)),
                    &b.cloned(),
                    &o.cloned(),
                    &t.cloned(),
                    owned,
                    FieldValue::Attendee,
                )
            };
            merged.extend(entry);
        }
        merged
    }

    /// Merges the writer's own attendee entry as an attendee: the reply is its own, the rest of
    /// the entry is the organizer's.
    fn own_entry(
        &mut self,
        base: Option<&Attendee>,
        ours: Option<&Attendee>,
        theirs: Option<&Attendee>,
    ) -> Option<Attendee> {
        let address = display_address(base, ours, theirs);
        let (Some(b), Some(o), Some(t)) = (base, ours, theirs) else {
            // Adding or removing itself is the organizer's call.
            return self.merge(
                Field::Attendees,
                Some(&address),
                &base.cloned(),
                &ours.cloned(),
                &theirs.cloned(),
                false,
                FieldValue::Attendee,
            );
        };

        // Everything but the reply: compare with the reply masked out.
        let rest = |a: &Attendee| Attendee {
            participation: Participation::NeedsAction,
            ..a.clone()
        };
        let mut entry = self.merge(
            Field::Attendees,
            Some(&address),
            &rest(b),
            &rest(o),
            &rest(t),
            false,
            |rest| FieldValue::Attendee(Some(rest)),
        );

        let participation =
            match verdict(&b.participation, &o.participation, &t.participation, true) {
                Verdict::Ours => o.participation,
                Verdict::Conflict => {
                    self.conflicts.push(Conflict {
                        field: Field::Attendees,
                        attendee: Some(address),
                        base: FieldValue::Attendee(Some(b.clone())),
                        ours: FieldValue::Attendee(Some(o.clone())),
                        theirs: FieldValue::Attendee(Some(t.clone())),
                    });
                    t.participation
                }
                Verdict::Theirs | Verdict::Refused => t.participation,
            };
        entry.participation = participation;
        Some(entry)
    }
}

/// An attendee's identity in a list: its address, lowercased, and which occurrence of that
/// address it is.
#[derive(PartialEq, Eq)]
struct Key {
    address: String,
    occurrence: usize,
}

fn keyed(attendees: &[Attendee]) -> Vec<(Key, &Attendee)> {
    let mut out: Vec<(Key, &Attendee)> = Vec::with_capacity(attendees.len());
    for attendee in attendees {
        let address = attendee.address.to_ascii_lowercase();
        let occurrence = out.iter().filter(|(key, _)| key.address == address).count();
        out.push((
            Key {
                address,
                occurrence,
            },
            attendee,
        ));
    }
    out
}

fn find<'a>(list: &[(Key, &'a Attendee)], key: &Key) -> Option<&'a Attendee> {
    list.iter()
        .find(|(candidate, _)| candidate == key)
        .map(|(_, attendee)| *attendee)
}

/// The address to report for an entry: theirs' spelling where it has one, else ours', else base's.
fn display_address(
    base: Option<&Attendee>,
    ours: Option<&Attendee>,
    theirs: Option<&Attendee>,
) -> String {
    theirs
        .or(ours)
        .or(base)
        .map(|attendee| attendee.address.clone())
        .unwrap_or_default()
}
