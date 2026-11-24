//! UTC timestamps without a calendar crate.
//!
//! std has `SystemTime` but no calendar, and the job records written by the
//! old Node runtime store `Date.prototype.toISOString()` strings. Records
//! from both runtimes must stay interchangeable, so formatting matches JS
//! byte for byte (including the `+YYYYYY` / `-YYYYYY` extended years), and
//! parsing accepts what V8 accepts for the ISO forms we can meet in a record.
//!
//! The calendar maths is Howard Hinnant's days-from-civil / civil-from-days
//! (proleptic Gregorian, exact for the whole JS Date range).

use std::time::{SystemTime, UNIX_EPOCH};

const MS_PER_DAY: i64 = 86_400_000;

/// ECMAScript time values are limited to ±8.64e15 ms (±100,000,000 days);
/// `toISOString` throws outside it. We clamp instead of failing so a hostile
/// or corrupt value can never panic or print garbage.
const JS_MAX_MS: i64 = 8_640_000_000_000_000;

/// Current time in unix milliseconds. A clock set before 1970 yields a
/// negative value rather than an error.
pub(crate) fn now_millis() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_millis()).unwrap_or(i64::MAX),
        Err(e) => {
            let back = e.duration();
            // Round toward -infinity like JS Date.now() would on such a clock.
            let ms = i64::try_from(back.as_millis()).unwrap_or(i64::MAX);
            let extra = i64::from(back.subsec_nanos() % 1_000_000 != 0);
            -(ms.saturating_add(extra))
        }
    }
}

/// `new Date(ms).toISOString()`, e.g. `2026-05-31T15:02:38.473Z`.
/// Values outside the JS Date range are clamped to its ends.
pub(crate) fn iso_from_millis(ms: i64) -> String {
    let ms = ms.clamp(-JS_MAX_MS, JS_MAX_MS);
    let days = ms.div_euclid(MS_PER_DAY);
    let rem = ms.rem_euclid(MS_PER_DAY);
    let (y, m, d) = civil_from_days(days);
    let hh = rem / 3_600_000;
    let mm = rem / 60_000 % 60;
    let ss = rem / 1000 % 60;
    let frac = rem % 1000;
    let year = if (0..=9999).contains(&y) {
        format!("{y:04}")
    } else if y > 9999 {
        format!("+{y:06}")
    } else {
        format!("-{:06}", -y)
    };
    format!("{year}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}.{frac:03}Z")
}

/// `new Date().toISOString()`.
pub(crate) fn iso_now() -> String {
    iso_from_millis(now_millis())
}

/// Parse an ISO-8601 timestamp the way V8's `new Date(s)` does for the
/// date-time-string format, returning unix ms.
///
/// Accepted: `YYYY`, `YYYY-MM`, `YYYY-MM-DD` (UTC), optionally followed by
/// `THH:MM[:SS[.fff…]]` and a mandatory `Z` / `±HH:MM` / `±HHMM` offset.
/// Years may be `±YYYYYY`. Like V8: day 1–31 is accepted for any month and
/// overflows (`02-30` is 2 March), `24:00:00` means next midnight, extra
/// fraction digits are truncated, `T`/`Z` are case-insensitive.
///
/// Deliberately rejected (V8 would accept): a time with no offset, which V8
/// reads as *local* time — that depends on the machine's zone and never
/// appears in records we write; and V8's legacy free-form formats.
pub(crate) fn parse_iso_millis(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let mut p = Cursor { b, i: 0 };

    let year = match p.peek() {
        Some(c @ (b'+' | b'-')) => {
            p.i += 1;
            let v = p.digits(6)?;
            if c == b'-' {
                // "-000000" is explicitly invalid in ECMAScript.
                if v == 0 {
                    return None;
                }
                -v
            } else {
                v
            }
        }
        _ => p.digits(4)?,
    };
    let mut month = 1;
    let mut day = 1;
    if p.eat(b'-') {
        month = p.digits(2)?;
        if p.eat(b'-') {
            day = p.digits(2)?;
        }
    }
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let mut time_ms = 0i64;
    let mut offset_ms = 0i64;
    if matches!(p.peek(), Some(b'T' | b't')) {
        p.i += 1;
        let hh = p.digits(2)?;
        if !p.eat(b':') {
            return None;
        }
        let mi = p.digits(2)?;
        let mut ss = 0;
        let mut ms = 0;
        if p.eat(b':') {
            ss = p.digits(2)?;
            if p.eat(b'.') {
                let start = p.i;
                let mut n = 0;
                while let Some(c) = p.peek().filter(u8::is_ascii_digit) {
                    if n < 3 {
                        ms = ms * 10 + i64::from(c - b'0');
                    }
                    n += 1;
                    p.i += 1;
                }
                if p.i == start {
                    return None;
                }
                for _ in n..3 {
                    ms *= 10;
                }
            }
        }
        let midnight_24 = hh == 24 && mi == 0 && ss == 0 && ms == 0;
        if !(hh <= 23 || midnight_24) || mi > 59 || ss > 59 {
            return None;
        }
        time_ms = ((hh * 60 + mi) * 60 + ss) * 1000 + ms;

        match p.peek() {
            Some(b'Z' | b'z') => p.i += 1,
            Some(c @ (b'+' | b'-')) => {
                p.i += 1;
                let oh = p.digits(2)?;
                p.eat(b':');
                let om = p.digits(2)?;
                if oh > 23 || om > 59 {
                    return None;
                }
                let off = (oh * 60 + om) * 60_000;
                offset_ms = if c == b'+' { off } else { -off };
            }
            // No offset: V8 would use local time. See doc comment.
            _ => return None,
        }
    }
    if p.i != b.len() {
        return None;
    }

    let days = days_from_civil(year, month, day);
    let total = days
        .checked_mul(MS_PER_DAY)?
        .checked_add(time_ms)?
        .checked_sub(offset_ms)?;
    if total.abs() > JS_MAX_MS {
        return None;
    }
    Some(total)
}

