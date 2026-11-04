//! Series expansion: daylight-saving gaps and folds, floating and all-day series, extra and
//! excluded dates, overrides, and what is refused with a loss.

mod common;

use calendar_sync_model::{
    EventSeries, Field, Frequency, InstanceKey, InstanceRange, LossReason, NDay, Recurrence,
    RecurrenceRule, RuleEnd, Skip, Weekday,
};
use calendar_sync_recurrence::{CivilRule, Source, Window};
use common::*;

fn rule(frequency: Frequency) -> RecurrenceRule {
    RecurrenceRule::new(frequency)
}

fn counted(frequency: Frequency, count: u32) -> RecurrenceRule {
    let mut rule = rule(frequency);
    rule.end = RuleEnd::Count { count };
    rule
}

const WIDE_START: &str = "1900-01-01T00:00:00Z";
const WIDE_END: &str = "2100-01-01T00:00:00Z";

fn wide() -> Window {
    window(WIDE_START, WIDE_END)
}

#[test]
fn weekly_series_keeps_its_wall_time_across_spring_forward() {
    let series = series(
        zoned("2007-03-04T09:00", "America/New_York"),
        None,
        Recurrence::rule(counted(Frequency::Weekly, 3)),
    );
    let expansion = expand(&series, "UTC", &wide());
    assert!(expansion.losses.is_empty(), "{:?}", expansion.losses);
    assert_eq!(
        starts(&expansion),
        [
            ts("2007-03-04T14:00:00Z"),
            ts("2007-03-11T13:00:00Z"),
            ts("2007-03-18T13:00:00Z"),
        ]
    );
}

#[test]
fn an_instance_in_a_gap_moves_later_by_the_gap_and_still_counts() {
    // Errata 4271 to RFC 5545 section 3.3.10: handled as section 3.3.5 says, with the offset
    // before the gap. 02:30 EST on 11 March 2007 is 03:30 EDT.
    let series = series(
        zoned("2007-03-10T02:30", "America/New_York"),
        None,
        Recurrence::rule(counted(Frequency::Daily, 3)),
    );
    let expansion = expand(&series, "UTC", &wide());
    assert!(expansion.losses.is_empty(), "{:?}", expansion.losses);
    assert_eq!(
        starts(&expansion),
        [
            ts("2007-03-10T07:30:00Z"),
            ts("2007-03-11T07:30:00Z"),
            ts("2007-03-12T06:30:00Z"),
        ]
    );
    // The instance keeps its written civil time as its identity.
    assert_eq!(
        expansion.occurrences[1].key,
        InstanceKey::new(zoned("2007-03-11T02:30", "America/New_York"))
    );
}

#[test]
fn an_instance_in_a_fold_is_the_first_of_the_two() {
    // RFC 5545 section 3.3.5: 01:30 on 4 November 2007 is 01:30 EDT.
    let series = series(
        zoned("2007-11-03T01:30", "America/New_York"),
        None,
        Recurrence::rule(counted(Frequency::Daily, 3)),
    );
    let expansion = expand(&series, "UTC", &wide());
    assert_eq!(
        starts(&expansion),
        [
            ts("2007-11-03T05:30:00Z"),
            ts("2007-11-04T05:30:00Z"),
            ts("2007-11-05T06:30:00Z"),
        ]
    );
}

#[test]
fn hourly_instances_that_meet_in_a_gap_are_one_occurrence() {
    // 02:30 does not exist and lands on 03:30 EDT, which the rule also produces. RFC 5545
    // section 3.8.5.3: where instances duplicate, only one is considered.
    let series = series(
        zoned("2007-03-11T00:30", "America/New_York"),
        None,
        Recurrence::rule(counted(Frequency::Hourly, 4)),
    );
    let expansion = expand(&series, "UTC", &wide());
    assert_eq!(
        starts(&expansion),
        [
            ts("2007-03-11T05:30:00Z"),
            ts("2007-03-11T06:30:00Z"),
            ts("2007-03-11T07:30:00Z"),
        ]
    );
}

#[test]
fn a_timed_series_keeps_its_civil_length_across_a_change() {
    let series = series(
        zoned("2007-03-10T09:00", "America/New_York"),
        Some(zoned("2007-03-10T17:00", "America/New_York")),
        Recurrence::rule(counted(Frequency::Daily, 2)),
    );
    let expansion = expand(&series, "UTC", &wide());
    let second = &expansion.occurrences[1];
    assert_eq!(second.start, ts("2007-03-11T13:00:00Z"));
    assert_eq!(second.end, ts("2007-03-11T21:00:00Z"));
    assert_eq!(
        second.times.end(),
        Some(&zoned("2007-03-11T17:00", "America/New_York"))
    );
}

