# References

The specifications calendar-sync is built against, vendored as ground truth. When the code and a
summary of a spec disagree, the file here decides. Every file is pinned to an exact version,
release, revision or commit, and `SHA256SUMS` holds its checksum.

The list is fixed by the maintainer. Do not add, drop or refresh a file without an issue that
says so. Test corpora are not references and do not belong here.

Nothing here is packaged: the crates live under `crates/`, and `cargo package` only takes files
from a crate's own directory.

## Checking it

```sh
mise run references:verify    # offline: checksums, every file listed, README and SHA256SUMS agree
mise run references:upstream  # downloads every source again and compares it
cd references && sha256sum -c SHA256SUMS   # the same checksums, with coreutils
```

`references:verify` runs in CI (the `references` job) and in the pre-push hook. It fails when one
byte of a vendored file changes, when a file under `references/` is not in `SHA256SUMS`, and when
this README and `SHA256SUMS` do not name the same files.

`references:upstream` runs weekly in `.github/workflows/scheduled.yml`. Each table row's `Check`
cell says what a difference means:

- `immutable`: the source is a published RFC, an archived draft revision, a release tag or a
  commit. A difference fails the run, because the vendored copy is no longer what its pin names.
- `snapshot`: the source is a moving document vendored as a dated copy. A difference is reported
  and does not fail. For the Google discovery document the report compares its `revision` field,
  because Google serves the document with its keys in a varying order.

`verify` and `upstream` read the tables below: the `File` cell (a path below `references/`), the
`Source` cell (the exact URL downloaded) and the `Check` cell. Keep those three cells in that form.

## Refreshing a file

Only under an issue that asks for it. Download the file from its `Source` to its `File` path,
update its row (version, retrieval date), regenerate the checksums, and run `references:verify`:

```sh
cd references
find . -type f ! -name README.md ! -name SHA256SUMS | sed 's|^\./||' | LC_ALL=C sort \
  | xargs sha256sum > SHA256SUMS
```

## RFCs: `rfc/`

