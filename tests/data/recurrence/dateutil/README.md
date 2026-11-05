# python-dateutil rrule cases

`cases.json` holds the test cases of python-dateutil's `RRuleTest`, ported as data. The crate test `crates/calendar-sync-recurrence/tests/dateutil.rs` expands each one and compares.

| | |
|---|---|
| Source | <https://github.com/dateutil/dateutil>, `tests/test_rrule.py` |
| Tag | `2.9.0.post0`, commit `1ae807774053c071acc9e7d3d27778fba0a7773e` |
| SHA-256 of `test_rrule.py` | `6b8336530d7954f05df72436c3aeef5acb3060587739bdade3c48a02a53001ea` |
| Licence | BSD-3-Clause, and Apache-2.0 for contributions after 2017-12-01. `LICENSE` is dateutil's, unchanged |

## Regenerating

```sh
python3 -I tests/data/recurrence/dateutil/port.py path/to/test_rrule.py > tests/data/recurrence/dateutil/cases.json
```

`port.py` parses the test file and never runs it. It keeps each method whose whole body is `self.assertEqual(list(rrule(...)), [datetime(...), ...])` and whose arguments are all literals. It skips `byeaster`, which is not RFC 5545, and any case with a time zone. 214 of the 242 such methods are ported.

## Where this library differs

dateutil expands some rules RFC 5545 forbids. `divergences.json` names each one, the RFC 5545 section 3.3.10 requirement it breaks, and the loss this library reports instead of instances. Every other ported case must agree exactly.