/// Tiny byte cursor for `parse_iso_millis`.
struct Cursor<'a> {
    b: &'a [u8],
    i: usize,
}

impl Cursor<'_> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    /// Exactly `n` ASCII digits (n <= 6, so no overflow).
    fn digits(&mut self, n: usize) -> Option<i64> {
        let mut v = 0i64;
        for _ in 0..n {
            let c = self.peek().filter(u8::is_ascii_digit)?;
            v = v * 10 + i64::from(c - b'0');
            self.i += 1;
        }
        Some(v)
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date. `d` may exceed the
/// month length; the result simply runs on into the next month (V8 parity).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Inverse of `days_from_civil`: (year, month 1-12, day 1-31).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Expected strings below were produced by `new Date(ms).toISOString()`
    // in Node so the port is pinned to JS output.
    const JS_CASES: &[(i64, &str)] = &[
        (0, "1970-01-01T00:00:00.000Z"),
        (-1, "1969-12-31T23:59:59.999Z"),
        (1, "1970-01-01T00:00:00.001Z"),
        (-86_400_000, "1969-12-31T00:00:00.000Z"),
        (-86_400_001, "1969-12-30T23:59:59.999Z"),
        (951_782_400_000, "2000-02-29T00:00:00.000Z"),
        (951_868_800_000, "2000-03-01T00:00:00.000Z"),
        (946_684_799_999, "1999-12-31T23:59:59.999Z"),
        (1_709_164_800_000, "2024-02-29T00:00:00.000Z"),
        (1_709_251_199_999, "2024-02-29T23:59:59.999Z"),
        (4_107_456_000_000, "2100-02-28T00:00:00.000Z"),
        (4_107_542_400_000, "2100-03-01T00:00:00.000Z"),
        (1_780_239_758_473, "2026-05-31T15:02:38.473Z"),
        (-2_208_988_800_000, "1900-01-01T00:00:00.000Z"),
        (-5_097_600_001, "1969-11-02T23:59:59.999Z"),
        (-62_135_596_800_000, "0001-01-01T00:00:00.000Z"),
        (-62_167_219_200_000, "0000-01-01T00:00:00.000Z"),
        (-62_167_219_200_001, "-000001-12-31T23:59:59.999Z"),
        (-62_198_755_200_000, "-000001-01-01T00:00:00.000Z"),
        (253_402_300_799_999, "9999-12-31T23:59:59.999Z"),
        (253_402_300_800_000, "+010000-01-01T00:00:00.000Z"),
        (8_640_000_000_000_000, "+275760-09-13T00:00:00.000Z"),
        (-8_640_000_000_000_000, "-271821-04-20T00:00:00.000Z"),
        (2_147_483_647_000, "2038-01-19T03:14:07.000Z"),
        (13_574_563_200_000, "2400-02-29T00:00:00.000Z"),
    ];

    #[test]
    fn iso_from_millis_matches_js() {
        for &(ms, want) in JS_CASES {
            assert_eq!(iso_from_millis(ms), want, "ms={ms}");
        }
    }

    #[test]
    fn iso_from_millis_clamps_out_of_range() {
        assert_eq!(iso_from_millis(i64::MAX), "+275760-09-13T00:00:00.000Z");
        assert_eq!(iso_from_millis(i64::MIN), "-271821-04-20T00:00:00.000Z");
    }

    #[test]
    fn parse_round_trips_js_cases() {
        for &(ms, s) in JS_CASES {
            assert_eq!(parse_iso_millis(s), Some(ms), "{s}");
        }
    }

    #[test]
    fn round_trip_sweep() {
        // Every ~13.7 days across ±1000 years, with odd ms offsets.
        let mut ms = -31_556_952_000_000i64;
        while ms < 31_556_952_000_000 {
            let s = iso_from_millis(ms);
            assert_eq!(parse_iso_millis(&s), Some(ms), "{s}");
            ms += 1_183_746_123;
        }
    }

    #[test]
    fn civil_days_consistent() {
        for z in -800_000..800_000i64 {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
    }

    #[test]
    fn parse_variants_match_v8() {
        // Expected values from `new Date(s).toISOString()` in Node.
        let cases: &[(&str, &str)] = &[
            ("2026-05-28T15:00:00Z", "2026-05-28T15:00:00.000Z"),
            ("2026-05-28T15:00:00+00:00", "2026-05-28T15:00:00.000Z"),
            ("2026-05-28T15:00:00-00:00", "2026-05-28T15:00:00.000Z"),
            ("2026-05-28T15:00:00+02:00", "2026-05-28T13:00:00.000Z"),
            ("2026-05-28T15:00:00+0200", "2026-05-28T13:00:00.000Z"),
            ("2026-05-28T15:00:00+23:59", "2026-05-27T15:01:00.000Z"),
            ("2026-05-28T15:00:00.1239Z", "2026-05-28T15:00:00.123Z"),
            ("2026-05-28T15:00:00.1Z", "2026-05-28T15:00:00.100Z"),
            ("2026-05-28T24:00:00Z", "2026-05-29T00:00:00.000Z"),
            ("2026-05-28T15:00Z", "2026-05-28T15:00:00.000Z"),
            ("2026-05-28t15:00:00z", "2026-05-28T15:00:00.000Z"),
            ("2026-05-28", "2026-05-28T00:00:00.000Z"),
            ("2026-05", "2026-05-01T00:00:00.000Z"),
            ("2026", "2026-01-01T00:00:00.000Z"),
            ("2026-02-30T00:00:00Z", "2026-03-02T00:00:00.000Z"),
            ("2026-02-29T00:00:00Z", "2026-03-01T00:00:00.000Z"),
            ("2024-02-29T00:00:00Z", "2024-02-29T00:00:00.000Z"),
            ("+010000-01-01T00:00:00Z", "+010000-01-01T00:00:00.000Z"),
            ("-000001-01-01T00:00:00Z", "-000001-01-01T00:00:00.000Z"),
            ("1969-12-31T23:59:59.999Z", "1969-12-31T23:59:59.999Z"),
        ];
        for &(input, want) in cases {
            let ms = parse_iso_millis(input);
            assert_eq!(ms.map(iso_from_millis).as_deref(), Some(want), "{input}");
        }
    }

    #[test]
    fn parse_rejects_what_v8_rejects() {
        for s in [
            "",
            "garbage",
            "2026-05-28T24:00:01Z",
            "-000000-01-01T00:00:00Z",
            "2026-05-28T15:00:60Z",
            "2026-05-28T15:60:00Z",
            "2026-13-01",
            "2026-00-01",
            "2026-05-00",
            "2026-05-32",
            " 2026-05-28T15:00:00Z",
            "2026-05-28T15:00:00Z ",
            "2026-05-28T15:00:00.Z",
            "2026-05-28T15:00:00+24:00",
            "2026-05-28T15:00:00+12:60",
            "+275760-09-13T00:00:00.001Z",
            "-271821-04-19T23:59:59.999Z",
            "20260528",
            "2026-05-28T",
            "2026-05-28T15",
            "2026-05-28T15:00:00+",
            "２０２６-05-28",
        ] {
            assert_eq!(parse_iso_millis(s), None, "{s:?}");
        }
    }

    #[test]
    fn parse_rejects_local_time_forms() {
        // V8 reads these as local time; we refuse rather than guess a zone.
        assert_eq!(parse_iso_millis("2026-05-28T15:00:00"), None);
        assert_eq!(parse_iso_millis("2026-05-28T15:00:00.000"), None);
        // Legacy (non-ISO) date forms, also local time in V8.
        assert_eq!(parse_iso_millis("2026-5-28"), None);
    }

    #[test]
    fn parse_range_edges() {
        assert_eq!(
            parse_iso_millis("+275760-09-13T00:00:00.000Z"),
            Some(JS_MAX_MS)
        );
        assert_eq!(
            parse_iso_millis("-271821-04-20T00:00:00.000Z"),
            Some(-JS_MAX_MS)
        );
        // Max-width garbage must not overflow.
        assert_eq!(parse_iso_millis("+999999-12-31T23:59:59.999Z"), None);
        assert_eq!(parse_iso_millis("-999999-01-01T00:00:00.000Z"), None);
    }

    #[test]
    fn now_is_sane() {
        let a = now_millis();
        // After 2020-01-01 and before 2200.
        assert!(a > 1_577_836_800_000);
        assert!(a < 7_258_118_400_000);
        let s = iso_now();
        assert_eq!(s.len(), 24);
        assert!(s.ends_with('Z'));
        let parsed = parse_iso_millis(&s).unwrap_or(0);
        assert!(parsed >= a);
    }
}
