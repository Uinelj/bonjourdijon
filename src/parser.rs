use chrono::{DateTime, Datelike, Duration, Local, NaiveTime, TimeZone, Utc, Weekday};

/// Result of parsing a reminder string.
#[derive(Debug, Clone)]
pub struct ParsedReminder {
    pub message: String,
    pub remind_at: DateTime<Utc>,
    /// For periodic reminders: repeat interval in seconds. `None` = one-shot.
    pub interval_secs: Option<i64>,
}

/// Parse natural-language reminder text.
///
/// Supported patterns:
/// - "in 2 hours do laundry"
/// - "in 30 minutes take out trash"
/// - "in 3 days clean fridge"
/// - "do laundry before friday"
/// - "vacuum before tomorrow"
/// - "buy groceries before 2026-06-15"
///
/// Returns `None` if the input can't be parsed.
pub fn parse_reminder(input: &str) -> Option<ParsedReminder> {
    let input = input.trim();
    let lower = input.to_lowercase();

    // Pattern 0: "every <N> <unit> [to] <message>" — periodic reminder
    if lower.starts_with("every ") {
        return parse_every_duration(input);
    }

    // Pattern 1: "in <N> <unit> [to] <message>"
    if lower.starts_with("in ") {
        return parse_in_duration(input);
    }

    // Pattern 2: "<message> before <deadline>"
    if let Some(pos) = lower.find(" before ") {
        let message = input[..pos].trim().to_string();
        let deadline_str = input[pos + 8..].trim();
        if let Some(dt) = parse_deadline(deadline_str) {
            return Some(ParsedReminder {
                message,
                remind_at: dt,
                interval_secs: None,
            });
        }
    }

    // Pattern 3: just a duration-like thing at the start "2h do laundry"
    if let Some(parsed) = parse_shorthand_duration(input) {
        return Some(parsed);
    }

    None
}

fn parse_in_duration(input: &str) -> Option<ParsedReminder> {
    // "in <N> <unit> [to] <message>"
    let rest = &input[3..]; // skip "in "
    let parts: Vec<&str> = rest.splitn(3, ' ').collect();
    if parts.len() < 2 {
        return None;
    }

    let n: i64 = parts[0].parse().ok()?;
    let unit = parts[1].to_lowercase();
    let duration = match_unit(&unit, n)?;

    let message_part = if parts.len() >= 3 { parts[2] } else { "" };
    // Strip leading "to " from the message
    let message = message_part
        .strip_prefix("to ")
        .or_else(|| message_part.strip_prefix("to\u{00a0}"))
        .unwrap_or(message_part)
        .trim()
        .to_string();

    let message = if message.is_empty() {
        "Reminder".to_string()
    } else {
        message
    };

    let remind_at = Utc::now() + duration;
    Some(ParsedReminder {
        message,
        remind_at,
        interval_secs: None,
    })
}

fn parse_every_duration(input: &str) -> Option<ParsedReminder> {
    // "every <N> <unit> [to] <message>"
    let rest = &input[6..]; // skip "every "
    let parts: Vec<&str> = rest.splitn(3, ' ').collect();
    if parts.len() < 2 {
        return None;
    }

    let n: i64 = parts[0].parse().ok()?;
    let unit = parts[1].to_lowercase();
    let duration = match_unit(&unit, n)?;
    let interval_secs = duration.num_seconds();

    let message_part = if parts.len() >= 3 { parts[2] } else { "" };
    // Strip leading "to " from the message
    let message = message_part
        .strip_prefix("to ")
        .or_else(|| message_part.strip_prefix("to\u{00a0}"))
        .unwrap_or(message_part)
        .trim()
        .to_string();

    let message = if message.is_empty() {
        "Reminder".to_string()
    } else {
        message
    };

    // First fire is one interval from now
    let remind_at = Utc::now() + duration;
    Some(ParsedReminder {
        message,
        remind_at,
        interval_secs: Some(interval_secs),
    })
}

