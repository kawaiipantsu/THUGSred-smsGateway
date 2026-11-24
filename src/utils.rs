//! Small shared helpers: timestamps, identifiers, number handling, redaction.

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use sha2::{Digest, Sha256};

/// Current UTC time in the canonical storage format.
pub fn now() -> String {
    ts(Utc::now())
}

/// Canonical storage format: RFC 3339, milliseconds, `Z` suffix (sortable).
pub fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn ts_after(seconds: i64) -> String {
    ts(Utc::now() + Duration::seconds(seconds))
}

pub fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// Normalise a telephone number for comparison: strips separators and turns
/// a leading `00` into `+`. Alphanumeric senders are returned unchanged.
pub fn normalize_number(raw: &str) -> String {
    let t = raw.trim();
    let dialable = t
        .chars()
        .all(|c| c.is_ascii_digit() || " -()./+".contains(c));
    if !dialable || t.is_empty() {
        return t.to_string();
    }
    let mut digits: String = t.chars().filter(|c| c.is_ascii_digit()).collect();
    if t.starts_with('+') {
        digits.insert(0, '+');
    } else if let Some(rest) = digits.strip_prefix("00") {
        digits = format!("+{rest}");
    }
    digits
}

/// Trailing digits used to correlate numbers reported in different formats
/// (national versus international).
pub fn number_suffix(n: &str, len: usize) -> String {
    let d: Vec<char> = n.chars().filter(|c| c.is_ascii_digit()).collect();
    d[d.len().saturating_sub(len)..].iter().collect()
}

/// Whether two numbers plausibly denote the same subscriber.
pub fn numbers_match(a: &str, b: &str) -> bool {
    let (a, b) = (normalize_number(a), normalize_number(b));
    if a == b {
        return true;
    }
    // Two international numbers must match exactly. The suffix comparison
    // only bridges national and international notation of one number.
    if a.starts_with('+') && b.starts_with('+') {
        return false;
    }
    let (sa, sb) = (number_suffix(&a, 8), number_suffix(&b, 8));
    sa.len() >= 6 && sa == sb
}

/// Mask a telephone number for log output, keeping the last two digits.
pub fn mask_number(n: &str) -> String {
    let count = n.chars().count();
    if count <= 4 {
        return "*".repeat(count);
    }
    let head: String = n
        .chars()
        .take(if n.starts_with('+') { 3 } else { 2 })
        .collect();
    let tail: String = n.chars().skip(count - 2).collect();
    format!(
        "{head}{}{tail}",
        "*".repeat(count - head.chars().count() - 2)
    )
}

/// Match a value against a list entry: exact (case-insensitive) or a prefix
/// ending in `*`. Telephone numbers are compared in normalised form.
pub fn list_matches(pattern: &str, value: &str) -> bool {
    let (p, v) = (normalize_number(pattern), normalize_number(value));
    match p.strip_suffix('*') {
        Some(prefix) => v.to_lowercase().starts_with(&prefix.to_lowercase()),
        None => p.eq_ignore_ascii_case(&v),
    }
}

/// Reduce an arbitrary string to a safe file name component.
pub fn sanitize_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let mut out: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-_.".contains(c) {
                c
            } else {
                '_'
            }
        })
        .take(96)
        .collect();
    while out.starts_with('.') {
        out.remove(0);
    }
    if out.is_empty() {
        out.push_str("file");
    }
    out
}

/// Constant-time comparison of two secrets.
pub fn secure_eq(a: &str, b: &str) -> bool {
    let (ha, hb) = (Sha256::digest(a.as_bytes()), Sha256::digest(b.as_bytes()));
    ha.iter()
        .zip(hb.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// Exponential backoff: `initial * 2^(attempt-1)` capped at `max` seconds.
pub fn backoff_seconds(initial: u64, max: u64, attempt: u32) -> u64 {
    let shift = attempt.saturating_sub(1).min(20);
    initial.saturating_mul(1u64 << shift).min(max.max(initial))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(normalize_number("+45 12 34-56 78"), "+4512345678");
        assert_eq!(normalize_number("004512345678"), "+4512345678");
        assert_eq!(normalize_number("ExampleSvc"), "ExampleSvc");
        assert!(numbers_match("+4512345678", "12345678"));
        assert!(!numbers_match("+4512345678", "+4512345679"));
        assert!(!numbers_match("123", "9123"));
        assert!(
            !numbers_match("+4612345678", "+4512345678"),
            "same suffix, different country"
        );
        assert_eq!(mask_number("+4512345678"), "+45******78");
    }

    #[test]
    fn list_matching() {
        assert!(list_matches("+45*", "+4512345678"));
        assert!(list_matches("exampleservice", "ExampleService"));
        assert!(!list_matches("+46*", "+4512345678"));
    }

    #[test]
    fn filenames() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("..\\..\\evil name.exe"), "evil_name.exe");
        assert_eq!(sanitize_filename("...."), "file");
        assert_eq!(sanitize_filename(""), "file");
    }

    #[test]
    fn backoff() {
        assert_eq!(backoff_seconds(5, 3600, 1), 5);
        assert_eq!(backoff_seconds(5, 3600, 4), 40);
        assert_eq!(backoff_seconds(5, 3600, 60), 3600);
    }

    #[test]
    fn secrets_compare() {
        assert!(secure_eq("abc", "abc"));
        assert!(!secure_eq("abc", "abd"));
    }
}
