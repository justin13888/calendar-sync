# Security policy

## Reporting a vulnerability

Report vulnerabilities privately, through GitHub's private vulnerability reporting: open the repository's [Security tab](https://github.com/justin13888/calendar-sync/security) and choose **Report a vulnerability**, or go straight to [the advisory form](https://github.com/justin13888/calendar-sync/security/advisories/new). Only the maintainers can read the report.

Do not open a public issue, pull request or discussion about a vulnerability. If the private form is unavailable, open an issue that asks for a private contact and contains no details of the vulnerability.

Include what you can of:

- the affected crate and version, or the commit
- what an attacker can do, and under what conditions
- the steps or input that reproduce it, such as an iCalendar file or a provider response

## What happens next

The maintainer acknowledges the report, confirms or rules out the vulnerability, and agrees a disclosure date with you. A confirmed vulnerability is fixed in a new release of each affected crate, then disclosed in a published GitHub security advisory that credits you unless you ask otherwise.

## Supported versions

Nothing is published yet. Once crates are published, fixes go into the latest release of each affected crate; older versions are not patched.

## Scope

calendar-sync parses untrusted input: iCalendar data, recurrence rules, and responses from Google Calendar, Microsoft Graph and CalDAV servers. A panic, unbounded memory use or unbounded work caused by such input is in scope, and so is any way of making the sync engine write events to the wrong calendar or account.

The application supplies storage, HTTP, credentials and scheduling, so how it stores credentials or tokens is outside this library. A vulnerability in a dependency belongs to that dependency's project, unless calendar-sync uses the dependency in a way that makes it exploitable.