#[test]
fn a_floating_series_is_placed_in_the_viewers_zone() {
    let series = series(
        floating("2024-06-01T09:00"),
        Some(floating("2024-06-01T10:00")),
        Recurrence::rule(counted(Frequency::Daily, 2)),
    );
    let tokyo = expand(&series, "Asia/Tokyo", &wide());
    let los_angeles = expand(&series, "America/Los_Angeles", &wide());
    assert_eq!(
        starts(&tokyo),
        [ts("2024-06-01T00:00:00Z"), ts("2024-06-02T00:00:00Z")]
    );
    assert_eq!(
        starts(&los_angeles),
        [ts("2024-06-01T16:00:00Z"), ts("2024-06-02T16:00:00Z")]
    );
    // The values themselves stay floating.
    assert_eq!(
        start_values(&tokyo),
        [floating("2024-06-01T09:00"), floating("2024-06-02T09:00")]
    );
}

#[test]
fn an_all_day_series_spans_whole_days_in_the_viewers_zone() {
    let series = series(
        day("2024-07-04"),
        Some(day("2024-07-05")),
        Recurrence::rule(counted(Frequency::Yearly, 2)),
    );
    let expansion = expand(&series, "Europe/Berlin", &wide());
    assert_eq!(
        start_values(&expansion),
        [day("2024-07-04"), day("2025-07-04")]
    );
    let first = &expansion.occurrences[0];
    assert_eq!(first.start, ts("2024-07-03T22:00:00Z"));
    assert_eq!(first.end, ts("2024-07-04T22:00:00Z"));
    assert_eq!(first.times.end(), Some(&day("2024-07-05")));

    // A window ending as the day starts in Berlin misses it.
    let before = window("2024-07-01T00:00:00Z", "2024-07-03T22:00:00Z");
    assert!(
        expand(&series, "Europe/Berlin", &before)
            .occurrences
            .is_empty()
    );
}

#[test]
fn a_leap_day_anniversary_follows_rfc_7529_section_4_3_4() {
    let mut forward = rule(Frequency::Yearly);
    forward.rscale = Some("gregorian".into());
    forward.skip = Skip::Forward;
    let series_forward = series(day("2012-02-29"), None, Recurrence::rule(forward));
    let series_plain = series(
        day("2012-02-29"),
        None,
        Recurrence::rule(rule(Frequency::Yearly)),
    );
    let years = window("2012-01-01T00:00:00Z", "2017-12-31T00:00:00Z");
    assert_eq!(
        start_values(&expand(&series_forward, "UTC", &years)),
        [
            day("2012-02-29"),
            day("2013-03-01"),
            day("2014-03-01"),
            day("2015-03-01"),
            day("2016-02-29"),
            day("2017-03-01"),
        ]
    );
    assert_eq!(
        start_values(&expand(&series_plain, "UTC", &years)),
        [day("2012-02-29"), day("2016-02-29")]
    );
}

#[test]
fn skip_moves_a_missing_month_day_backward_or_forward() {
    let monthly = |skip: Skip| {
        let mut rule = counted(Frequency::Monthly, 4);
        rule.skip = skip;
        CivilRule::new(&rule, dt("2024-01-31T10:00"), None)
            .unwrap()
            .instances(None, dt("2100-01-01T00:00"))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        monthly(Skip::Backward),
        [
            dt("2024-01-31T10:00"),
            dt("2024-02-29T10:00"),
            dt("2024-03-31T10:00"),
            dt("2024-04-30T10:00"),
        ]
    );
    assert_eq!(
        monthly(Skip::Forward),
        [
            dt("2024-01-31T10:00"),
            dt("2024-03-01T10:00"),
            dt("2024-03-31T10:00"),
            dt("2024-05-01T10:00"),
        ]
    );
    assert_eq!(
        monthly(Skip::Omit),
        [
            dt("2024-01-31T10:00"),
            dt("2024-03-31T10:00"),
            dt("2024-05-31T10:00"),
            dt("2024-07-31T10:00"),
        ]
    );
}

