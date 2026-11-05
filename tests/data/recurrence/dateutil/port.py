"""Port python-dateutil's rrule test cases to JSON data.

Usage: python3 -I port.py PATH/TO/test_rrule.py > cases.json

The test file is parsed, never imported or executed. Only methods of `RRuleTest` whose whole
body is one `self.assertEqual(list(rrule(...)), [datetime(...), ...])` are ported, and only
when every argument is a literal this script understands. `byeaster` is not RFC 5545 and is
skipped, as is any case with a time zone. Each rule is written in the serialized form of
`calendar_sync_model::RecurrenceRule`, so the test deserializes it directly.
"""

import ast
import json
import sys

FREQUENCIES = {
    "YEARLY": "yearly",
    "MONTHLY": "monthly",
    "WEEKLY": "weekly",
    "DAILY": "daily",
    "HOURLY": "hourly",
    "MINUTELY": "minutely",
    "SECONDLY": "secondly",
}
WEEKDAYS = {"MO": "mo", "TU": "tu", "WE": "we", "TH": "th", "FR": "fr", "SA": "sa", "SU": "su"}
WEEKDAY_NUMBERS = ["mo", "tu", "we", "th", "fr", "sa", "su"]
INT_LISTS = {
    "bymonthday": "byMonthDay",
    "byyearday": "byYearDay",
    "byweekno": "byWeekNo",
    "byhour": "byHour",
    "byminute": "byMinute",
    "bysecond": "bySecond",
    "bysetpos": "bySetPosition",
}


class Unported(Exception):
    """A case this script does not port."""


def integer(node):
    if isinstance(node, ast.Constant) and type(node.value) is int:
        return node.value
    if isinstance(node, ast.UnaryOp) and isinstance(node.op, ast.USub):
        return -integer(node.operand)
    raise Unported("not an integer literal")


def items(node):
    if isinstance(node, (ast.Tuple, ast.List)):
        return list(node.elts)
    return [node]


def date_time(node):
    if not (isinstance(node, ast.Call) and isinstance(node.func, ast.Name)):
        raise Unported("not a call")
    if node.func.id != "datetime" or node.keywords:
        raise Unported("not a naive datetime")
    parts = [integer(arg) for arg in node.args] + [0, 0, 0]
    year, month, day, hour, minute, second = parts[:6]
    return f"{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}"


def weekday(node):
    if isinstance(node, ast.Name) and node.id in WEEKDAYS:
        return {"day": WEEKDAYS[node.id]}
    if (
        isinstance(node, ast.Call)
        and isinstance(node.func, ast.Name)
        and node.func.id in WEEKDAYS
        and len(node.args) == 1
        and not node.keywords
    ):
        return {"day": WEEKDAYS[node.func.id], "nthOfPeriod": integer(node.args[0])}
    if isinstance(node, ast.Constant) and type(node.value) is int:
        return {"day": WEEKDAY_NUMBERS[node.value]}
    raise Unported("not a weekday")


def rule(call):
    if not (isinstance(call, ast.Call) and isinstance(call.func, ast.Name)):
        raise Unported("not a call")
    if call.func.id != "rrule" or len(call.args) != 1:
        raise Unported("not rrule(FREQ, ...)")
    frequency = call.args[0]
    if not (isinstance(frequency, ast.Name) and frequency.id in FREQUENCIES):
        raise Unported("unknown frequency")
    out = {"frequency": FREQUENCIES[frequency.id]}
    start = None
    for keyword in call.keywords:
        name, value = keyword.arg, keyword.value
        if name == "dtstart":
            start = date_time(value)
        elif name == "count":
            out["end"] = {"type": "count", "count": integer(value)}
        elif name == "until":
            out["end"] = {
                "type": "until",
                "until": {"type": "floating", "dateTime": date_time(value)},
            }
        elif name == "interval":
            out["interval"] = integer(value)
        elif name == "wkst":
            out["firstDayOfWeek"] = weekday(value)["day"]
        elif name == "bymonth":
            out["byMonth"] = [{"month": integer(v)} for v in items(value)]
        elif name == "byweekday":
            out["byDay"] = [weekday(v) for v in items(value)]
        elif name in INT_LISTS:
            out[INT_LISTS[name]] = [integer(v) for v in items(value)]
        elif name == "cache":
            pass
        else:
            raise Unported(f"argument {name}")
    if start is None:
        raise Unported("no dtstart")
    return out, start


def case(method):
    if len(method.body) != 1 or not isinstance(method.body[0], ast.Expr):
        raise Unported("not a single statement")
    call = method.body[0].value
    if not (
        isinstance(call, ast.Call)
        and isinstance(call.func, ast.Attribute)
        and call.func.attr == "assertEqual"
        and len(call.args) == 2
    ):
        raise Unported("not assertEqual")
    produced, expected = call.args
    if not (
        isinstance(produced, ast.Call)
        and isinstance(produced.func, ast.Name)
        and produced.func.id == "list"
        and len(produced.args) == 1
    ):
        raise Unported("not list(rrule(...))")
    parsed, start = rule(produced.args[0])
    if not isinstance(expected, ast.List):
        raise Unported("expected value is not a list")
    return {
        "name": method.name,
        "start": start,
        "rule": parsed,
        "expected": [date_time(node) for node in expected.elts],
    }


def main(path):
    with open(path, encoding="utf-8") as source:
        tree = ast.parse(source.read(), filename=path)
    cases = []
    for node in tree.body:
        if isinstance(node, ast.ClassDef) and node.name == "RRuleTest":
            for method in node.body:
                if isinstance(method, ast.FunctionDef) and method.name.startswith("test"):
                    try:
                        cases.append(case(method))
                    except Unported:
                        pass
    json.dump({"cases": cases}, sys.stdout, indent=1)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main(sys.argv[1])
