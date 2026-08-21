//! Counts and durations, in the English a developer reads.
//!
//! Worth their own functions because `commit(s)` and `0 minutes` are exactly
//! the details that make a tool read as unfinished, and each had spread across
//! several messages before it was one.

/// `1 commit`, `2 commits`.
///
/// Regular English `-s` only, which covers every noun riabuild counts. Worth a
/// function because `commit(s)` is exactly the sort of detail that makes a
/// tool read as unfinished, and it had spread to four separate messages.
pub fn plural(count: u64, unit: &str) -> String {
    if count == 1 {
        format!("{count} {unit}")
    } else {
        format!("{count} {unit}s")
    }
}

/// A count of minutes as something a person can judge at a glance.
///
/// A brokered credential lasts around a month, and "43199 more minute(s)" is a
/// number nobody can convert in their head — it reads as an error rather than
/// as "this is fine for weeks". Zero components are dropped so short durations
/// stay short.
pub fn duration_words(minutes: u64) -> String {
    if minutes == 0 {
        return "less than a minute".to_string();
    }
    let parts = [
        (minutes / (60 * 24), "day"),
        ((minutes / 60) % 24, "hour"),
        (minutes % 60, "minute"),
    ];
    parts
        .iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, unit)| plural(*count, unit))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_months_worth_of_minutes_reads_as_days() {
        // The number that prompted this: a 30-day Infisical credential, one
        // minute in, rendered as "43199 more minute(s)".
        assert_eq!(duration_words(43_199), "29 days 23 hours 59 minutes");
        assert_eq!(duration_words(43_200), "30 days");
    }

    #[test]
    fn empty_components_are_left_out() {
        assert_eq!(duration_words(1), "1 minute");
        assert_eq!(duration_words(59), "59 minutes");
        assert_eq!(duration_words(60), "1 hour");
        assert_eq!(duration_words(90), "1 hour 30 minutes");
        assert_eq!(duration_words(1440), "1 day");
        assert_eq!(duration_words(1500), "1 day 1 hour");
    }

    #[test]
    fn an_expired_credential_does_not_read_as_zero_minutes() {
        assert_eq!(duration_words(0), "less than a minute");
    }
}