#[test]
fn excluded_and_extra_dates_match_by_instant_whatever_their_form() {
    let mut recurrence = Recurrence::rule(counted(Frequency::Daily, 5));
    // 09:00 UTC is 10:00 in Berlin in January.
    recurrence
        .excluded_dates
        .push(instant("2024-01-02T09:00:00Z"));
    recurrence
        .excluded_dates
        .push(zoned("2024-01-04T10:00", "Europe/Berlin"));
    recurrence
        .dates
        .push(zoned("2024-01-10T12:00", "Europe/Berlin"));
    // A duplicate of a rule instance is kept once.
    recurrence.dates.push(instant("2024-01-03T09:00:00Z"));
    let series = series(zoned("2024-01-01T10:00", "Europe/Berlin"), None, recurrence);
    let expansion = expand(&series, "UTC", &wide());
    assert!(expansion.losses.is_empty(), "{:?}", expansion.losses);
    assert_eq!(
        starts(&expansion),
        [
            ts("2024-01-01T09:00:00Z"),
            ts("2024-01-03T09:00:00Z"),
            ts("2024-01-05T09:00:00Z"),
            ts("2024-01-10T11:00:00Z"),
        ]
    );
}

#[test]
fn an_exclusion_rule_removes_its_instances() {
    let mut recurrence = Recurrence::rule(counted(Frequency::Daily, 7));
    let mut weekends = rule(Frequency::Weekly);
    weekends.by_day = vec![NDay::every(Weekday::Saturday), NDay::every(Weekday::Sunday)];
    recurrence.exclusion_rules.push(weekends);
    // 2024-01-01 is a Monday.
    let series = series(floating("2024-01-01T09:00"), None, recurrence);
    let expansion = expand(&series, "UTC", &wide());
    assert_eq!(expansion.occurrences.len(), 5);
}

fn weekly_meeting() -> EventSeries {
    series(
        zoned("2024-01-01T10:00", "Europe/Berlin"),
        Some(zoned("2024-01-01T11:00", "Europe/Berlin")),
        Recurrence::rule(counted(Frequency::Weekly, 6)),
    )
}

#[test]
fn overrides_move_cancel_and_carry_forward() {
    let mut series = weekly_meeting();
    modified(
        &mut series,
        zoned("2024-01-08T10:00", "Europe/Berlin"),
        zoned("2024-01-09T14:00", "Europe/Berlin"),
        Some(zoned("2024-01-09T15:00", "Europe/Berlin")),
        InstanceRange::ThisInstance,
    );
    cancelled(&mut series, zoned("2024-01-15T10:00", "Europe/Berlin"));
    modified(
        &mut series,
        zoned("2024-01-22T10:00", "Europe/Berlin"),
        zoned("2024-01-22T12:00", "Europe/Berlin"),
        Some(zoned("2024-01-22T13:30", "Europe/Berlin")),
        InstanceRange::ThisAndFuture,
    );
    let expansion = expand(&series, "UTC", &wide());
    assert!(expansion.losses.is_empty(), "{:?}", expansion.losses);
    assert_eq!(
        starts(&expansion),
        [
            ts("2024-01-01T09:00:00Z"),
            ts("2024-01-09T13:00:00Z"),
            ts("2024-01-22T11:00:00Z"),
            ts("2024-01-29T11:00:00Z"),
            ts("2024-02-05T11:00:00Z"),
        ]
    );
    let last = expansion.occurrences.last().unwrap();
    assert_eq!(last.end, ts("2024-02-05T12:30:00Z"));
    assert_eq!(
        last.key,
        InstanceKey::new(zoned("2024-02-05T10:00", "Europe/Berlin"))
    );
    assert_eq!(
        last.source,
        Source::Override {
            key: InstanceKey::new(zoned("2024-01-22T10:00", "Europe/Berlin")),
            range: InstanceRange::ThisAndFuture,
        }
    );
    assert_eq!(last.event.ids.id, "override");
    assert_eq!(expansion.occurrences[0].source, Source::Master);
}

#[test]
fn a_utc_recurrence_id_overrides_a_zoned_instance() {
    let mut series = weekly_meeting();
    modified(
        &mut series,
        instant("2024-01-08T09:00:00Z"),
        zoned("2024-01-08T16:00", "Europe/Berlin"),
        None,
        InstanceRange::ThisInstance,
    );
    let expansion = expand(&series, "UTC", &wide());
    assert_eq!(expansion.occurrences.len(), 6);
    assert_eq!(expansion.occurrences[1].start, ts("2024-01-08T15:00:00Z"));
}

