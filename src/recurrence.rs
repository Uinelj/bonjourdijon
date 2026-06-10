use chrono::{DateTime, Duration, Utc};
use cron::Schedule;
use std::str::FromStr;

/// Normalize a user-supplied cron expression to the 7-field format expected
/// by the `cron` crate: `sec min hour dom month dow year`.
///
/// The `cron` crate uses 1=SUN .. 7=SAT (or names like SUN, MON).
/// Standard POSIX cron uses 0=SUN .. 6=SAT. We auto-convert numeric
/// day-of-week `0` → `SUN` so users can write standard cron expressions.
///
/// Accepts:
///   - 5-field (standard POSIX): `min hour dom month dow` → prepends `0` seconds, appends `*` year
///   - 6-field: `sec min hour dom month dow` → appends `*` year
///   - 7-field: passed through as-is
pub fn normalize_cron(expr: &str) -> String {
    let mut fields: Vec<String> = expr.split_whitespace().map(|s| s.to_string()).collect();

    // For 5-field input, the dow is fields[4]; for 6-field, it's fields[5]
    let dow_idx = match fields.len() {
        5 => Some(4),
        6 => Some(5),
        7 => Some(5),
        _ => None,
    };

    // Convert POSIX numeric 0 (Sunday) → SUN, because the cron crate
    // treats 0 as "no match" / an error in the dow position.
    if let Some(idx) = dow_idx {
        fields[idx] = normalize_dow_field(&fields[idx]);
    }

    let joined: String = fields.join(" ");
    match fields.len() {
        5 => format!("0 {joined} *"),
        6 => format!("{joined} *"),
        7 => joined,
        _ => joined,
    }
}

/// Normalize a single day-of-week field from POSIX numbering (0=Sun, 6=Sat)
/// to names the `cron` crate understands (SUN, MON, ..., SAT).
/// Handles ranges (e.g. "1-5"), lists (e.g. "0,3,6"), and wildcards.
fn normalize_dow_field(field: &str) -> String {
    // Already uses names or wildcards — pass through
    if field == "*"
        || field.contains(|c: char| c.is_ascii_alphabetic())
    {
        return field.to_string();
    }

    // Map POSIX digit → name
    fn digit_to_name(d: &str) -> String {
        match d.trim() {
            "0" | "7" => "SUN".to_string(),
            "1" => "MON".to_string(),
            "2" => "TUE".to_string(),
            "3" => "WED".to_string(),
            "4" => "THU".to_string(),
            "5" => "FRI".to_string(),
            "6" => "SAT".to_string(),
            other => other.to_string(), // pass through (e.g. */2)
        }
    }

    // Handle comma-separated lists: "0,3,6"
    if field.contains(',') {
        return field
            .split(',')
            .map(|part| {
                if part.contains('-') {
                    // range inside a list: "1-5"
                    normalize_dow_range(part)
                } else {
                    digit_to_name(part)
                }
            })
            .collect::<Vec<_>>()
            .join(",");
    }

    // Handle ranges: "1-5"
    if field.contains('-') {
        return normalize_dow_range(field);
    }

    // Handle step syntax: "*/2" or "0/2"
    if field.contains('/') {
        return field.to_string(); // pass through — the cron crate handles these
    }

    // Single digit
    digit_to_name(field)
}

fn normalize_dow_range(range: &str) -> String {
    let names = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];
    let parts: Vec<&str> = range.split('-').collect();
    if parts.len() == 2 {
        let start: usize = parts[0].trim().parse().unwrap_or(99);
        let end: usize = parts[1].trim().parse().unwrap_or(99);
        if start <= 6 && end <= 6 {
            return format!("{}-{}", names[start], names[end]);
        }
    }
    range.to_string()
}

