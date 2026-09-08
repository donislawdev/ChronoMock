//! Session zone and instant conversions: offsets, FILETIME, Unix epoch, and their labels.
//!
//! One place for the arithmetic that untouchable rule 2 governs - the moment a user types is local
//! in the session zone, everything internal is UTC, and the zone is always spelled out. These were
//! scattered across the driver, the calculator and the CDP path, which is how the session-zone
//! default drifted into three different answers once (R2-S7).

use chrono_core::{filetime_utc_to_wall, Moment};

pub(crate) fn now_epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// FILETIME ticks (100 ns since 1601) at the Unix epoch (1970-01-01T00:00:00Z).
pub(crate) const FT_UNIX_EPOCH: i64 = 116_444_736_000_000_000;

/// Unix-epoch ms for a session moment (local-in-zone, internally UTC - rule 2), reusing the core's
/// anchor math so the CDP and native paths agree on the instant. `None` if the moment is out of the
/// representable range.
pub(crate) fn moment_epoch_ms(local: &str, bias: Option<i32>) -> Option<i64> {
    let m = Moment { local: local.to_string(), tz_bias_min: bias };
    let ft = chrono_core::moment_to_filetime_utc(&m).ok()?;
    Some((ft - FT_UNIX_EPOCH) / 10_000)
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
    // Parse as u32 so an INNER sign (e.g. `+-5:00` or `+05:-30`) is rejected, not silently taken as a
    // negative component (M-4). Range-check hours 0..=14 and minutes 0..=59, which also keeps
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
    let hours: u32 = h.parse().map_err(|_| format!("bad zone hours in '{raw}' (digits only)"))?;
    let mins: u32 = m.parse().map_err(|_| format!("bad zone minutes in '{raw}' (digits only)"))?;
    if hours > 14 {
        return Err(format!(
            "zone hours out of range in '{raw}' (0..=14; real zones run -12:00..=+14:00, and this tool allows the wider band on purpose)"
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
    const DAYS_1601_TO_1970: i64 = 134_774;
    (DAYS_1601_TO_1970 * 86_400 + d.as_secs() as i64) * 10_000_000 + (d.subsec_nanos() as i64 / 100)
}

/// Format a session bias (minutes, UTC = local + bias) back to a "+HH:MM" zone label.
pub(crate) fn format_bias(bias: i32) -> String {
    let offset = -bias;
    let sign = if offset < 0 { '-' } else { '+' };
    let abs = offset.abs();
    format!("{sign}{:02}:{:02}", abs / 60, abs % 60)
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
}