#[test]
fn an_override_moving_an_instance_into_the_window_is_found() {
    let mut series = weekly_meeting();
    modified(
        &mut series,
        zoned("2024-01-01T10:00", "Europe/Berlin"),
        zoned("2024-03-01T10:00", "Europe/Berlin"),
        None,
        InstanceRange::ThisInstance,
    );
    let march = window("2024-03-01T00:00:00Z", "2024-03-02T00:00:00Z");
    let expansion = expand(&series, "UTC", &march);
    assert_eq!(starts(&expansion), [ts("2024-03-01T09:00:00Z")]);
}

#[test]
fn an_orphan_override_is_shown_and_one_of_an_excluded_instance_is_not() {
    let mut series = weekly_meeting();
    if let Some(master) = &mut series.master
        && let Some(recurrence) = &mut master.fields.recurrence
    {
        recurrence
            .excluded_dates
            .push(zoned("2024-01-08T10:00", "Europe/Berlin"));
    }
    modified(
        &mut series,
        zoned("2024-01-08T10:00", "Europe/Berlin"),
        zoned("2024-01-08T12:00", "Europe/Berlin"),
        None,
        InstanceRange::ThisInstance,
    );
    modified(
        &mut series,
        zoned("2024-01-03T10:00", "Europe/Berlin"),
        zoned("2024-01-03T10:00", "Europe/Berlin"),
        None,
        InstanceRange::ThisInstance,
    );
    let expansion = expand(&series, "UTC", &wide());
    assert_eq!(
        starts(&expansion),
        [
            ts("2024-01-01T09:00:00Z"),
            ts("2024-01-03T09:00:00Z"),
            ts("2024-01-15T09:00:00Z"),
            ts("2024-01-22T09:00:00Z"),
            ts("2024-01-29T09:00:00Z"),
            ts("2024-02-05T09:00:00Z"),
        ]
    );
}

#[test]
fn a_series_without_a_master_shows_its_overrides() {
    let mut series = EventSeries {
        master: None,
        overrides: empty_overrides(),
    };
    modified(
        &mut series,
        zoned("2024-01-08T10:00", "Europe/Berlin"),
        zoned("2024-01-08T10:00", "Europe/Berlin"),
        None,
        InstanceRange::ThisInstance,
    );
    cancelled(&mut series, zoned("2024-01-15T10:00", "Europe/Berlin"));
    let expansion = expand(&series, "UTC", &wide());
    assert_eq!(starts(&expansion), [ts("2024-01-08T09:00:00Z")]);
}

#[test]
fn an_unsupported_calendar_scale_yields_nothing_and_a_loss() {
    let mut hebrew = rule(Frequency::Yearly);
    hebrew.rscale = Some("hebrew".into());
    let mut series = series(day("2014-02-08"), None, Recurrence::rule(hebrew));
    modified(
        &mut series,
        day("2015-02-27"),
        day("2015-02-28"),
        None,
        InstanceRange::ThisInstance,
    );
    let expansion = expand(&series, "UTC", &wide());
    assert!(expansion.occurrences.is_empty());
    assert_eq!(expansion.losses.len(), 1);
    assert_eq!(expansion.losses[0].field, Field::Recurrence);
    assert_eq!(expansion.losses[0].reason, LossReason::Unsupported);
    assert!(expansion.losses[0].detail.contains("RSCALE=hebrew"));
}

#[test]
fn a_rule_this_library_cannot_read_yields_nothing_and_a_loss() {
    let mut weekly = rule(Frequency::Weekly);
    weekly.by_month_day = vec![1];
    let series = series(floating("2024-01-01T09:00"), None, Recurrence::rule(weekly));
    let expansion = expand(&series, "UTC", &wide());
    assert!(expansion.occurrences.is_empty());
    assert_eq!(expansion.losses[0].reason, LossReason::Malformed);

    let mut hourly = rule(Frequency::Hourly);
    hourly.end = RuleEnd::Count { count: 2 };
    let all_day = series_of_day(hourly);
    let expansion = expand(&all_day, "UTC", &wide());
    assert!(expansion.occurrences.is_empty());
    assert_eq!(expansion.losses[0].reason, LossReason::Malformed);
}

#[test]
fn an_all_day_series_ignores_times_of_day_and_says_so() {
    // RFC 5545 section 3.3.10: BYHOUR, BYMINUTE and BYSECOND with a DATE start "MUST be
    // ignored".
    let mut daily = counted(Frequency::Daily, 2);
    daily.by_hour = vec![9, 17];
    let series = series_of_day(daily);
    let expansion = expand(&series, "UTC", &wide());
    assert_eq!(
        start_values(&expansion),
        [day("2024-01-01"), day("2024-01-02")]
    );
    assert_eq!(expansion.losses.len(), 1);
    assert!(expansion.losses[0].detail.contains("ignored"));
}

