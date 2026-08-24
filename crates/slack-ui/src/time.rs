//! Turning Slack timestamps into the short forms a transcript needs.
//!
//! Slack sends epoch seconds. What a reader wants depends on distance: today
//! needs a clock time, this week needs a weekday, anything older needs a date.

use chrono::{DateTime, Datelike, Duration, Local, TimeZone};

use slack_api::models::Ts;

/// `14:32` — the time shown beside a message.
pub fn clock(ts: &Ts) -> String {
    match local(ts) {
        Some(dt) => dt.format("%H:%M").to_string(),
        None => String::new(),
    }
}

/// The separator that introduces a new day in the transcript.
pub fn day_heading(ts: &Ts) -> String {
    let Some(dt) = local(ts) else {
        return String::new();
    };
    let today = Local::now().date_naive();
    let day = dt.date_naive();

    if day == today {
        "Today".to_string()
    } else if day == today - Duration::days(1) {
        "Yesterday".to_string()
    } else if today.signed_duration_since(day).num_days() < 7 {
        dt.format("%A").to_string()
    } else if day.year() == today.year() {
        dt.format("%A, %-d %B").to_string()
    } else {
        dt.format("%-d %B %Y").to_string()
    }
}

/// The compact stamp on a sidebar row or a search hit.
pub fn relative(ts: &Ts) -> String {
    let Some(dt) = local(ts) else {
        return String::new();
    };
    let today = Local::now().date_naive();
    let day = dt.date_naive();

    if day == today {
        dt.format("%H:%M").to_string()
    } else if day == today - Duration::days(1) {
        "Yesterday".to_string()
    } else if today.signed_duration_since(day).num_days() < 7 {
        dt.format("%a").to_string()
    } else {
        dt.format("%d/%m/%Y").to_string()
    }
}

/// The unambiguous form, for a tooltip over a short one.
///
/// A transcript shows `14:32`, which is only readable next to a day heading
/// that may be far up the pane; this is what the reader checks against.
pub fn full(ts: &Ts) -> String {
    match local(ts) {
        Some(dt) => dt.format("%A, %-d %B %Y at %H:%M:%S").to_string(),
        None => String::new(),
    }
}

/// `until 15:00` for a snooze that is already running.
pub fn until_clock(epoch_seconds: i64) -> String {
    match Local.timestamp_opt(epoch_seconds, 0).single() {
        Some(dt) => dt.format("%H:%M").to_string(),
        None => String::new(),
    }
}

/// Whether two messages are close enough in time to share one header.
pub fn within_grouping_window(earlier: &Ts, later: &Ts) -> bool {
    let gap = later.epoch_seconds() - earlier.epoch_seconds();
    (0..=300).contains(&gap)
}

/// Whether two messages fall on different local days.
pub fn crosses_day_boundary(earlier: &Ts, later: &Ts) -> bool {
    match (local(earlier), local(later)) {
        (Some(a), Some(b)) => a.date_naive() != b.date_naive(),
        _ => false,
    }
}

fn local(ts: &Ts) -> Option<DateTime<Local>> {
    let seconds = ts.epoch_seconds();
    if seconds <= 0 {
        return None;
    }
    Local.timestamp_opt(seconds, 0).single()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts_at(dt: DateTime<Local>) -> Ts {
        Ts(format!("{}.000000", dt.timestamp()))
    }

    #[test]
    fn today_reads_as_today() {
        assert_eq!(day_heading(&ts_at(Local::now())), "Today");
    }

    #[test]
    fn yesterday_reads_as_yesterday() {
        let ts = ts_at(Local::now() - Duration::days(1));
        assert_eq!(day_heading(&ts), "Yesterday");
    }

    #[test]
    fn an_unset_timestamp_renders_as_nothing_rather_than_1970() {
        assert_eq!(clock(&Ts::default()), "");
        assert_eq!(day_heading(&Ts("0.000000".into())), "");
        assert_eq!(full(&Ts::default()), "");
    }

    #[test]
    fn the_full_form_names_the_day_the_date_and_the_second() {
        let ts = ts_at(Local::now());
        let full = full(&ts);
        assert!(full.contains(&Local::now().format("%Y").to_string()));
        assert!(full.contains(" at "));
        // Two colons: hours:minutes:seconds.
        assert_eq!(full.matches(':').count(), 2);
    }

    #[test]
    fn grouping_covers_five_minutes_and_no_more() {
        let base = Ts("1700000000.000100".into());
        assert!(within_grouping_window(
            &base,
            &Ts("1700000299.000100".into())
        ));
        assert!(!within_grouping_window(
            &base,
            &Ts("1700000301.000100".into())
        ));
    }

    #[test]
    fn grouping_never_looks_backwards() {
        let base = Ts("1700000600.000100".into());
        assert!(!within_grouping_window(
            &base,
            &Ts("1700000000.000100".into())
        ));
    }
}
