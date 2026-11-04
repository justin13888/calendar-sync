# ADR 0001: The iCalendar crate

- **Status:** accepted
- **Date:** 2026-11-04
- **Issue:** #2
- **Decision:** `ical-rs` 0.7.0 (crate `ical-rs`, library `ical`, repository `pimalaya/ical`), pinned with `=0.7.0`. `calcard` 0.3.14 is not used as a dependency.

## Context

`calendar-sync-ical` (#18) maps iCalendar to the model and back. Write-back must leave every byte the edit did not touch unchanged, and the crate must stay pure: no clock, filesystem, environment or ambient randomness, and a build for `wasm32-unknown-unknown`.

The [library evaluation](https://github.com/justin13888/Sunrise/blob/docs/calendar-first-class-goals/docs/09-integrations/calendar-library-evaluation.md) found two candidates and judged both from their READMEs only:

- `ical-rs` claims byte-faithful editing, recurrence with overrides, RFC 7529 `RSCALE`, and JSCalendar. It is new and was untested here.
- `calcard` 0.3.14 is the last release under MIT or Apache-2.0. The repository is archived, and development continues inside Stalwart under AGPL-3.0 or a commercial licence. It would be the fallback, for parsing only.

The issue set four exit criteria for the spike. This ADR records the evidence for each, and the decision.

## How the spike ran

The spike code was a throwaway binary built outside the workspace. It is not committed. It depended on `ical-rs` 0.7.0 (features `tzdb` and `rscale`) and `calcard` 0.3.14, and ran on Rust 1.99.1 on 2026-11-04.

| Corpus | Source | Commit | Files | Licence |
|---|---|---|---|---|
| libical | <https://github.com/libical/libical>, `test-data/` (every `*.ics`, including the 1,194 VTIMEZONE files under `zoneinfo2025c*/`, and the `fuzz*` crash inputs) | `1fa3287ac9009b7447b341ff1dabbd769965a1cf` (2026-11-03) | 1,265 | `LGPL-2.1-only OR MPL-2.0`, per the repository's `REUSE.toml` entry for `test-data/**` |
| ical.js | <https://github.com/kewisch/ical.js>, `samples/` and `test/parser/` (every `*.ics`) | `7bc16367692451cea30b2751f4ed5532d709d402` (2026-11-02) | 46 | `MPL-2.0` (`LICENSE`) |

For each file the spike did the following:

1. **Round trip.** It parsed every calendar in the file strictly with `IcalCst::parse_many`, serialized each one, and compared the concatenation with the input. It also ran `IcalCst::parse_recovering` over every file and compared the result in the same way.
2. **Edit.** It changed the `SUMMARY` of the first `VEVENT` that has one, then diffed the output against the input line by line. The edit counts as local when the only changed lines are that property's line and its folded continuations.
3. **Expansion.** It expanded every `VEVENT`, `VTODO` and `VJOURNAL` with both crates over the window [1970-01-01, 2040-01-01). For `ical-rs` it used `IcalRecurTimeline` per `UID`, with zones from `tzdb::zones`. For `calcard` it used `ICalendar::expand_dates` with UTC as the default zone. It then compared the multisets of (start, end) instants in UTC. A file where only the ends differ is counted apart from one where the starts differ.

Six hand-written cases supplement the corpora. The first three cover DST: a weekly series across a spring-forward, and a daily series whose instances fall in a gap and in a fold. The other three cover overrides: a moved instance plus an `EXDATE`, a `RECURRENCE-ID` written in UTC against a `TZID` series, and `RANGE=THISANDFUTURE`. The four worked examples of RFC 7529 section 4.3 were also run, plus three Gregorian `SKIP` cases.

## Evidence

### Byte-identical round trip over the libical and ical.js corpora: met

| Corpus | Strict parse accepted | Strict round trip identical | Recovering round trip identical | `SUMMARY` edits local to one line |
|---|---|---|---|---|
| libical | 1,218 / 1,265 | 1,218 / 1,218 | 1,265 / 1,265 | 14 / 14 |
| ical.js | 46 / 46 | 46 / 46 | 46 / 46 | 17 / 17 |

The strict parser rejected 47 libical files. 37 are `fuzz*` crash inputs. The other 10 (`caltime`, `crash`, `get_char_test`, `issue250` to `issue253`, `malloc`, `stresstest`, `zday`) are deliberately malformed: lines with no colon, non-UTF-8 property names, a component with no `END`. The recovering parser keeps those files byte for byte. Every file in both corpora came back identical through at least one of the two parsers, and every edit changed only the edited property.

### Override and DST results agree with `calcard`: not met as written, and every disagreement favours `ical-rs`

On ical.js, 14 of the 24 files that contain recurrence produce instants. For 9 of them the two crates agree exactly. For 1 only the ends differ, and for 4 the starts differ. On libical, 9 of the 637 files that contain recurrence produce instants with at least one crate, and none agrees exactly. The other 628 are mostly VTIMEZONE definitions with no events. Of the 9, only the ends differ in 1, the starts differ in 5, and 3 parse with `calcard` only.

Each disagreement was read against RFC 5545 (`references/rfc/rfc5545.txt`, with line numbers):

| Case | `ical-rs` | `calcard` 0.3.14 | RFC 5545 |
|---|---|---|---|
| Daily 02:30 `America/New_York` series over the spring-forward gap | Skips the instance in the gap, and `COUNT` keeps counting | Emits 02:30Z: the gap time is read as UTC | Section 3.3.10 (l. 2382-2386): such instances "MUST be ignored and MUST NOT be counted". **`ical-rs` is right** |
| Daily 01:30 `America/New_York` series over the fall-back fold | 05:30Z, the first occurrence (EDT) | 01:30Z: the fold time is read as UTC | Section 3.3.5 (l. 1889-1893): the first occurrence. **`ical-rs` is right** |
| Custom `VTIMEZONE` `TZID:America-SanJose` (libical `2446.ics`) | Resolves through the calendar's own `VTIMEZONE`: 21:00Z in PDT, 22:00Z in PST | Ignores the definition and reads every instance as UTC (14:00Z) | Section 3.6.5: the `VTIMEZONE` defines the zone. **`ical-rs` is right** |
| `VTIMEZONE` whose first onset is after the event (libical `2445.ics`, `US-Eastern` 1998-03-12 08:30) | -04:00, the `TZOFFSETFROM` of the earliest observance | +00:00 | Undefined. The `ical-rs` reading matches libical's convention |
| The rest of libical `2445.ics`: `VFREEBUSY` periods, and a `VJOURNAL` on a `DATE` | The spike expanded no `VFREEBUSY`. The `VJOURNAL` has zero duration | Expands `VFREEBUSY` as events. The `VJOURNAL` lasts one day | Spike scope, not a defect. Section 3.6.3 gives a `VJOURNAL` no duration |
| Override with a `TZID` `RECURRENCE-ID` (ical.js `recur_instances.ics`) | Replaces the instance | Keeps the original and adds the override | Section 3.8.4.4: the override replaces the instance. **`ical-rs` is right** |
| Override whose `RECURRENCE-ID` is in UTC and matches an `RDATE` with a `TZID` (same file) | Replaces it | Keeps both | Same. **`ical-rs` is right** |
| Two `RRULE`s (ical.js `multiple_rrules.ics`) | The union of both rules (36) | The first rule only (18) | Section 3.6.1 (l. 2938-2941) lists `rrule` under "The following is OPTIONAL, but SHOULD NOT occur more than once." Section 3.8.5.3 (l. 6810-6812): "The recurrence set generated with multiple "RRULE" properties is undefined." Undefined. The `ical-rs` union follows the same section's Description (l. 6823-6826), which gathers the instances "generated by any of the specified "RRULE" and "RDATE" properties" |
| `DATE-TIME` `DTSTART` with no `DTEND` or `DURATION` (ical.js `only_dtstart_time.ics`, libical `recur-errors.ics`) | Zero duration | Ends at 23:59:59 that day | Section 3.6.1 (l. 2994-2998): ends at the start. **`ical-rs` is right** |
| `DATE` `EXDATE` beside a `DATE-TIME` `DTSTART` (ical.js `rdate_exdate.ics`) | Removes every instance on that day | Ignores it | Mistyped input. `ical-rs` follows ical.js, which has a test for this behaviour |
| `RDATE` typed `DATE-TIME` beside a `DATE` `DTSTART` (ical.js `google_birthday.ics`) | Drops the mistyped `RDATE` | Keeps it | Mistyped input. Both readings are defensible |
| libical `restriction.ics`, `smallcluster.ics`, and the three files only `calcard` parses (`fuzz448717854`, `issue251.ics`, `stresstest.ics`) | No instants, or a strict parse error | Instants | Duplicated `RECURRENCE-ID`, `UNTIL=123456T123456`, missing `DTSTART` value: deliberately malformed |
| libical `2446.ics`, `large.ics` | | | iTIP message streams, with `CANCEL`, `COUNTER` and orphan overrides across separate calendars. A per-file instant set is not a meaningful comparison here. The one semantic difference traced in them is the `America-SanJose` case above |
| Weekly `America/New_York` series across spring-forward. Moved instance plus `EXDATE` in `Europe/Berlin`. UTC `RECURRENCE-ID`. `RANGE=THISANDFUTURE` | Same instants as `calcard` | Same instants as `ical-rs` | Both right |

`ical-rs` never disagreed with RFC 5545 where the RFC decides the case. Where `calcard` diverges on well-formed input and the RFC decides the case, the RFC rules against `calcard`. That rules `calcard` 0.3.14 out as an oracle for DST: it reads gap and fold times, and every time under a non-IANA `VTIMEZONE`, as UTC.

### RSCALE claims reproduced or recorded as false: reproduced

With the `rscale` feature, `IcalRecurExpand` reproduced every row of the four RFC 7529 section 4.3 tables (`references/rfc/rfc7529.txt`):

- Chinese New Year
- the Ethiopic 13th month
- the Hebrew anniversary in Adar I with `SKIP=FORWARD`
- the Gregorian leap day with `SKIP=FORWARD`

It also got three further cases right: plain `FREQ=YEARLY` from 29 February, which yields leap years only, and monthly from the 31st with `SKIP=BACKWARD` and with `SKIP=OMIT`.

The scale arithmetic comes from `icu_calendar` (Unicode-3.0). The README's RFC table still says that a non-Gregorian scale "is carried, not expanded", which contradicts the 0.7.0 changelog and the behaviour seen here. The README is stale; the claim in the changelog holds. `calcard` 0.3.14's rule engine (`datecalc`) has no `RSCALE` support.

### Maintainer and API churn: acceptable, with containment

| | `ical-rs` | `calcard` 0.3.14 |
|---|---|---|
| Repository | `pimalaya/ical`, created 2026-07-03, not archived, last push 2026-11-03 | `stalwartlabs/calcard`, archived. Later versions are AGPL-3.0 or commercial |
| Maintainers | One (61 of 61 commits). Funded by NLnet NGI Zero grants through 2027 | None for the permissive line |
| Releases | 11 in three months (0.1.0 on 2026-08-08 to 0.7.0 on 2026-11-03). Every minor release is breaking under 0.x semver, and 0.7.0 alone lists six breaking changes | Final |
| Licence | MIT OR Apache-2.0 | Apache-2.0 OR MIT |
| Dependency graph, all features | 50 crates. Passes this repository's `deny.toml` (`bans`, `licenses`, `sources`). No `getrandom`, `rand`, `tokio`, `hyper` or `reqwest` | `chrono` and `chrono-tz` |
| Date and time | Its own civil arithmetic. `jiff` and `jiff-tzdb` behind `tzdb` | `chrono`, against this repository's rule that `jiff` is the only date and time library |
| `no_std`, wasm | `no_std` with `alloc`. Builds for `wasm32-unknown-unknown` with all features | Not checked: ruled out above |
| MSRV | 1.89, within this workspace's 1.91 | |

The churn is real: a single maintainer, and a breaking release every few weeks. It is acceptable because its cost lands in one place, `calendar-sync-ical`, and because no fallback is better. `calcard` 0.3.14 is frozen with the DST and override defects above, and any fix to it would come from AGPL code that this repository must never copy.

## Decision

1. `calendar-sync-ical` depends on `ical-rs`, pinned `=0.7.0`. Enable `tzdb`. Enable `rscale` only if #4's RSCALE support needs it.
2. No `ical-rs` type appears in the public API of any `calendar-sync` crate. `calendar-sync-ical` maps to and from the model, so an upgrade or a replacement stays inside that crate.
3. An upgrade is its own pull request. It reruns the corpus check from #18 and reads the changelog's **BREAKING** entries.
4. `calcard` is not a dependency of any crate, normal or dev. It may run as a differential oracle out of process or in a throwaway spike, and only for non-DST recurrence: its zone handling is wrong, as shown above. Post-archive `calcard` code is AGPL: never copy it.
5. No corpus is vendored. libical's `test-data` is `LGPL-2.1-only OR MPL-2.0` and ical.js is `MPL-2.0`. Both are copyleft, so `tests/corpora/` stays empty. A harness that needs them fetches them at the commits pinned above, outside the tree.

## Consequences

- #18 runs the round trip and edit checks over libical and ical.js out of tree, at the commits above, rather than over vendored files.
- #4 should not take `calcard` 0.3.14 as ground truth for gaps, folds or custom `VTIMEZONE`s. RFC 5545 section 3.3.10 ignores an `RRULE` instance that falls in a gap, while section 3.3.5 shifts a written `DTSTART` in a gap forward. Using jiff's "compatible" disambiguation alone would give the second reading to both. `ical-rs`'s `IcalRecurTimeline` implements both and can serve as a second oracle.
- The `ical-rs` README is not evidence. Its changelog and its behaviour are.

## Reopen when

- `ical-rs` is archived, goes six months without a release, or leaves a reported data-loss bug (bytes changed that an edit did not touch) unfixed for 30 days.
- `ical-rs` changes licence away from MIT OR Apache-2.0, or adds a dependency that fails `deny.toml`, the purity gate or the wasm build.
- An upgrade breaks the byte-identical round trip or the edit locality on either corpus.
- `ical-rs` disagrees with RFC 5545, with its errata, or with RFC 7529 on a case the RFC decides, and upstream declines the fix.
- A permissively licensed crate offers byte-faithful editing with more than one maintainer and a stable API.
