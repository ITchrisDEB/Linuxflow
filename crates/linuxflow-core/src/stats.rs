//! Hub statistics.

use crate::domain::{HistoryEntry, Stats};
use std::sync::OnceLock;
use time::{Date, Month, OffsetDateTime, UtcOffset};

static CACHED_OFFSET: OnceLock<i32> = OnceLock::new();

/// Memorises the local time offset. To call at start-up, before starting threads:
/// the `time` crate refuses to read the local time zone once the process is multi-threaded.
pub fn init_local_offset() {
    if let Ok(off) = UtcOffset::current_local_offset() {
        let _ = CACHED_OFFSET.set(off.whole_seconds());
    }
}

/// Local date (`YYYY-MM-DD`) of an ISO 8601 timestamp. `None` if the timestamp is unreadable.
/// `offset_secs`: offset to use; `None` = local time zone (fallback: memorised value, then UTC).
pub fn local_date_of(ts: &str, offset_secs: Option<i32>) -> Option<String> {
    let utc = parse_iso(ts)?;
    let off = match offset_secs {
        Some(secs) => UtcOffset::from_whole_seconds(secs).ok()?,
        None => UtcOffset::local_offset_at(utc)
            .ok()
            .or_else(|| {
                CACHED_OFFSET
                    .get()
                    .and_then(|s| UtcOffset::from_whole_seconds(*s).ok())
            })
            .unwrap_or(UtcOffset::UTC),
    };
    let d = utc.to_offset(off).date();
    Some(format!(
        "{:04}-{:02}-{:02}",
        d.year(),
        u8::from(d.month()),
        d.day()
    ))
}

/// Parses `YYYY-MM-DDThh:mm:ss[.fff](Z|±hh:mm)` (what `toISOString` / `Clock::now_iso` produce).
fn parse_iso(ts: &str) -> Option<OffsetDateTime> {
    let b = ts.as_bytes();
    if b.len() < 20 || !ts.is_ascii() {
        return None;
    }
    let num = |a: usize, z: usize| -> Option<i32> { ts.get(a..z)?.parse::<i32>().ok() };
    if b[4] != b'-'
        || b[7] != b'-'
        || !(b[10] == b'T' || b[10] == b't' || b[10] == b' ')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, s) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    let mut i = 19;
    if b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    let rest = ts.get(i..)?;
    let offset_secs = match rest {
        "Z" | "z" => 0,
        _ => {
            let sign = match rest.as_bytes().first()? {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let hh: i32 = rest.get(1..3)?.parse().ok()?;
            let mm: i32 = rest.get(4..6)?.parse().ok()?;
            if rest.len() != 6 || rest.as_bytes()[3] != b':' {
                return None;
            }
            sign * (hh * 3600 + mm * 60)
        }
    };
    let date = Date::from_calendar_date(
        y,
        Month::try_from(u8::try_from(mo).ok()?).ok()?,
        u8::try_from(d).ok()?,
    )
    .ok()?;
    let dt = date
        .with_hms(
            u8::try_from(h).ok()?,
            u8::try_from(mi).ok()?,
            u8::try_from(s).ok()?,
        )
        .ok()?;
    Some(dt.assume_offset(UtcOffset::from_whole_seconds(offset_secs).ok()?))
}

/// History statistics; `today_local` in `YYYY-MM-DD` format, local time zone of the machine.
pub fn compute_stats(history: &[HistoryEntry], today_local: &str) -> Stats {
    compute_stats_at(history, today_local, None)
}