fn series_of_day(rule: RecurrenceRule) -> EventSeries {
    series(day("2024-01-01"), None, Recurrence::rule(rule))
}

#[test]
fn an_unknown_time_zone_yields_nothing_and_a_loss() {
    let series = series(
        zoned("2024-01-01T10:00", "Mars/Olympus_Mons"),
        None,
        Recurrence::rule(counted(Frequency::Daily, 2)),
    );
    let expansion = expand(&series, "UTC", &wide());
    assert!(expansion.occurrences.is_empty());
    assert_eq!(expansion.losses[0].field, Field::Times);
    assert!(expansion.losses[0].detail.contains("Mars/Olympus_Mons"));
}

#[test]
fn zones_may_come_from_a_closure() {
    let series = series(
        zoned("2024-01-01T10:00", "Office"),
        None,
        Recurrence::rule(counted(Frequency::Daily, 2)),
    );
    let office =
        |id: &calendar_sync_model::TimeZoneId| (id.as_str() == "Office").then(|| tz("Asia/Tokyo"));
    let expansion =
        calendar_sync_recurrence::Expander::new(&office, tz("UTC")).expand(&series, &wide());
    assert_eq!(
        starts(&expansion),
        [ts("2024-01-01T01:00:00Z"), ts("2024-01-02T01:00:00Z")]
    );
}

#[test]
fn an_extra_date_of_another_form_is_left_out_and_reported() {
    let mut recurrence = Recurrence::rule(counted(Frequency::Daily, 2));
    recurrence.dates.push(day("2024-01-10"));
    let series = series(floating("2024-01-01T09:00"), None, recurrence);
    let expansion = expand(&series, "UTC", &wide());
    assert_eq!(expansion.occurrences.len(), 2);
    assert_eq!(expansion.losses[0].reason, LossReason::Malformed);
}

#[test]
fn an_endless_series_is_bounded_by_the_window() {
    let series = series(
        instant("1970-01-01T00:00:00Z"),
        None,
        Recurrence::rule(rule(Frequency::Daily)),
    );
    let days = window("2026-01-01T00:00:00Z", "2026-01-04T00:00:00Z");
    assert_eq!(
        starts(&expand(&series, "UTC", &days)),
        [
            ts("2026-01-01T00:00:00Z"),
            ts("2026-01-02T00:00:00Z"),
            ts("2026-01-03T00:00:00Z"),
        ]
    );
}

#[test]
fn a_counted_series_counts_from_its_start_whatever_the_window() {
    // 20,000 days from 1970-01-01 ends on 2024-10-03.
    let series = series(
        instant("1970-01-01T00:00:00Z"),
        None,
        Recurrence::rule(counted(Frequency::Daily, 20_000)),
    );
    let inside = window("2024-10-03T00:00:00Z", "2024-10-06T00:00:00Z");
    assert_eq!(
        starts(&expand(&series, "UTC", &inside)),
        [ts("2024-10-03T00:00:00Z")]
    );
}

#[test]
fn the_start_counts_as_the_first_instance_even_off_the_rule() {
    // RFC 5545 section 3.3.10: "The DTSTART property value always counts as the first
    // occurrence." 2024-01-02 is a Tuesday; the rule produces Mondays.
    let mut mondays = counted(Frequency::Weekly, 3);
    mondays.by_day = vec![NDay::every(Weekday::Monday)];
    let series = series(
        floating("2024-01-02T09:00"),
        None,
        Recurrence::rule(mondays),
    );
    assert_eq!(
        start_values(&expand(&series, "UTC", &wide())),
        [
            floating("2024-01-02T09:00"),
            floating("2024-01-08T09:00"),
            floating("2024-01-15T09:00"),
        ]
    );
}

#[test]
fn an_expansion_too_long_to_finish_is_reported() {
    let series = series(
        instant("1970-01-01T00:00:00Z"),
        None,
        Recurrence::rule(counted(Frequency::Secondly, u32::MAX)),
    );
    let days = window("2026-01-01T00:00:00Z", "2026-01-02T00:00:00Z");
    let expansion = expand(&series, "UTC", &days);
    assert!(expansion.occurrences.is_empty());
    assert_eq!(expansion.losses[0].reason, LossReason::Unsupported);
    assert!(expansion.losses[0].detail.contains("expansion stopped"));
}
