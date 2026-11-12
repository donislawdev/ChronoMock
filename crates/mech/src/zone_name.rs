//! Which name a session's zone is handed out under (ADR-26). Why a zone borrows a name, and the table of
//! names it can borrow, are in `chrono_ctl::zone_name`. The choice is made here, where the block is
//! written, because it needs the session's start, and a date to compare it with.

use std::cell::Cell;

use chrono_ctl::zone_name::{borrowed_index, borrowed_name, BORROWED_NAMES, OWN_ZONE_NAME};

/// Whether ICU names a zone at this bias from the bias alone. With daylight saving time disabled it names
/// a whole-hour offset `Etc/UTC` or `Etc/GMT±N`, and tzdata has those from `Etc/GMT-14` (+14:00) to
/// `Etc/GMT+12` (-12:00). A session accepts -13:00 and -14:00 too, under which ICU builds a name it does
/// not know and ends as it does at a fractional offset (measured 2026-11-12, `tools/probes/icu-zone`).
fn named_by_offset(bias_min: i32) -> bool {
    bias_min % 60 == 0 && (-14 * 60..=12 * 60).contains(&bias_min)
}

/// The UTC FILETIME a borrowed name's period starts at, `None` for the zone's own name.
fn borrowed_since(index: u32) -> Option<i64> {
    let name = borrowed_name(index)?;
    let start = chrono_core::Moment { local: name.fixed_since_utc.to_string(), tz_bias_min: Some(0) };
    chrono_core::moment_to_filetime_utc(&start).ok()
}

/// The name a session's zone is handed out under, chosen once when the session starts, and whether the
/// clock has since been jumped to before the period that name holds in.
pub(crate) struct SessionZoneName {
    bias_min: i32,
    index: u32,
    /// When the borrowed name's period starts, `None` when the zone keeps its own name.
    since: Option<i64>,
    /// Set by the first jump that lands before `since`, and kept: ICU read the zone when the program first
    /// asked, so what it showed after that jump stays shown, wherever the clock goes next.
    left_period: Cell<bool>,
}

impl SessionZoneName {
    /// The name for a session at `bias_min` that starts at `start_ft` (UTC FILETIME): the first borrowed
    /// name at that offset whose period the start lies in, or the zone's own. A whole-hour offset finds
    /// none, because ICU names it from the bias.
    pub(crate) fn choose(bias_min: i32, start_ft: i64) -> Self {
        let index = (0..BORROWED_NAMES.len())
            .map(borrowed_index)
            .find(|&index| {
                borrowed_name(index).is_some_and(|name| name.bias_min == bias_min)
                    && borrowed_since(index).is_some_and(|since| start_ft >= since)
            })
            .unwrap_or(OWN_ZONE_NAME);
        SessionZoneName { bias_min, index, since: borrowed_since(index), left_period: Cell::new(false) }
    }

    /// The index the block carries, which the hook turns back into a key name.
    pub(crate) fn index(&self) -> u32 {
        self.index
    }

    /// The clock was set to `at_ft` (UTC FILETIME) by a jump.
    pub(crate) fn note_landing(&self, at_ft: i64) {
        if self.since.is_some_and(|since| at_ft < since) {
            self.left_period.set(true);
        }
    }

    /// Whether ICU finds no zone for this session: neither the bias nor a borrowed name names it.
    pub(crate) fn unnamed_for_icu(&self) -> bool {
        self.index == OWN_ZONE_NAME && !named_by_offset(self.bias_min)
    }

