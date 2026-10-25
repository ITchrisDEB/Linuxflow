//! Localised formats (numbers, sizes, percentages, relative time).

/// Separators of a language.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NumberStyle {
    pub group: &'static str,
    pub decimal: &'static str,
    /// Space placed before `%` (empty in English, U+202F in French).
    pub percent_space: &'static str,
}

/// Numeric style for a language (`fr`, `fr_CA`, `de`...). English by default.
pub fn style_for(lang: &str) -> NumberStyle {
    const EN: NumberStyle = NumberStyle {
        group: ",",
        decimal: ".",
        percent_space: "",
    };
    match lang.split('_').next().unwrap_or("") {
        // U+202F: narrow no-break space (French standard).
        "fr" => NumberStyle {
            group: "\u{202f}",
            decimal: ",",
            percent_space: "\u{202f}",
        },
        "de" | "es" | "it" | "pt" | "nl" | "tr" | "id" => NumberStyle {
            group: ".",
            decimal: ",",
            percent_space: "\u{a0}",
        },
        "ru" | "pl" | "cs" | "sv" | "nb" | "fi" | "uk" => NumberStyle {
            group: "\u{a0}",
            decimal: ",",
            percent_space: "\u{a0}",
        },
        _ => EN,
    }
}

/// Formats `value` with `decimals` decimals according to `style`. Non-finite => "–".
pub fn number(style: NumberStyle, value: f64, decimals: usize) -> String {
    if !value.is_finite() {
        return "\u{2013}".to_string();
    }
    let decimals = decimals.min(12);
    let raw = format!("{:.*}", decimals, value.abs());
    let (int, frac) = raw.split_once('.').unwrap_or((&raw, ""));
    let mut grouped = String::with_capacity(int.len() + int.len() / 3 * style.group.len());
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            grouped.push_str(style.group);
        }
        grouped.push(c);
    }
    let is_zero = raw.chars().all(|c| c == '0' || c == '.');
    let mut out = String::new();
    if value.is_sign_negative() && !is_zero {
        out.push('-');
    }
    out.push_str(&grouped);
    if !frac.is_empty() {
        out.push_str(style.decimal);
        out.push_str(frac);
    }
    out
}

/// Percentage from a ratio (0.25 => "25 %").
pub fn percent(style: NumberStyle, ratio: f64, decimals: usize) -> String {
    format!(
        "{}{}%",
        number(style, ratio * 100.0, decimals),
        style.percent_space
    )
}

/// Unit chosen for a size in bytes (decimal base) and decimals.
pub fn byte_unit(bytes: u64) -> (usize, f64, usize) {
    const STEPS: [f64; 5] = [1.0, 1e3, 1e6, 1e9, 1e12];
    let idx = STEPS.iter().rposition(|s| bytes as f64 >= *s).unwrap_or(0);
    // Bytes and KB without a decimal; MB, GB, TB with one decimal.
    (idx, bytes as f64 / STEPS[idx], usize::from(idx >= 2))
}

/// Splitting of a timestamp into local fields.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LocalTime {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub ordinal_day: i64,
}

/// Converts a Unix timestamp to local time for a UTC offset in seconds.
pub fn local_time(unix_secs: i64, offset_secs: i32) -> Option<LocalTime> {
    let shifted = unix_secs.checked_add(i64::from(offset_secs))?;
    let dt = time::OffsetDateTime::from_unix_timestamp(shifted).ok()?;
    Some(LocalTime {
        year: dt.year(),
        month: u8::from(dt.month()),
        day: dt.day(),
        hour: dt.hour(),
        minute: dt.minute(),
        ordinal_day: shifted.div_euclid(86_400),
    })
}

/// Local UTC offset in seconds at the given instant (0 if unavailable).
pub fn local_offset_secs(unix_secs: i64) -> i32 {
    time::OffsetDateTime::from_unix_timestamp(unix_secs)
        .ok()
        .and_then(|t| time::UtcOffset::local_offset_at(t).ok())
        .map(|o| o.whole_seconds())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        let en = style_for("en_US");
        let fr = style_for("fr_FR");
        assert_eq!(number(en, 1234567.891, 2), "1,234,567.89");
        assert_eq!(number(fr, 1234567.891, 2), "1\u{202f}234\u{202f}567,89");
        assert_eq!(number(fr, 999.0, 0), "999");
        assert_eq!(number(fr, 1000.0, 0), "1\u{202f}000");
        assert_eq!(number(en, -1500.5, 1), "-1,500.5");
        assert_eq!(number(en, -0.0001, 2), "0.00");
        assert_eq!(number(en, f64::NAN, 1), "\u{2013}");
        assert!(number(en, 1e300, 0).len() > 300);
    }

    #[test]
    fn percents() {
        assert_eq!(percent(style_for("en"), 0.425, 1), "42.5%");
        assert_eq!(percent(style_for("fr"), 0.425, 1), "42,5\u{202f}%");
        assert_eq!(percent(style_for("fr"), 1.0, 0), "100\u{202f}%");
    }

    #[test]
    fn units() {
        assert_eq!(byte_unit(0), (0, 0.0, 0));
        assert_eq!(byte_unit(999).0, 0);
        assert_eq!(byte_unit(1_000).0, 1);
        assert_eq!(byte_unit(1_500_000).0, 2);
        assert_eq!(byte_unit(3_100_000_000), (3, 3.1, 1));
        assert_eq!(byte_unit(u64::MAX).0, 4);
    }

    #[test]
    fn local_fields() {
        // 2026-10-24 14:05:00 UTC
        let t = 1_792_850_700;
        let l = local_time(t, 0);
        assert_eq!(
            l.map(|l| (l.year, l.month, l.day, l.hour, l.minute)),
            Some((2026, 10, 24, 14, 5))
        );
        let l = local_time(t, 3600 * 10);
        assert_eq!(l.map(|l| (l.day, l.hour)), Some((25, 0)));
        assert!(local_time(i64::MAX, 5).is_none());
    }
}
