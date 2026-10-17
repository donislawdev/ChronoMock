//! Session zone and instant conversions: offsets, FILETIME, Unix epoch, and their labels.
//!
//! One place for the arithmetic that untouchable rule 2 governs - the moment a user types is local
//! in the session zone, everything internal is UTC, and the zone is always spelled out. These were
//! scattered across the driver, the calculator and the CDP path, which is how the session-zone
//! default drifted into three different answers once (R2-S7).

use chrono_core::{filetime_utc_to_wall, Moment, SessionMomentError};

pub(crate) fn now_epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// FILETIME ticks (100 ns since 1601) at the Unix epoch (1970-01-01T00:00:00Z).
pub(crate) const FT_UNIX_EPOCH: i64 = 116_444_736_000_000_000;

/// The last fake instant a session clock shows, in Unix-epoch ms: the native clamp
/// (`chrono_core::FAKE_WALL_MAX`, a whole second, so the division is exact). A Chromium session's
/// clock and the shim in every page stop here, so the pages, the panel and a natively hooked host all
/// stand at one moment (R4-S8).
pub(crate) const WALL_MAX_MS: i64 = (chrono_core::FAKE_WALL_MAX - FT_UNIX_EPOCH) / 10_000;

/// 1601-01-01T00:00:00Z in Unix-epoch ms: the bottom of the same range. A clock never runs backward, so
/// this is a net under an entry that is already checked, as it is on the native side.
pub(crate) const WALL_MIN_MS: i64 = -FT_UNIX_EPOCH / 10_000;

/// Unix-epoch ms for a session moment (local-in-zone, internally UTC - rule 2), through the same gate
/// the native session holds a moment to (`chrono_core::session_instant`), so the CDP and native paths
/// agree on the instant AND on which moments a session may run at (R4-S7, R4-S9).
pub(crate) fn moment_epoch_ms(local: &str, bias: i32) -> Result<i64, SessionMomentError> {
    let m = Moment { local: local.to_string(), tz_bias_min: Some(bias) };
    let (ft, _) = chrono_core::session_instant(&m)?;
    Ok((ft - FT_UNIX_EPOCH) / 10_000)
}

/// Session-zone wall-clock text for a Unix-epoch ms instant, via the core formatter (one source of
/// truth for the civil half).
pub(crate) fn epoch_ms_to_wall(epoch_ms: i64, bias: i32) -> String {
    let ft = epoch_ms.saturating_mul(10_000).saturating_add(FT_UNIX_EPOCH);
    filetime_utc_to_wall(ft, bias)
}

/// Parse a "+HH:MM" / "-HH:MM" offset into a session bias in minutes.
/// UTC = local + bias, so a local zone of UTC+2 gives bias -120.
pub(crate) fn parse_zone_to_bias(raw: &str) -> Result<i32, String> {
    let bytes = raw.as_bytes();
    let sign = match bytes.first() {
        Some(b'+') => 1,
        Some(b'-') => -1,
        _ => return Err(format!("zone must start with + or -, got '{raw}'")),
    };
    let rest = &raw[1..];
    let (h, m) = rest
        .split_once(':')
        .ok_or_else(|| format!("zone must look like +HH:MM, got '{raw}'"))?;
    // An INNER sign is rejected, not silently taken as a component (M-4). Parsing as u32 caught a minus
    // and let a PLUS through, because Rust's integer parsers accept one - so `++05:00` and `+05:+30`
    // were valid zones (R4-N34). The digits check is what refuses both. Range-check hours 0..=14 and
    // minutes 0..=59, which also keeps
    // `hours * 60 + mins` well inside i32: the old i32 parse overflowed on a huge value and, in a
    // release build (no overflow-checks), WRAPPED to a wrong bias - a silently wrong session time,
    // exactly the "off by N hours" error untouchable rule 2 guards against.
    //
    // The bound is 14 on BOTH sides, though the real map runs -12:00..=+14:00, so -13:00 and -14:00
    // are offsets no place on earth uses. Deliberate, and left as it is after review (R2-C4): this is
    // a tool for putting an app in a time it will not otherwise see, and refusing an offset merely
    // because no country uses it would drop coverage to buy nothing (untouchable rule 27). The
    // session stays internally consistent at any offset in the band. What was wrong was the MESSAGE,
    // which said "0..=14" as if that were the map - it now says which part is real.
    if !chrono_core::is_ascii_digits(h) {
        return Err(format!("bad zone hours in '{raw}' (digits only)"));
    }
    if !chrono_core::is_ascii_digits(m) {
        return Err(format!("bad zone minutes in '{raw}' (digits only)"));
    }
    let hours: u32 = h.parse().map_err(|_| format!("bad zone hours in '{raw}' (digits only)"))?;
    let mins: u32 = m.parse().map_err(|_| format!("bad zone minutes in '{raw}' (digits only)"))?;
    if hours > 14 {
        return Err(format!(
            "zone hours out of range in '{raw}' (0..=14 - real zones run -12:00..=+14:00, and this tool allows the wider band on purpose)"
        ));
    }
    if mins > 59 {
        return Err(format!("zone minutes out of range in '{raw}' (0..=59)"));
    }
    let offset = sign * (hours as i32 * 60 + mins as i32);
    Ok(-offset)
}

