//! Human-friendly byte-size formatting.
//!
//! `byte_suffix` turns a raw byte count into a ` (4.5 GB)`-style suffix that is
//! appended beside the full number. The full number is always still shown; this
//! module only produces the additive suffix. Decimal units (1 KB = 1000 bytes).

/// Decimal size units in ascending order. Largest supported unit is PB.
const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];

/// Byte counts at or below this value get no suffix.
const SUFFIX_THRESHOLD: u64 = 9999;

/// Returns the size suffix (leading space, parenthesized) for a byte count, or
/// `None` when `n <= 9999`.
///
/// Decimal base (÷1000), one decimal place, capped at PB. If rounding would
/// push the mantissa to `1000.0`, the next-larger unit is used instead.
pub fn byte_suffix(n: u64) -> Option<String> {
    if n <= SUFFIX_THRESHOLD {
        return None;
    }

    let mut idx = 0;
    let mut value = n as f64;
    for i in 0..UNITS.len() {
        idx = i;
        value = n as f64 / 1000_f64.powi(i as i32 + 1);
        if value < 1000.0 {
            break;
        }
    }

    if (value * 10.0).round() / 10.0 >= 1000.0 && idx + 1 < UNITS.len() {
        idx += 1;
        value /= 1000.0;
    }

    Some(format!(" ({value:.1} {})", UNITS[idx]))
}

/// Parse a displayed numeric string into a byte count for formatting. Strips
/// `,` / `_` separators and all whitespace. Returns `None` for empty,
/// non-integer, negative, or otherwise non-`u64` input.
pub fn parse_byte_count(s: &str) -> Option<u64> {
    let cleaned: String = s.chars().filter(|c| !c.is_whitespace() && *c != ',' && *c != '_').collect();
    if cleaned.is_empty() {
        return None;
    }
    cleaned.parse::<u64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_suffix_at_or_below_threshold() {
        assert_eq!(byte_suffix(0), None);
        assert_eq!(byte_suffix(9999), None);
    }

    #[test]
    fn smallest_suffix_just_above_threshold() {
        assert_eq!(byte_suffix(10_000).as_deref(), Some(" (10.0 KB)"));
    }

    #[test]
    fn one_case_per_unit() {
        assert_eq!(byte_suffix(512_000).as_deref(), Some(" (512.0 KB)"));
        assert_eq!(byte_suffix(1_000_000).as_deref(), Some(" (1.0 MB)"));
        assert_eq!(byte_suffix(4_500_000_000).as_deref(), Some(" (4.5 GB)"));
        assert_eq!(byte_suffix(2_000_000_000_000).as_deref(), Some(" (2.0 TB)"));
        assert_eq!(byte_suffix(3_000_000_000_000_000).as_deref(), Some(" (3.0 PB)"));
    }

    #[test]
    fn rounding_promotes_to_next_unit() {
        // 999_999 / 1000 = 999.999 -> rounds to 1000.0 KB -> promote to 1.0 MB.
        assert_eq!(byte_suffix(999_999).as_deref(), Some(" (1.0 MB)"));
    }

    #[test]
    fn beyond_pb_stays_pb() {
        assert_eq!(byte_suffix(5_000_000_000_000_000_000).as_deref(), Some(" (5000.0 PB)"));
    }

    #[test]
    fn parse_strips_separators_and_whitespace() {
        assert_eq!(parse_byte_count("4,500,000,000"), Some(4_500_000_000));
        assert_eq!(parse_byte_count("1_000_000"), Some(1_000_000));
        assert_eq!(parse_byte_count("  512000 "), Some(512_000));
    }

    #[test]
    fn parse_rejects_non_integers() {
        assert_eq!(parse_byte_count(""), None);
        assert_eq!(parse_byte_count("1.5"), None);
        assert_eq!(parse_byte_count("-5"), None);
        assert_eq!(parse_byte_count("abc"), None);
    }

    #[test]
    fn parse_rejects_whitespace_only() {
        assert_eq!(parse_byte_count("   "), None);
    }
}
