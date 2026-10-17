/// Current UTC time as RFC 3339 with whole seconds, e.g. `2026-10-15T10:00:00Z`.
pub fn now_rfc3339() -> String {
    let now = jiff::Timestamp::now();
    jiff::Timestamp::from_second(now.as_second())
        .unwrap_or(now)
        .to_string()
}

/// The UTC time `days` days ago, in the same form as [`now_rfc3339`], so the
/// two compare as strings.
pub fn days_ago_rfc3339(days: i64) -> String {
    let now = jiff::Timestamp::now().as_second();
    jiff::Timestamp::from_second(now - days * 86_400)
        .unwrap_or(jiff::Timestamp::UNIX_EPOCH)
        .to_string()
}