fn parse_shorthand_duration(input: &str) -> Option<ParsedReminder> {
    // "2h do laundry" or "30m take out trash"
    let first_space = input.find(' ').unwrap_or(input.len());
    let token = &input[..first_space];

    // Try to parse e.g. "2h", "30m", "3d"
    if token.len() < 2 {
        return None;
    }
    let (num_str, unit_char) = token.split_at(token.len() - 1);
    let n: i64 = num_str.parse().ok()?;
    let duration = match unit_char {
        "s" => Duration::seconds(n),
        "m" => Duration::minutes(n),
        "h" => Duration::hours(n),
        "d" => Duration::days(n),
        "w" => Duration::weeks(n),
        _ => return None,
    };

    let message = input[first_space..].trim().to_string();
    let message = if message.is_empty() {
        "Reminder".to_string()
    } else {
        message
    };

    let remind_at = Utc::now() + duration;
    Some(ParsedReminder {
        message,
        remind_at,
        interval_secs: None,
    })
}

fn match_unit(unit: &str, n: i64) -> Option<Duration> {
    match unit.trim_end_matches('s') {
        "second" | "sec" => Some(Duration::seconds(n)),
        "minute" | "min" => Some(Duration::minutes(n)),
        "hour" | "hr" => Some(Duration::hours(n)),
        "day" => Some(Duration::days(n)),
        "week" | "wk" => Some(Duration::weeks(n)),
        _ => None,
    }
}

pub fn parse_deadline(s: &str) -> Option<DateTime<Utc>> {
    let lower = s.to_lowercase();
    let local_now = Local::now();

    // "tomorrow"
    if lower == "tomorrow" {
        let tomorrow = local_now.date_naive() + Duration::days(1);
        let dt = tomorrow.and_time(NaiveTime::from_hms_opt(9, 0, 0)?);
        return Some(
            Local
                .from_local_datetime(&dt)
                .single()?
                .with_timezone(&Utc),
        );
    }

    // "today"
    if lower == "today" {
        let today = local_now.date_naive();
        let dt = today.and_time(NaiveTime::from_hms_opt(9, 0, 0)?);
        return Some(
            Local
                .from_local_datetime(&dt)
                .single()?
                .with_timezone(&Utc),
        );
    }

    // Day of week: "monday", "tuesday", etc.
    if let Some(target_weekday) = parse_weekday(&lower) {
        let current_weekday = local_now.weekday();
        let days_ahead = (target_weekday.num_days_from_monday() as i64
            - current_weekday.num_days_from_monday() as i64
            + 7)
            % 7;
        let days_ahead = if days_ahead == 0 { 7 } else { days_ahead };
        let target_date = local_now.date_naive() + Duration::days(days_ahead);
        let dt = target_date.and_time(NaiveTime::from_hms_opt(9, 0, 0)?);
        return Some(
            Local
                .from_local_datetime(&dt)
                .single()?
                .with_timezone(&Utc),
        );
    }

    // Try ISO date: "2026-06-15"
    if let Ok(nd) = chrono::NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d") {
        let dt = nd.and_time(NaiveTime::from_hms_opt(9, 0, 0)?);
        return Some(
            Local
                .from_local_datetime(&dt)
                .single()?
                .with_timezone(&Utc),
        );
    }

    // Try "june 15" or "jun 15" style
    if let Some(dt) = parse_month_day(&lower) {
        return Some(dt);
    }

    None
}

fn parse_weekday(s: &str) -> Option<Weekday> {
    match s {
        "monday" | "mon" => Some(Weekday::Mon),
        "tuesday" | "tue" | "tues" => Some(Weekday::Tue),
        "wednesday" | "wed" => Some(Weekday::Wed),
        "thursday" | "thu" | "thurs" => Some(Weekday::Thu),
        "friday" | "fri" => Some(Weekday::Fri),
        "saturday" | "sat" => Some(Weekday::Sat),
        "sunday" | "sun" => Some(Weekday::Sun),
        _ => None,
    }
}

