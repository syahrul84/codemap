/// Current UTC time as RFC 3339 with whole seconds, e.g. `2026-10-15T10:00:00Z`.
pub fn now_rfc3339() -> String {
    let now = jiff::Timestamp::now();
    jiff::Timestamp::from_second(now.as_second())
        .unwrap_or(now)
        .to_string()
}