/// Variant with an explicit time offset (tests, multi-threaded callers).
pub fn compute_stats_at(
    history: &[HistoryEntry],
    today_local: &str,
    offset_secs: Option<i32>,
) -> Stats {
    let mut words_today = 0u32;
    let mut total = 0u32;
    let mut audio_ms = 0u64;
    let mut words_timed = 0u64;
    let mut latency = 0u64;
    let mut latency_n = 0u64;
    for h in history {
        total = total.saturating_add(h.words);
        if local_date_of(&h.ts, offset_secs).as_deref() == Some(today_local) {
            words_today = words_today.saturating_add(h.words);
        }
        if h.audio_ms > 0 {
            audio_ms += h.audio_ms;
            words_timed += u64::from(h.words);
        }
        if h.latency_ms > 0 {
            latency += h.latency_ms;
            latency_n += 1;
        }
    }
    // Rounding of half-values towards +infinity; all the values are positive.
    let round = |x: f64| (x + 0.5).floor();
    Stats {
        words_today,
        total_words: total,
        dictations: u32::try_from(history.len()).unwrap_or(u32::MAX),
        avg_wpm: if audio_ms > 0 {
            round(words_timed as f64 / (audio_ms as f64 / 60_000.0))
        } else {
            0.0
        },
        avg_latency_ms: if latency_n > 0 {
            round(latency as f64 / latency_n as f64)
        } else {
            0.0
        },
        minutes_spoken: round(audio_ms as f64 / 60_000.0),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::domain::PasteDisposition;

    fn entry(ts: &str, words: u32, audio_ms: u64, latency_ms: u64) -> HistoryEntry {
        HistoryEntry {
            id: "x".into(),
            ts: ts.into(),
            text: String::new(),
            raw: String::new(),
            words,
            audio_ms,
            stt_ms: 0,
            latency_ms,
            language: None,
            app: None,
            pasted: PasteDisposition::Clipboard,
        }
    }

    #[test]
    fn empty_history_is_all_zero() {
        assert_eq!(
            compute_stats_at(&[], "2026-10-24", Some(0)),
            Stats::default()
        );
    }

    #[test]
    fn basic_aggregates() {
        let h = vec![
            entry("2026-10-24T10:00:00.000Z", 30, 12_000, 400),
            entry("2026-10-24T11:00:00Z", 10, 8_000, 600),
            entry("2026-10-23T11:00:00Z", 60, 60_000, 0),
        ];
        let s = compute_stats_at(&h, "2026-10-24", Some(0));
        assert_eq!(s.words_today, 40);
        assert_eq!(s.total_words, 100);
        assert_eq!(s.dictations, 3);
        // 100 words / (80 s = 1.333 min) = 75
        assert_eq!(s.avg_wpm, 75.0);
        assert_eq!(s.avg_latency_ms, 500.0);
        assert_eq!(s.minutes_spoken, 1.0);
    }

    #[test]
    fn entries_without_audio_or_latency_are_skipped_for_averages() {
        let h = vec![
            entry("2026-10-24T10:00:00Z", 50, 0, 0),
            entry("2026-10-24T10:00:00Z", 20, 30_000, 0),
        ];
        let s = compute_stats_at(&h, "2026-10-24", Some(0));
        assert_eq!(s.total_words, 70);
        assert_eq!(s.avg_wpm, 40.0); // only the 20 timed words count
        assert_eq!(s.avg_latency_ms, 0.0);
        let only_untimed = compute_stats_at(&h[..1], "2026-10-24", Some(0));
        assert_eq!(only_untimed.avg_wpm, 0.0);
        assert_eq!(only_untimed.minutes_spoken, 0.0);
    }

    #[test]
    fn rounding_half_up() {
        // 30 s -> 0.5 min -> rounded to 1 (half up)
        let s = compute_stats_at(&[entry("2026-10-24T10:00:00Z", 1, 30_000, 1)], "x", Some(0));
        assert_eq!(s.minutes_spoken, 1.0);
        let h = vec![
            entry("2026-10-24T10:00:00Z", 1, 1000, 1),
            entry("2026-10-24T10:00:00Z", 1, 1000, 2),
        ];
        assert_eq!(compute_stats_at(&h, "x", Some(0)).avg_latency_ms, 2.0); // 1,5 -> 2
    }

    #[test]
    fn today_uses_local_date_not_utc() {
        // 23:30 UTC on the 23rd = 01:30 on the 24th at UTC+2
        let h = vec![entry("2026-10-23T23:30:00Z", 5, 1000, 1)];
        assert_eq!(
            compute_stats_at(&h, "2026-10-24", Some(2 * 3600)).words_today,
            5
        );
        assert_eq!(compute_stats_at(&h, "2026-10-24", Some(0)).words_today, 0);
        assert_eq!(
            compute_stats_at(&h, "2026-10-23", Some(-5 * 3600)).words_today,
            5
        );
    }

    #[test]
    fn invalid_timestamp_never_counts_today() {
        let h = vec![
            entry("n'importe quoi", 9, 0, 0),
            entry("", 1, 0, 0),
            entry("2026-13-40T00:00:00Z", 2, 0, 0),
        ];
        let s = compute_stats_at(&h, "2026-10-24", Some(0));
        assert_eq!(s.words_today, 0);
        assert_eq!(s.total_words, 12);
    }

    #[test]
    fn parses_offsets_in_timestamp() {
        assert_eq!(
            local_date_of("2026-10-24T01:00:00+02:00", Some(0)).as_deref(),
            Some("2026-10-23")
        );
        assert_eq!(
            local_date_of("2026-10-24T23:00:00.123456-03:00", Some(0)).as_deref(),
            Some("2026-10-25")
        );
        assert_eq!(local_date_of("2026-10-24", Some(0)), None);
    }

    #[test]
    fn default_offset_path_does_not_panic() {
        let _ = compute_stats(&[entry("2026-10-24T10:00:00Z", 1, 1, 1)], "2026-10-24");
    }
}