    /// Whether a jump took the clock to before the period of the borrowed name.
    pub(crate) fn left_period(&self) -> bool {
        self.left_period.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_ctl::zone_name::zone_key_name;

    /// The UTC FILETIME of a moment written as UTC civil time.
    fn utc(local: &str) -> i64 {
        chrono_core::moment_to_filetime_utc(&chrono_core::Moment { local: local.into(), tz_bias_min: Some(0) })
            .expect("a moment inside the session range")
    }

    /// One tick of a FILETIME, the smallest step a clock can be set by.
    const TICK: i64 = 1;

    #[test]
    fn every_borrowed_name_starts_at_a_moment_a_session_can_hold() {
        for at in 0..BORROWED_NAMES.len() {
            let index = borrowed_index(at);
            assert!(borrowed_since(index).is_some(), "{} has a start that is not a moment", zone_key_name(index));
        }
        assert_eq!(borrowed_since(OWN_ZONE_NAME), None);
    }

    /// Each name holds from the first moment of its period on, and not one tick before it, where its IANA
    /// zone still had another offset - the zone then keeps its own name, which ICU cannot name.
    #[test]
    fn a_name_is_borrowed_from_the_first_moment_of_its_period_and_not_before() {
        for (at, name) in BORROWED_NAMES.iter().enumerate() {
            let since = utc(name.fixed_since_utc);
            let inside = SessionZoneName::choose(name.bias_min, since);
            assert_eq!(inside.index(), borrowed_index(at), "{} at the start of its period", name.key);
            assert!(!inside.unnamed_for_icu());
            let later = SessionZoneName::choose(name.bias_min, utc("2091-06-15T12:00:00"));
            assert_eq!(later.index(), borrowed_index(at), "{} long after the start of its period", name.key);
            let before = SessionZoneName::choose(name.bias_min, since - TICK);
            assert_eq!(before.index(), OWN_ZONE_NAME, "{} one tick before its period", name.key);
            assert!(before.unnamed_for_icu(), "{} before its period", name.key);
        }
    }

    /// The moments are tzdata's, as measured (`chrono_ctl::zone_name::BORROWED_NAMES`). Read here a second
    /// way, as the instant the zone's new offset began, so a moment copied in the wrong zone shows.
    #[test]
    fn india_borrows_from_the_end_of_war_time() {
        // Asia/Kolkata went from +06:30 back to +05:30 on 1945-10-15 at 00:00 local war time.
        let end_of_war_time = chrono_core::moment_to_filetime_utc(&chrono_core::Moment {
            local: "1945-10-15T00:00:00".into(),
            tz_bias_min: Some(-390),
        })
        .expect("a moment");
        assert_eq!(SessionZoneName::choose(-330, end_of_war_time).index(), borrowed_index(3));
        assert_eq!(zone_key_name(borrowed_index(3)), "India Standard Time");
        assert_eq!(SessionZoneName::choose(-330, end_of_war_time - TICK).index(), OWN_ZONE_NAME);
    }

    /// ICU names a whole-hour offset from -12:00 to +14:00 from the bias, so such a zone borrows nothing and
    /// is not reported. -13:00 and -14:00, and an offset no Windows zone keeps, are.
    #[test]
    fn icu_names_a_whole_hour_inside_tzdata_and_nothing_else_without_a_borrowed_name() {
        let start = utc("2091-06-15T12:00:00");
        for (bias, named) in [
            (0, true),
            (-60, true),
            (300, true),
            (720, true),
            (-840, true),
            (780, false),
            (840, false),
            (-83, false),
            (210, false),
            (-630, false),
            (-765, false),
        ] {
            let zone = SessionZoneName::choose(bias, start);
            assert_eq!(zone.index(), OWN_ZONE_NAME, "bias {bias} borrowed a name");
            assert_eq!(!zone.unnamed_for_icu(), named, "bias {bias}");
        }
    }

    /// A jump to before the period is remembered, and a later jump back into it does not forget it. A zone
    /// with its own name has no period to leave.
    #[test]
    fn a_jump_before_the_period_of_the_borrowed_name_is_kept() {
        let zone = SessionZoneName::choose(-330, utc("2091-06-15T12:00:00"));
        zone.note_landing(utc("1945-10-14T17:30:00"));
        assert!(!zone.left_period(), "the first moment of the period is inside it");
        zone.note_landing(utc("1944-06-15T12:00:00"));
        assert!(zone.left_period());
        zone.note_landing(utc("2091-06-15T12:00:00"));
        assert!(zone.left_period(), "a jump back does not take it back");

        let own = SessionZoneName::choose(-330, utc("1944-06-15T12:00:00"));
        own.note_landing(utc("1601-01-02T00:00:00"));
        assert!(!own.left_period());
    }
}