/// Expand a recurring item into concrete [`DateTime<Utc>`] values within `[from, to)`.
///
/// Uses `cron_expr` when present, otherwise falls back to `interval_secs`
/// arithmetic anchored at `anchor`. Non-recurring items return the anchor
/// itself if it falls within the window, or an empty vec otherwise.
pub fn expand_occurrences(
    anchor: DateTime<Utc>,
    cron_expr: Option<&str>,
    interval_secs: Option<i64>,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Vec<DateTime<Utc>> {
    // ── Cron-based ──────────────────────────────────────────────────
    if let Some(expr) = cron_expr {
        if let Some(dates) = expand_cron(expr, from, to) {
            return dates;
        }
        // If cron parse failed, fall through to interval
    }

    // ── Interval-based ──────────────────────────────────────────────
    if let Some(secs) = interval_secs {
        if secs > 0 {
            return expand_interval(anchor, secs, from, to);
        }
    }

    // ── Non-recurring ───────────────────────────────────────────────
    if anchor >= from && anchor < to {
        vec![anchor]
    } else {
        vec![]
    }
}

/// Find the next occurrence strictly after `after`.
/// Useful for rescheduling a completed recurring chore/event.
pub fn next_occurrence_after(
    anchor: DateTime<Utc>,
    cron_expr: Option<&str>,
    interval_secs: Option<i64>,
    after: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    // Try cron first
    if let Some(expr) = cron_expr {
        let normalized = normalize_cron(expr);
        if let Ok(schedule) = Schedule::from_str(&normalized) {
            return schedule.after(&after).next().map(|dt| dt.with_timezone(&Utc));
        }
    }

    // Fall back to interval
    if let Some(secs) = interval_secs {
        if secs > 0 {
            let dur = Duration::seconds(secs);
            // Jump to first occurrence >= after
            let diff = (after - anchor).num_seconds();
            let n = if diff <= 0 {
                1
            } else {
                (diff / secs) + 1
            };
            let next = anchor + Duration::seconds(n * secs);
            // Make sure we're strictly after
            if next <= after {
                return Some(next + dur);
            }
            return Some(next);
        }
    }

    None
}

fn expand_cron(
    expr: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Option<Vec<DateTime<Utc>>> {
    let normalized = normalize_cron(expr);
    let schedule = Schedule::from_str(&normalized).ok()?;

    // Start iterating from just before `from`
    let start = from - Duration::seconds(1);
    let mut results = Vec::new();
    for dt in schedule.after(&start) {
        let dt_utc = dt.with_timezone(&Utc);
        if dt_utc >= to {
            break;
        }
        if dt_utc >= from {
            results.push(dt_utc);
        }
    }
    Some(results)
}

fn expand_interval(
    anchor: DateTime<Utc>,
    interval_secs: i64,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Vec<DateTime<Utc>> {
    let mut results = Vec::new();

    // Jump directly to the first occurrence >= from
    let diff = (from - anchor).num_seconds();
    let start_n = if diff <= 0 {
        0i64
    } else {
        diff / interval_secs
    };

    // Safety: cap iterations to avoid unbounded loops
    let max_iterations = 1000;
    for i in 0..max_iterations {
        let n = start_n + i;
        let occ = anchor + Duration::seconds(n * interval_secs);
        if occ >= to {
            break;
        }
        if occ >= from {
            results.push(occ);
        }
    }
    results
}

/// Convert a day-of-week POSIX digit (0=Sun … 6=Sat) to its English name.
fn dow_digit_to_name(d: &str) -> &str {
    match d.trim() {
        "0" | "7" => "Sunday",
        "1" => "Monday",
        "2" => "Tuesday",
        "3" => "Wednesday",
        "4" => "Thursday",
        "5" => "Friday",
        "6" => "Saturday",
        _ => d,
    }
}

/// Convert a cron expression or interval_secs into a short human-readable label
/// suitable for display in the web UI.
///
/// Handles standard 5-field cron (`min hour dom month dow`) and common interval_secs
/// values. Returns "One-time" when neither is set.
pub fn cron_to_human(cron_expr: Option<&str>, interval_secs: Option<i64>) -> String {
    if let Some(expr) = cron_expr {
        let f: Vec<&str> = expr.split_whitespace().collect();
        if f.len() == 5 {
            let (min, hour, dom, _mon, dow) = (f[0], f[1], f[2], f[3], f[4]);
            let time = format!("{hour}:{min:0>2}");
            return match (dom, dow) {
                ("*", "*") => format!("Every day at {time}"),
                ("*", "1-5") | ("*", "MON-FRI") => format!("Weekdays at {time}"),
                ("*", d) => {
                    // Could be a single digit "0" or a name "SUN"
                    let name = match d.to_uppercase().as_str() {
                        "SUN" => "Sunday",
                        "MON" => "Monday",
                        "TUE" => "Tuesday",
                        "WED" => "Wednesday",
                        "THU" => "Thursday",
                        "FRI" => "Friday",
                        "SAT" => "Saturday",
                        _ => dow_digit_to_name(d),
                    };
                    format!("Every {name} at {time}")
                }
                (d, "*") => format!("Day {d} of every month at {time}"),
                _ => format!("Cron: {expr}"),
            };
        }
    }
    if let Some(secs) = interval_secs {
        if secs > 0 {
            return match secs {
                86400 => "Every day".into(),
                604800 => "Every week".into(),
                1209600 => "Every 2 weeks".into(),
                2592000 => "Every ~month".into(),
                s if s % 86400 == 0 => format!("Every {} days", s / 86400),
                s if s % 3600 == 0 => format!("Every {}h", s / 3600),
                s => format!("Every {}s", s),
            };
        }
    }
    "One-time".into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, NaiveTime, TimeZone};

    fn dt(year: i32, month: u32, day: u32, hour: u32, min: u32) -> DateTime<Utc> {
        Utc.from_utc_datetime(
            &NaiveDate::from_ymd_opt(year, month, day)
                .unwrap()
                .and_time(NaiveTime::from_hms_opt(hour, min, 0).unwrap()),
        )
    }

    #[test]
    fn test_normalize_cron_5_field() {
        // POSIX 0 = Sunday → normalized to SUN
        assert_eq!(normalize_cron("0 9 * * 0"), "0 0 9 * * SUN *");
    }

    #[test]
    fn test_normalize_cron_7_field_passthrough() {
        // 7-field with names passes through
        assert_eq!(
            normalize_cron("0 0 9 * * SUN *"),
            "0 0 9 * * SUN *"
        );
    }

    #[test]
    fn test_normalize_cron_dow_range() {
        // "0 9 * * 1-5" → Mon through Fri
        assert_eq!(normalize_cron("0 9 * * 1-5"), "0 0 9 * * MON-FRI *");
    }

    #[test]
    fn test_expand_cron_weekly_sunday() {
        // "0 9 * * 0" = every Sunday at 09:00 (POSIX format, 0=Sun)
        let from = dt(2026, 6, 1, 0, 0);
        let to = dt(2026, 7, 1, 0, 0);
        let results = expand_occurrences(
            dt(2026, 6, 7, 9, 0), // anchor (first Sunday)
            Some("0 9 * * 0"),
            None,
            from,
            to,
        );
        // June 2026 Sundays: 7, 14, 21, 28
        assert_eq!(results.len(), 4);
        assert_eq!(results[0], dt(2026, 6, 7, 9, 0));
        assert_eq!(results[1], dt(2026, 6, 14, 9, 0));
        assert_eq!(results[2], dt(2026, 6, 21, 9, 0));
        assert_eq!(results[3], dt(2026, 6, 28, 9, 0));
    }

    #[test]
    fn test_expand_interval_3_days() {
        let anchor = dt(2026, 6, 1, 9, 0);
        let from = dt(2026, 6, 1, 0, 0);
        let to = dt(2026, 6, 15, 0, 0);
        let interval = 3 * 86400; // 3 days
        let results = expand_occurrences(anchor, None, Some(interval), from, to);
        // June 1, 4, 7, 10, 13 at 09:00
        assert_eq!(results.len(), 5);
        assert_eq!(results[0], dt(2026, 6, 1, 9, 0));
        assert_eq!(results[1], dt(2026, 6, 4, 9, 0));
        assert_eq!(results[4], dt(2026, 6, 13, 9, 0));
    }

    #[test]
    fn test_non_recurring_in_range() {
        let anchor = dt(2026, 6, 10, 9, 0);
        let from = dt(2026, 6, 1, 0, 0);
        let to = dt(2026, 7, 1, 0, 0);
        let results = expand_occurrences(anchor, None, None, from, to);
        assert_eq!(results, vec![anchor]);
    }

    #[test]
    fn test_non_recurring_out_of_range() {
        let anchor = dt(2026, 5, 10, 9, 0);
        let from = dt(2026, 6, 1, 0, 0);
        let to = dt(2026, 7, 1, 0, 0);
        let results = expand_occurrences(anchor, None, None, from, to);
        assert!(results.is_empty());
    }

    #[test]
    fn test_next_occurrence_cron() {
        let after = dt(2026, 6, 10, 12, 0); // Wednesday
        let next = next_occurrence_after(
            dt(2026, 6, 7, 9, 0),
            Some("0 9 * * 0"), // Sundays at 9 (POSIX: 0=Sun)
            None,
            after,
        );
        assert_eq!(next, Some(dt(2026, 6, 14, 9, 0)));
    }

    #[test]
    fn test_next_occurrence_interval() {
        let anchor = dt(2026, 6, 1, 9, 0);
        let after = dt(2026, 6, 5, 12, 0);
        let next = next_occurrence_after(anchor, None, Some(3 * 86400), after);
        // anchor + 2*3days = June 7 09:00
        assert_eq!(next, Some(dt(2026, 6, 7, 9, 0)));
    }

    #[test]
    fn test_cron_preferred_over_interval() {
        // When both cron and interval are provided, cron wins
        let from = dt(2026, 6, 1, 0, 0);
        let to = dt(2026, 6, 15, 0, 0);
        let results = expand_occurrences(
            dt(2026, 6, 1, 9, 0),
            Some("0 9 * * 0"), // Sundays (POSIX: 0=Sun)
            Some(86400),       // Daily (should be ignored)
            from,
            to,
        );
        // Should follow cron (Sundays), not daily interval
        assert_eq!(results.len(), 2); // June 7, 14
        assert_eq!(results[0], dt(2026, 6, 7, 9, 0));
    }

    // ── cron_to_human tests ──────────────────────────────────────────

    #[test]
    fn test_cron_to_human_daily() {
        assert_eq!(cron_to_human(Some("0 9 * * *"), None), "Every day at 9:00");
    }

    #[test]
    fn test_cron_to_human_weekly_dow_digit() {
        assert_eq!(
            cron_to_human(Some("0 9 * * 0"), None),
            "Every Sunday at 9:00"
        );
        assert_eq!(
            cron_to_human(Some("0 9 * * 1"), None),
            "Every Monday at 9:00"
        );
    }

    #[test]
    fn test_cron_to_human_weekdays() {
        assert_eq!(
            cron_to_human(Some("0 9 * * 1-5"), None),
            "Weekdays at 9:00"
        );
    }

    #[test]
    fn test_cron_to_human_monthly() {
        assert_eq!(
            cron_to_human(Some("0 9 15 * *"), None),
            "Day 15 of every month at 9:00"
        );
    }

    #[test]
    fn test_cron_to_human_interval_fallback() {
        assert_eq!(cron_to_human(None, Some(604800)), "Every week");
        assert_eq!(cron_to_human(None, Some(1209600)), "Every 2 weeks");
        assert_eq!(cron_to_human(None, Some(86400)), "Every day");
        assert_eq!(cron_to_human(None, Some(259200)), "Every 3 days");
    }

    #[test]
    fn test_cron_to_human_one_time() {
        assert_eq!(cron_to_human(None, None), "One-time");
    }
}