Plain-text RFCs from the RFC Editor, unmodified. Copyright © the IETF Trust and the persons
identified as the document authors (RFC 3339: © The Internet Society). Reproduced in full under the
IETF Trust Legal Provisions, [BCP 78](https://www.rfc-editor.org/info/bcp78), which permit copying
an RFC without modification.

| File | Version | Source | Retrieved | Licence | Check | Why we vendor it |
|---|---|---|---|---|---|---|
| `rfc/rfc5545.txt` | RFC 5545 | <https://www.rfc-editor.org/rfc/rfc5545.txt> | 2026-11-04 | IETF Trust, BCP 78 | immutable | iCalendar: the event model, `RRULE` expansion and the text format `calendar-sync-ical` reads and writes |
| `rfc/rfc5545-inline-errata.html` | RFC 5545 with errata inline, as rendered on 2026-11-04 | <https://www.rfc-editor.org/rfc/inline-errata/rfc5545.html> | 2026-11-04 | IETF Trust, BCP 78 | snapshot | The corrected RFC 5545; the recurrence oracle follows the errata, not the original text |
| `rfc/rfc7529.txt` | RFC 7529 | <https://www.rfc-editor.org/rfc/rfc7529.txt> | 2026-11-04 | IETF Trust, BCP 78 | immutable | Non-Gregorian recurrence (`RSCALE`, `SKIP`): what an unsupported calendar scale must do |
| `rfc/rfc7986.txt` | RFC 7986 | <https://www.rfc-editor.org/rfc/rfc7986.txt> | 2026-11-04 | IETF Trust, BCP 78 | immutable | iCalendar properties added since 5545 (`COLOR`, `CONFERENCE`, `IMAGE`, ...) |
| `rfc/rfc6868.txt` | RFC 6868 | <https://www.rfc-editor.org/rfc/rfc6868.txt> | 2026-11-04 | IETF Trust, BCP 78 | immutable | `^`-escaping of iCalendar parameter values |
| `rfc/rfc5546.txt` | RFC 5546 | <https://www.rfc-editor.org/rfc/rfc5546.txt> | 2026-11-04 | IETF Trust, BCP 78 | immutable | iTIP: `METHOD`, `SEQUENCE` and how scheduling changes an event |
| `rfc/rfc4791.txt` | RFC 4791 | <https://www.rfc-editor.org/rfc/rfc4791.txt> | 2026-11-04 | IETF Trust, BCP 78 | immutable | CalDAV: calendar collections, `calendar-query` and `calendar-multiget` |
| `rfc/rfc4918.txt` | RFC 4918 | <https://www.rfc-editor.org/rfc/rfc4918.txt> | 2026-11-04 | IETF Trust, BCP 78 | immutable | WebDAV: `PROPFIND`, ETags, `If-Match` and multistatus, which CalDAV builds on |
| `rfc/rfc6578.txt` | RFC 6578 | <https://www.rfc-editor.org/rfc/rfc6578.txt> | 2026-11-04 | IETF Trust, BCP 78 | immutable | WebDAV `sync-collection`: the CalDAV incremental sync token |
| `rfc/rfc6638.txt` | RFC 6638 | <https://www.rfc-editor.org/rfc/rfc6638.txt> | 2026-11-04 | IETF Trust, BCP 78 | immutable | CalDAV scheduling: the server-side effects of writing an event with attendees |
| `rfc/rfc6764.txt` | RFC 6764 | <https://www.rfc-editor.org/rfc/rfc6764.txt> | 2026-11-04 | IETF Trust, BCP 78 | immutable | CalDAV service discovery (`.well-known`, SRV) |
| `rfc/rfc9110.txt` | RFC 9110 | <https://www.rfc-editor.org/rfc/rfc9110.txt> | 2026-11-04 | IETF Trust, BCP 78 | immutable | HTTP semantics: conditional requests, ETags, status codes and `Retry-After` |
| `rfc/rfc6585.txt` | RFC 6585 | <https://www.rfc-editor.org/rfc/rfc6585.txt> | 2026-11-04 | IETF Trust, BCP 78 | immutable | HTTP 429 Too Many Requests, which every provider client must back off on |
| `rfc/rfc3339.txt` | RFC 3339 | <https://www.rfc-editor.org/rfc/rfc3339.txt> | 2026-11-04 | The Internet Society, BCP 78 | immutable | Timestamps in the Google and Graph JSON payloads |
| `rfc/rfc8984.txt` | RFC 8984 | <https://www.rfc-editor.org/rfc/rfc8984.txt> | 2026-11-04 | IETF Trust, BCP 78 | immutable | JSCalendar 1.0: a JSON event model to compare the internal model against |

## Internet-Drafts: `drafts/`

The latest revision of each draft on the retrieval date, from the IETF archive, unmodified. A newer
revision is a new file under a new issue, not a refresh of this one. Copyright © the IETF Trust and
the persons identified as the document authors, reproduced under BCP 78.

| File | Version | Source | Retrieved | Licence | Check | Why we vendor it |
|---|---|---|---|---|---|---|
| `drafts/draft-ietf-calext-jscalendarbis-22.txt` | revision 22, 5 October 2026 | <https://www.ietf.org/archive/id/draft-ietf-calext-jscalendarbis-22.txt> | 2026-11-04 | IETF Trust, BCP 78 | immutable | JSCalendar 2.0, the revision of RFC 8984 the model tracks |
| `drafts/draft-ietf-jmap-calendars-33.txt` | revision 33, 14 October 2026 | <https://www.ietf.org/archive/id/draft-ietf-jmap-calendars-33.txt> | 2026-11-04 | IETF Trust, BCP 78 | immutable | JMAP for Calendars: sync state and change semantics for a JSCalendar store |

## Unicode CLDR: `cldr/release-49/`

From [unicode-org/cldr](https://github.com/unicode-org/cldr) at the `release-49` tag (commit
`96e54b273d2da4ec6fbad350ba19ff23fa24a588`), unmodified. Copyright © 1991-2026 Unicode, Inc.,
licensed under Unicode-3.0; the licence's notice is reproduced in full [below](#unicode-licence-v3).

| File | Version | Source | Retrieved | Licence | Check | Why we vendor it |
|---|---|---|---|---|---|---|
| `cldr/release-49/windowsZones.xml` | CLDR 49 (`release-49`) | <https://raw.githubusercontent.com/unicode-org/cldr/release-49/common/supplemental/windowsZones.xml> | 2026-11-04 | Unicode-3.0 | immutable | Windows-to-IANA time zone mapping; the input the Graph time zone table is generated from |

## Google Calendar API: `google/`

The Calendar API v3 discovery document, as served by the Google API Discovery Service on the
retrieval date, unmodified. Copyright © Google LLC; provided under the
[Google APIs Terms of Service](https://developers.google.com/terms), and published by Google under
Apache-2.0 in [googleapis/discovery-artifact-manager](https://github.com/googleapis/discovery-artifact-manager).

| File | Version | Source | Retrieved | Licence | Check | Why we vendor it |
|---|---|---|---|---|---|---|
| `google/calendar-v3-discovery.json` | `revision` 20261030 | <https://www.googleapis.com/discovery/v1/apis/calendar/v3/rest> | 2026-11-04 | Google APIs Terms of Service; Apache-2.0 | snapshot | The `Event` resource, recurrence fields and `syncToken` paging `calendar-sync-google` maps |

## Microsoft Graph: `graph/`

From [microsoftgraph/microsoft-graph-docs-contrib](https://github.com/microsoftgraph/microsoft-graph-docs-contrib)
at commit `bb35043c742d4406d73ccae904c05fbfca7c7428` (2026-10-31), unmodified. Microsoft Graph
documentation, © Microsoft Corporation, licensed under
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/). No changes were made.

| File | Version | Source | Retrieved | Licence | Check | Why we vendor it |
|---|---|---|---|---|---|---|
| `graph/event.md` | commit `bb35043` | <https://raw.githubusercontent.com/microsoftgraph/microsoft-graph-docs-contrib/bb35043c742d4406d73ccae904c05fbfca7c7428/api-reference/v1.0/resources/event.md> | 2026-11-04 | CC-BY-4.0 | immutable | The `event` resource `calendar-sync-graph` maps |
| `graph/event-delta.md` | commit `bb35043` | <https://raw.githubusercontent.com/microsoftgraph/microsoft-graph-docs-contrib/bb35043c742d4406d73ccae904c05fbfca7c7428/api-reference/v1.0/api/event-delta.md> | 2026-11-04 | CC-BY-4.0 | immutable | `calendarView` delta: the Graph incremental sync request |
| `graph/patternedrecurrence.md` | commit `bb35043` | <https://raw.githubusercontent.com/microsoftgraph/microsoft-graph-docs-contrib/bb35043c742d4406d73ccae904c05fbfca7c7428/api-reference/v1.0/resources/patternedrecurrence.md> | 2026-11-04 | CC-BY-4.0 | immutable | Graph's recurrence: a pattern and a range |
| `graph/recurrencepattern.md` | commit `bb35043` | <https://raw.githubusercontent.com/microsoftgraph/microsoft-graph-docs-contrib/bb35043c742d4406d73ccae904c05fbfca7c7428/api-reference/v1.0/resources/recurrencepattern.md> | 2026-11-04 | CC-BY-4.0 | immutable | The recurrence patterns `RRULE` must map to and from |
| `graph/recurrencerange.md` | commit `bb35043` | <https://raw.githubusercontent.com/microsoftgraph/microsoft-graph-docs-contrib/bb35043c742d4406d73ccae904c05fbfca7c7428/api-reference/v1.0/resources/recurrencerange.md> | 2026-11-04 | CC-BY-4.0 | immutable | Recurrence ranges (`endDate`, `numbered`, `noEnd`) and their time zone |
| `graph/delta-query-events.md` | commit `bb35043` | <https://raw.githubusercontent.com/microsoftgraph/microsoft-graph-docs-contrib/bb35043c742d4406d73ccae904c05fbfca7c7428/concepts/delta-query-events.md> | 2026-11-04 | CC-BY-4.0 | immutable | How event delta pages, `@odata.deltaLink` and removals behave |
| `graph/delta-query-overview.md` | commit `bb35043` | <https://raw.githubusercontent.com/microsoftgraph/microsoft-graph-docs-contrib/bb35043c742d4406d73ccae904c05fbfca7c7428/concepts/delta-query-overview.md> | 2026-11-04 | CC-BY-4.0 | immutable | Delta query in general: token expiry and full resync |
| `graph/outlook-schedule-recurring-events.md` | commit `bb35043` | <https://raw.githubusercontent.com/microsoftgraph/microsoft-graph-docs-contrib/bb35043c742d4406d73ccae904c05fbfca7c7428/concepts/outlook-schedule-recurring-events.md> | 2026-11-04 | CC-BY-4.0 | immutable | Series masters, occurrences and exceptions in Outlook calendars |

## Unicode Licence V3

The notice the Unicode-3.0 licence requires to accompany `cldr/release-49/windowsZones.xml`:

```text
UNICODE LICENSE V3

COPYRIGHT AND PERMISSION NOTICE

Copyright © 1991-2026 Unicode, Inc.

NOTICE TO USER: Carefully read the following legal agreement. BY
DOWNLOADING, INSTALLING, COPYING OR OTHERWISE USING DATA FILES, AND/OR
SOFTWARE, YOU UNEQUIVOCALLY ACCEPT, AND AGREE TO BE BOUND BY, ALL OF THE
TERMS AND CONDITIONS OF THIS AGREEMENT. IF YOU DO NOT AGREE, DO NOT
DOWNLOAD, INSTALL, COPY, DISTRIBUTE OR USE THE DATA FILES OR SOFTWARE.

Permission is hereby granted, free of charge, to any person obtaining a
copy of data files and any associated documentation (the "Data Files") or
software and any associated documentation (the "Software") to deal in the
Data Files or Software without restriction, including without limitation
the rights to use, copy, modify, merge, publish, distribute, and/or sell
copies of the Data Files or Software, and to permit persons to whom the
Data Files or Software are furnished to do so, provided that either (a)
this copyright and permission notice appear with all copies of the Data
Files or Software, or (b) this copyright and permission notice appear in
associated Documentation.

THE DATA FILES AND SOFTWARE ARE PROVIDED "AS IS", WITHOUT WARRANTY OF ANY
KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT OF
THIRD PARTY RIGHTS.

IN NO EVENT SHALL THE COPYRIGHT HOLDER OR HOLDERS INCLUDED IN THIS NOTICE
BE LIABLE FOR ANY CLAIM, OR ANY SPECIAL INDIRECT OR CONSEQUENTIAL DAMAGES,
OR ANY DAMAGES WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS,
WHETHER IN AN ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION,
ARISING OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THE DATA
FILES OR SOFTWARE.

Except as contained in this notice, the name of a copyright holder shall
not be used in advertising or otherwise to promote the sale, use or other
dealings in these Data Files or Software without prior written
authorization of the copyright holder.
```