/// Real UTC now as FILETIME ticks (100 ns since 1601), via std - the driver is not
/// hooked, so this is genuine.
pub(crate) fn now_filetime_utc() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    (chrono_core::DAYS_1601_TO_1970 * 86_400 + d.as_secs() as i64) * 10_000_000 + (d.subsec_nanos() as i64 / 100)
}

/// Format a session bias (minutes, UTC = local + bias) back to a "+HH:MM" zone label - the core's one
/// label ([`chrono_core::bias_label`]), which the calculator's own output uses too (R5-A11).
pub(crate) fn format_bias(bias: i32) -> String {
    chrono_core::bias_label(bias)
}

/// The session zone in minutes (UTC = local + bias) for a command that named one or did not: the
/// caller's `--zone` when given, else the HOST's offset.
///
/// A named function rather than an `unwrap_or_else` repeated at each site, because it was repeated
/// at each site and the copies drifted: `chrono run` with no `--at` followed the host while `chrono
/// calc`, a relative `--at` and `run --preset` all fell back to UTC. Two halves of one tool then
/// disagreed about what day it is, and in a zone ahead of UTC `--base today` returned YESTERDAY
/// between midnight and the offset (R2-S7). One rule, one place, one test.
pub(crate) fn session_zone_default(named: Option<i32>) -> i32 {
    named.unwrap_or_else(chrono_mech::host_tz_bias_min)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zone_parses_valid_offsets_and_rejects_garbage() {
        // UTC = local + bias, so a local +HH:MM gives bias -(H*60 + M). Real offsets across the range parse.
        assert_eq!(parse_zone_to_bias("+00:00").unwrap(), 0);
        assert_eq!(parse_zone_to_bias("+02:00").unwrap(), -120);
        assert_eq!(parse_zone_to_bias("-05:00").unwrap(), 300);
        assert_eq!(parse_zone_to_bias("+05:45").unwrap(), -345); // Nepal
        assert_eq!(parse_zone_to_bias("+14:00").unwrap(), -840); // max real offset
        assert_eq!(parse_zone_to_bias("-12:00").unwrap(), 720);
        // An INNER sign is rejected (M-4), never silently taken as a negative component.
        assert!(parse_zone_to_bias("+-5:00").is_err());
        assert!(parse_zone_to_bias("+05:-30").is_err());
        // And a PLUS, which the u32 parse alone let through (R4-N34).
        assert!(parse_zone_to_bias("++05:00").is_err());
        assert!(parse_zone_to_bias("+05:+30").is_err());
        assert!(parse_zone_to_bias("+ 5:00").is_err());
        // Out of range, non-digit, missing parts - all rejected, no silent overflow/wrap.
        assert!(parse_zone_to_bias("+99:00").is_err());
        assert!(parse_zone_to_bias("+05:99").is_err());
        assert!(parse_zone_to_bias("+99999999:00").is_err()); // used to overflow i32 in a release build
        assert!(parse_zone_to_bias("05:00").is_err()); // no leading sign
        assert!(parse_zone_to_bias("+ab:cd").is_err());
        assert!(parse_zone_to_bias("+05").is_err()); // no minutes
    }

    #[test]
    fn format_bias_maps_common_zones() {
        assert_eq!(format_bias(0), "+00:00");
        assert_eq!(format_bias(-120), "+02:00"); // UTC+2
        assert_eq!(format_bias(300), "-05:00"); // UTC-5
    }

    #[test]
    fn the_session_zone_default_is_the_host_not_utc() {
        // R2-S7. `chrono calc --base today` used to resolve "now" in UTC while `chrono run` beside it
        // already followed the host, so in a zone ahead of UTC the calculator returned YESTERDAY
        // between midnight and the offset - a silent wrong date, the class this project calls
        // inadmissible. An explicit --zone still wins.
        assert_eq!(session_zone_default(Some(-120)), -120, "an explicit zone is never overridden");
        assert_eq!(session_zone_default(Some(0)), 0, "an explicit UTC is a choice, not an absence");
        assert_eq!(
            session_zone_default(None),
            chrono_mech::host_tz_bias_min(),
            "with no zone named, both surfaces read the host's"
        );
    }

    /// Random properties of the zone reader - the text after `--zone` and `--to-zone`, typed by
    /// whoever runs the command.
    mod properties {
        use super::*;
        use proptest::prelude::*;

        /// What the reader must make of `raw`, worked out apart from it: a sign, digits, a colon and
        /// digits, hours up to 14 and minutes up to 59, read in u128 so no length of digits can
        /// overflow the judge. `None` is a refusal.
        fn bias_of(raw: &str) -> Option<i32> {
            let sign: i128 = match raw.as_bytes().first()? {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let (hours, minutes) = raw[1..].split_once(':')?;
            let value = |digits: &str| -> Option<u128> {
                if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                digits.bytes().try_fold(0u128, |n, b| n.checked_mul(10)?.checked_add(u128::from(b - b'0')))
            };
            let (hours, minutes) = (value(hours)?, value(minutes)?);
            if hours > 14 || minutes > 59 {
                return None;
            }
            i32::try_from(-sign * (hours as i128 * 60 + minutes as i128)).ok()
        }

        /// A number as a zone writes one: at its limit and one past it far more often than chance
        /// would give, with or without the leading zero, no digits at all, or more than any integer
        /// holds.
        fn number(limit: u32) -> impl Strategy<Value = String> {
            prop_oneof![
                3 => (0..=limit + 1).prop_map(|n| format!("{n:02}")),
                1 => (0..=limit + 1).prop_map(|n| n.to_string()),
                1 => "[0-9]{0,4}",
                1 => "[0-9]{12,30}",
            ]
        }

        /// A sign inside the text, where none belongs - mostly absent.
        fn inner_sign() -> impl Strategy<Value = String> {
            prop_oneof![6 => Just(String::new()), 1 => "[-+]"]
        }

        /// Text a zone is written with, built from its parts so that the shapes the reader has to
        /// judge come up often. A pattern over the characters alone gave `++05:00` and an hour of
        /// exactly 15 so rarely that putting back R4-N34 and widening the band to fifteen hours both
        /// passed 256 cases (measured 2026-10-17, MISSED twice).
        fn written() -> impl Strategy<Value = String> {
            let sign = prop_oneof![4 => "[-+]", 1 => "", 1 => "[-+]{2}", 1 => "[^-+0-9]"];
            let colon = prop_oneof![6 => Just(":".to_string()), 1 => Just(String::new()), 1 => Just("::".to_string())];
            let tail = prop_oneof![6 => Just(String::new()), 1 => "[: 0-9a-z]{1,3}"];
            let built = (sign, inner_sign(), number(14), colon, inner_sign(), number(59), tail)
                .prop_map(|(s, before_hours, h, c, before_minutes, m, t)| format!("{s}{before_hours}{h}{c}{before_minutes}{m}{t}"));
            prop_oneof![8 => built, 1 => ".{0,10}"]
        }

        proptest! {
            /// R4-N34 and the wrapped bias. Any text is a bias inside the band, exactly the one its
            /// digits say, or a refusal - `++05:00` was taken as a zone, and a huge hour count once
            /// wrapped to a silently wrong session time (untouchable rule 2).
            #[test]
            fn any_text_is_the_bias_it_writes_or_a_refusal(raw in written()) {
                prop_assert_eq!(parse_zone_to_bias(&raw).ok(), bias_of(&raw), "for {:?}", raw);
            }

            /// Every bias in the band reads back from the label every surface prints for it - the
            /// sign convention (UTC = local + bias) holds in both directions.
            #[test]
            fn every_bias_in_the_band_reads_back_from_its_label(bias in -(14 * 60 + 59)..=(14 * 60 + 59)) {
                prop_assert_eq!(parse_zone_to_bias(&format_bias(bias)), Ok(bias));
            }
        }
    }
}
