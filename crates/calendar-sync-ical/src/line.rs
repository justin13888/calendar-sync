//! Content lines: reading names, parameters and values off parsed lines, and building new ones.
//!
//! Lines are read and written through `ical`'s byte-faithful syntax tree. A line this crate
//! builds is written as its logical text and handed to the tokeniser, so it carries no wire
//! layout and is folded at 75 octets on output (RFC 5545 section 3.1).

use ical::tree::cst::{IcalCst, IcalItem};
use ical::tree::line::IcalLine;

/// The property name without any group prefix.
pub(crate) fn name<'l>(line: &'l IcalLine<'_>) -> &'l str {
    let name = line.name.get();
    name.rsplit_once('.').map_or(name, |(_, name)| name)
}

/// Whether the line is the property `wanted`, in any case.
pub(crate) fn is(line: &IcalLine<'_>, wanted: &str) -> bool {
    name(line).eq_ignore_ascii_case(wanted)
}

/// The first value of the first parameter named `wanted`, unquoted and RFC 6868 decoded.
pub(crate) fn param(line: &IcalLine<'_>, wanted: &str) -> Option<String> {
    line.params
        .iter()
        .find(|param| param.name.get().eq_ignore_ascii_case(wanted))
        .map(|param| {
            param
                .values
                .first()
                .map_or_else(String::new, |value| decode_param_value(value.get()))
        })
}

/// Whether the line has a parameter named `wanted`.
pub(crate) fn has_param(line: &IcalLine<'_>, wanted: &str) -> bool {
    line.params
        .iter()
        .any(|param| param.name.get().eq_ignore_ascii_case(wanted))
}

/// The line's value, unescaped (RFC 5545 section 3.3.11).
pub(crate) fn text(line: &IcalLine<'_>) -> String {
    line.value.decode().into_owned()
}

/// The line's value as written, still escaped.
pub(crate) fn raw(line: &IcalLine<'_>) -> String {
    line.raw_value_str().into_owned()
}

/// The line as one logical content line, without its line ending: its name, parameters and
/// value exactly as written, unfolded.
pub(crate) fn logical(line: &IcalLine<'_>) -> String {
    let mut out = String::from(line.name.get());
    for param in &line.params {
        out.push(';');
        out.push_str(param.name.get());
        if !param.values.is_empty() {
            out.push('=');
            let values: Vec<&str> = param
                .values
                .iter()
                .map(ical::tree::leaf::IcalLeaf::get)
                .collect();
            out.push_str(&values.join(","));
        }
    }
    out.push(':');
    out.push_str(&line.raw_value_str());
    out
}

/// A parameter value with its quotes removed and RFC 6868's caret escapes decoded.
fn decode_param_value(raw: &str) -> String {
    let unquoted = raw
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(raw);
    let mut out = String::with_capacity(unquoted.len());
    let mut chars = unquoted.chars();
    while let Some(c) = chars.next() {
        if c != '^' {
            out.push(c);
            continue;
        }
        match chars.next() {
            // `^^`, and a caret ending the value.
            Some('^') | None => out.push('^'),
            Some('n' | 'N') => out.push('\n'),
            Some('\'') => out.push('"'),
            // RFC 6868 section 3: any other caret is left as it is.
            Some(other) => {
                out.push('^');
                out.push(other);
            }
        }
    }
    out
}

/// A parameter value as written: RFC 6868 caret escapes, quoted where it holds `:`, `;` or `,`.
pub(crate) fn encode_param_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '^' => out.push_str("^^"),
            '"' => out.push_str("^'"),
            '\r' => {
                if chars.peek() != Some(&'\n') {
                    out.push_str("^n");
                }
            }
            '\n' => out.push_str("^n"),
            c if c.is_control() && c != '\t' => {}
            c => out.push(c),
        }
    }
    if out.contains([':', ';', ',']) {
        format!("\"{out}\"")
    } else {
        out
    }
}

/// A text value as written: RFC 5545 section 3.3.11 escapes, every line break as `\n`.
pub(crate) fn escape_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push_str("\\n");
            }
            '\n' => out.push_str("\\n"),
            c if c.is_control() && c != '\t' => {}
            c => out.push(c),
        }
    }
    out
}

/// A new line: `name`, the given parameters (values encoded here), and a value already escaped.
pub(crate) fn build(name: &str, params: &[(&str, String)], value: &str) -> IcalLine<'static> {
    let mut text = String::from(name);
    for (param, param_value) in params {
        text.push(';');
        text.push_str(param);
        text.push('=');
        text.push_str(&encode_param_value(param_value));
    }
    text.push(':');
    text.push_str(value);
    tokenise(&text)
}

/// A replacement for `old`: its name as written and every parameter it had, except those named in
/// `set`, which take the given value or are removed when it is `None`, and the transfer
/// parameters `ENCODING` and `CHARSET`, which described the old value's bytes. `value` is already
/// escaped.
pub(crate) fn rewrite(
    old: &IcalLine<'_>,
    set: &[(&str, Option<String>)],
    value: &str,
) -> IcalLine<'static> {
    let mut text = String::from(old.name.get());
    let mut done = vec![false; set.len()];
    for param in &old.params {
        let param_name = param.name.get();
        if let Some(at) = set
            .iter()
            .position(|(name, _)| name.eq_ignore_ascii_case(param_name))
        {
            if let (false, Some(new)) = (done[at], &set[at].1) {
                text.push(';');
                text.push_str(param_name);
                text.push('=');
                text.push_str(&encode_param_value(new));
            }
            done[at] = true;
            continue;
        }
        if param_name.eq_ignore_ascii_case("ENCODING") || param_name.eq_ignore_ascii_case("CHARSET")
        {
            continue;
        }
        text.push(';');
        text.push_str(param_name);
        if !param.values.is_empty() {
            text.push('=');
            let values: Vec<&str> = param
                .values
                .iter()
                .map(ical::tree::leaf::IcalLeaf::get)
                .collect();
            text.push_str(&values.join(","));
        }
    }
    for ((name, new), done) in set.iter().zip(done) {
        if let (false, Some(new)) = (done, new) {
            text.push(';');
            text.push_str(name);
            text.push('=');
            text.push_str(&encode_param_value(new));
        }
    }
    text.push(':');
    text.push_str(value);
    tokenise(&text)
}

