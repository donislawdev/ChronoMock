//! The name the hook hands the session zone out under (ADR-26).
//!
//! The session zone is a fixed offset, and most readers build it from the bias the hook returns. ICU,
//! which Node.js, Deno, Bun and many applications carry, names the system zone first, from what
//! `GetDynamicTimeZoneInformation` says: a whole-hour offset with daylight saving time disabled from the
//! bias alone (`Etc/GMT-5`), any other from the key name, through CLDR's table of Windows zones. The
//! session's own name is in no such table, so at any other offset ICU found no zone at all: the runtimes
//! on V8 kept the right offset with no name, others showed UTC (measured 2026-11-12, `tools/probes/icu-zone`).
//!
//! At such an offset the zone can borrow the key name of a real Windows zone at the same offset, but only
//! from the moment the IANA zone CLDR maps that name to has kept the offset without daylight saving time.
//! ICU reads the whole history of that IANA zone, so at an earlier moment it shows the offset the zone had
//! then instead of the session's. Which name a session takes is decided where the block is written
//! (`chrono_mech::zone_name`), and this is the table both sides read: the mechanism the offsets and the
//! dates, the hook the key names behind the index it finds in [`crate::Ctl::zone_name`].

/// The session zone's own name, which no registry knows: the standard name of every answer the hook gives,
/// and the key name when none is borrowed.
pub const SESSION_ZONE_NAME: &str = "Chrono Session";

/// The index [`crate::Ctl::zone_name`] holds when the zone keeps its own name. Index `i` above it is
/// `BORROWED_NAMES[i - 1]`.
pub const OWN_ZONE_NAME: u32 = 0;

/// A Windows zone whose key name the session zone can borrow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BorrowedName {
    /// The zone's key name in the Windows registry, which CLDR maps to an IANA zone.
    pub key: &'static str,
    /// The zone's offset as a Win32 bias (UTC = local + bias).
    pub bias_min: i32,
    /// The first moment of the offset the IANA zone has kept since, without daylight saving time, as UTC
    /// civil time.
    pub fixed_since_utc: &'static str,
}

/// Every Windows zone at an offset ICU does not name from the bias whose IANA zone keeps that offset
/// without daylight saving time today, with the first moment it did, in the order a session looks for one.
///
/// Measured 2026-11-12. The moments come from three copies of tzdata that agree to the second (2026f in
/// Python's `tzdata`, 2026e in the ICU of Node.js, 2026b in the JDK), the IANA zone of each key from two
/// copies of CLDR's table that agree on every one (the ICU of Windows and the JDK's `tzmappings`). Sri
/// Lanka's zone is left out: it has kept +05:30 since 2006, inside India's period at the same offset, so it
/// would never be chosen. Zones that still observe daylight saving time (Newfoundland, Adelaide, Lord Howe,
/// Chatham) never qualify. Iran's has kept +03:30 since its last daylight saving time ended in 2022.
pub const BORROWED_NAMES: [BorrowedName; 8] = [
    // Pacific/Marquesas, -09:30.
    BorrowedName { key: "Marquesas Standard Time", bias_min: 570, fixed_since_utc: "1912-10-01T09:18:00" },
    // Asia/Tehran, +03:30.
    BorrowedName { key: "Iran Standard Time", bias_min: -210, fixed_since_utc: "2022-09-21T19:30:00" },
    // Asia/Kabul, +04:30.
    BorrowedName { key: "Afghanistan Standard Time", bias_min: -270, fixed_since_utc: "1944-12-31T20:00:00" },
    // Asia/Calcutta, +05:30.
    BorrowedName { key: "India Standard Time", bias_min: -330, fixed_since_utc: "1945-10-14T17:30:00" },
    // Asia/Katmandu, +05:45.
    BorrowedName { key: "Nepal Standard Time", bias_min: -345, fixed_since_utc: "1985-12-31T18:30:00" },
    // Asia/Rangoon, +06:30.
    BorrowedName { key: "Myanmar Standard Time", bias_min: -390, fixed_since_utc: "1945-05-02T15:00:00" },
    // Australia/Eucla, +08:45.
    BorrowedName { key: "Aus Central W. Standard Time", bias_min: -525, fixed_since_utc: "2009-03-28T17:15:00" },
    // Australia/Darwin, +09:30.
    BorrowedName { key: "AUS Central Standard Time", bias_min: -570, fixed_since_utc: "1944-03-25T16:30:00" },
];

/// The borrowed name an index stands for, `None` for the zone's own name and for an index this build does
/// not know.
pub fn borrowed_name(index: u32) -> Option<&'static BorrowedName> {
    let at = usize::try_from(index.checked_sub(1)?).ok()?;
    BORROWED_NAMES.get(at)
}

/// The key name the hook hands out for an index: the borrowed one, or the session's own.
pub fn zone_key_name(index: u32) -> &'static str {
    borrowed_name(index).map_or(SESSION_ZONE_NAME, |name| name.key)
}

/// The index of a borrowed name, for the mechanism to write into the block.
pub fn borrowed_index(at: usize) -> u32 {
    u32::try_from(at + 1).unwrap_or(OWN_ZONE_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_index_names_the_borrowed_zone_above_the_own_name_and_nothing_past_the_table() {
        assert_eq!(zone_key_name(OWN_ZONE_NAME), SESSION_ZONE_NAME);
        for (at, name) in BORROWED_NAMES.iter().enumerate() {
            let index = borrowed_index(at);
            assert_eq!(borrowed_name(index), Some(name));
            assert_eq!(zone_key_name(index), name.key);
        }
        let past = borrowed_index(BORROWED_NAMES.len());
        assert_eq!(borrowed_name(past), None, "an index past the table borrows nothing");
        assert_eq!(zone_key_name(past), SESSION_ZONE_NAME);
        assert_eq!(zone_key_name(u32::MAX), SESSION_ZONE_NAME);
    }

    /// The key name goes into `TimeZoneKeyName`, 128 UTF-16 units with the terminator, and the session's
    /// own name into `StandardName` too, which has 32. `set_wide` would cut a longer one short, and a key
    /// cut short is one no table knows.
    #[test]
    fn every_name_fits_the_field_it_is_written_into() {
        for name in &BORROWED_NAMES {
            assert!(name.key.encode_utf16().count() < 128, "{} does not fit TimeZoneKeyName", name.key);
            assert!(name.key.is_ascii(), "{} is not a plain registry key", name.key);
        }
        assert!(SESSION_ZONE_NAME.encode_utf16().count() < 32);
    }

    /// Two entries at one offset would leave the second unread wherever the first's period covers it, and
    /// a whole-hour offset needs no name at all - ICU names it from the bias.
    #[test]
    fn every_entry_is_a_fractional_offset_of_its_own() {
        for (at, name) in BORROWED_NAMES.iter().enumerate() {
            assert_ne!(name.bias_min % 60, 0, "{} is a whole-hour offset", name.key);
            assert!(
                BORROWED_NAMES[..at].iter().all(|earlier| earlier.bias_min != name.bias_min),
                "{} repeats an offset already in the table",
                name.key
            );
        }
    }
}
