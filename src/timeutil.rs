//! RFC 3339 timestamps. All state and API times are UTC.

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

pub fn now() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}

pub fn format(t: OffsetDateTime) -> String {
    t.format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into())
}

pub fn now_string() -> String {
    format(now())
}

pub fn parse(s: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(s, &Rfc3339).ok()
}

pub fn add(t: OffsetDateTime, d: std::time::Duration) -> OffsetDateTime {
    t + time::Duration::try_from(d).unwrap_or(time::Duration::MAX)
}

/// `HH:MM UTC` for message texts.
pub fn hhmm(t: OffsetDateTime) -> String {
    format!("{:02}:{:02} UTC", t.hour(), t.minute())
}

/// `DD.MM HH:MM UTC` for far deadlines.
pub fn ddmm_hhmm(t: OffsetDateTime) -> String {
    format!(
        "{:02}.{:02} {:02}:{:02} UTC",
        t.day(),
        u8::from(t.month()),
        t.hour(),
        t.minute()
    )
}

/// `YYYY-MM-DD HH:MM UTC`.
pub fn full(t: OffsetDateTime) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02} UTC",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_formats() {
        let t = parse("2026-11-15T17:04:05.123Z").unwrap();
        assert_eq!(hhmm(t), "17:04 UTC");
        assert_eq!(ddmm_hhmm(t), "15.11 17:04 UTC");
        assert_eq!(full(t), "2026-11-15 17:04 UTC");
        assert_eq!(parse(&format(t)), Some(t));
        assert!(parse("yesterday").is_none());
    }
}