/// Tokenises one logical line this crate wrote.
fn tokenise(text: &str) -> IcalLine<'static> {
    let mut bytes = Vec::with_capacity(text.len() + 2);
    bytes.extend_from_slice(text.as_bytes());
    bytes.extend_from_slice(b"\r\n");
    // A single line parses as a bare record holding just that line, which owns its bytes once
    // detached from the buffer.
    let parsed = IcalCst::parse(&bytes).map(IcalCst::into_static);
    match parsed.map(|record| record.items.into_iter().next()) {
        Ok(Some(IcalItem::Prop(line))) => line,
        // Every line built here has a UTF-8 name followed by a colon, which is all the tokeniser
        // asks of a line. Should that ever fail, the value is kept as an unstructured line.
        _ => IcalLine::text(String::from("X-CALENDAR-SYNC-UNWRITABLE"), text.to_owned()),
    }
}

/// The positions of the component's direct property lines named `wanted`, in order.
pub(crate) fn positions(component: &IcalCst<'_>, wanted: &str) -> Vec<usize> {
    component
        .items
        .iter()
        .enumerate()
        .filter_map(|(at, item)| match item {
            IcalItem::Prop(line) if is(line, wanted) => Some(at),
            _ => None,
        })
        .collect()
}

/// The component's first direct property line named `wanted`.
pub(crate) fn first<'c, 'a>(component: &'c IcalCst<'a>, wanted: &str) -> Option<&'c IcalLine<'a>> {
    component.items.iter().find_map(|item| match item {
        IcalItem::Prop(line) if is(line, wanted) => Some(line),
        _ => None,
    })
}

/// The name a component's `BEGIN` line gives it, or `""` for a bare record.
pub(crate) fn component_name<'c>(component: &'c IcalCst<'_>) -> std::borrow::Cow<'c, str> {
    component
        .begin
        .as_ref()
        .map_or(std::borrow::Cow::Borrowed(""), IcalLine::raw_value_str)
}

/// Whether the component is a `name` component, in any case.
pub(crate) fn component_is(component: &IcalCst<'_>, name: &str) -> bool {
    component_name(component).eq_ignore_ascii_case(name)
}

/// Removes the component's direct property lines named `wanted`.
pub(crate) fn remove_all(component: &mut IcalCst<'_>, wanted: &str) {
    component.items.retain(|item| match item {
        IcalItem::Prop(line) => !is(line, wanted),
        IcalItem::Component(_) | IcalItem::Opaque(_) => true,
    });
}

/// Sets a single-valued property: replaces the first line named `wanted` (keeping its other
/// parameters), adds one where there is none, or removes every such line when `value` is `None`.
/// `value` is already escaped.
pub(crate) fn set_single(
    component: &mut IcalCst<'static>,
    wanted: &str,
    params: &[(&str, Option<String>)],
    value: Option<&str>,
) {
    let Some(value) = value else {
        remove_all(component, wanted);
        return;
    };
    match positions(component, wanted).first() {
        Some(&at) => {
            if let IcalItem::Prop(old) = &component.items[at] {
                let new = rewrite(old, params, value);
                component.items[at] = IcalItem::Prop(new);
            }
        }
        None => {
            let params: Vec<(&str, String)> = params
                .iter()
                .filter_map(|(name, value)| value.clone().map(|value| (*name, value)))
                .collect();
            component.push_line(build(wanted, &params, value));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn param_values_decode_quotes_and_carets() {
        assert_eq!(decode_param_value("\"a;b\""), "a;b");
        assert_eq!(decode_param_value("x^^y^nz^'q^"), "x^y\nz\"q^");
        assert_eq!(decode_param_value("^x"), "^x");
    }

    #[test]
    fn param_values_encode_and_decode_back() {
        for value in [
            "plain",
            "with: colon",
            "a;b,c",
            "caret ^ and \"quote\"",
            "two\nlines",
        ] {
            assert_eq!(decode_param_value(&encode_param_value(value)), value);
        }
    }

    #[test]
    fn text_is_escaped() {
        assert_eq!(escape_text("a;b,c\\d\r\ne\nf"), "a\\;b\\,c\\\\d\\ne\\nf");
    }

    #[test]
    fn rewrite_keeps_unmanaged_parameters_and_drops_transfer_ones() {
        let (old, _) = IcalLine::take(
            b"ATTENDEE;X-KEEP=1;PARTSTAT=NEEDS-ACTION;CHARSET=UTF-8:mailto:a@example.com\r\n",
        )
        .unwrap();
        let new = rewrite(
            &old,
            &[
                ("PARTSTAT", Some("ACCEPTED".into())),
                ("CN", Some("A, B".into())),
            ],
            "mailto:a@example.com",
        );
        assert_eq!(
            logical(&new),
            "ATTENDEE;X-KEEP=1;PARTSTAT=ACCEPTED;CN=\"A, B\":mailto:a@example.com"
        );
    }
}