fn parse_month_day(s: &str) -> Option<DateTime<Utc>> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() != 2 {
        return None;
    }
    let month = match parts[0] {
        "january" | "jan" => 1,
        "february" | "feb" => 2,
        "march" | "mar" => 3,
        "april" | "apr" => 4,
        "may" => 5,
        "june" | "jun" => 6,
        "july" | "jul" => 7,
        "august" | "aug" => 8,
        "september" | "sep" => 9,
        "october" | "oct" => 10,
        "november" | "nov" => 11,
        "december" | "dec" => 12,
        _ => return None,
    };
    let day: u32 = parts[1].parse().ok()?;
    let year = Local::now().year();
    let nd = chrono::NaiveDate::from_ymd_opt(year, month, day)?;
    let dt = nd.and_time(NaiveTime::from_hms_opt(9, 0, 0)?);
    Some(
        Local
            .from_local_datetime(&dt)
            .single()?
            .with_timezone(&Utc),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_in_duration_hours() {
        let r = parse_reminder("in 2 hours to do laundry").unwrap();
        assert_eq!(r.message, "do laundry");
        let diff = r.remind_at - Utc::now();
        // Should be roughly 2 hours (within 5 seconds tolerance)
        assert!((diff.num_seconds() - 7200).abs() < 5);
    }

    #[test]
    fn test_in_duration_minutes() {
        let r = parse_reminder("in 30 minutes take out trash").unwrap();
        assert_eq!(r.message, "take out trash");
        let diff = r.remind_at - Utc::now();
        assert!((diff.num_seconds() - 1800).abs() < 5);
    }

    #[test]
    fn test_in_duration_days() {
        let r = parse_reminder("in 3 days clean fridge").unwrap();
        assert_eq!(r.message, "clean fridge");
        let diff = r.remind_at - Utc::now();
        assert!((diff.num_seconds() - 259200).abs() < 5);
    }

    #[test]
    fn test_before_weekday() {
        let r = parse_reminder("vacuum before friday").unwrap();
        assert_eq!(r.message, "vacuum");
        assert!(r.remind_at > Utc::now());
    }

    #[test]
    fn test_before_tomorrow() {
        let r = parse_reminder("buy milk before tomorrow").unwrap();
        assert_eq!(r.message, "buy milk");
        assert!(r.remind_at > Utc::now());
    }

    #[test]
    fn test_before_iso_date() {
        let r = parse_reminder("finish report before 2026-12-25").unwrap();
        assert_eq!(r.message, "finish report");
    }

    #[test]
    fn test_before_month_day() {
        let r = parse_reminder("clean garage before june 15").unwrap();
        assert_eq!(r.message, "clean garage");
    }

    #[test]
    fn test_shorthand_duration() {
        let r = parse_reminder("2h do laundry").unwrap();
        assert_eq!(r.message, "do laundry");
        let diff = r.remind_at - Utc::now();
        assert!((diff.num_seconds() - 7200).abs() < 5);
    }

    #[test]
    fn test_unparseable() {
        assert!(parse_reminder("asdfqwer").is_none());
    }

    #[test]
    fn test_every_week() {
        let r = parse_reminder("every 1 week check if we need to order stuff").unwrap();
        assert_eq!(r.message, "check if we need to order stuff");
        assert_eq!(r.interval_secs, Some(604800));
        let diff = r.remind_at - Utc::now();
        assert!((diff.num_seconds() - 604800).abs() < 5);
    }

    #[test]
    fn test_every_2_days() {
        let r = parse_reminder("every 2 days to water the plants").unwrap();
        assert_eq!(r.message, "water the plants");
        assert_eq!(r.interval_secs, Some(172800));
    }

    #[test]
    fn test_every_hours() {
        let r = parse_reminder("every 4 hours stretch").unwrap();
        assert_eq!(r.message, "stretch");
        assert_eq!(r.interval_secs, Some(14400));
    }

    #[test]
    fn test_one_shot_has_no_interval() {
        let r = parse_reminder("in 2 hours to do laundry").unwrap();
        assert_eq!(r.interval_secs, None);
    }

    #[test]
    fn test_before_has_no_interval() {
        let r = parse_reminder("vacuum before friday").unwrap();
        assert_eq!(r.interval_secs, None);
    }
}
