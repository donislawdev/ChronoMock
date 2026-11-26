//! What the random properties of this crate share: numbers drawn from their edges as well as from
//! anywhere, a calendar shaped like any a data file may write, a deadline for a case that could
//! loop, and the canary of the weekly long run.
//!
//! Compiled for tests only (`#[cfg(test)] mod search;` in lib.rs). The properties stand beside the
//! code they search, each in a `mod properties`, so `cargo test properties` selects every one of
//! them - the weekly run in .github/workflows/search.yml relies on that name. chrono-cli has a file
//! of the same name and the same kind: a crate cannot reach another crate's test code.

use crate::calendar::{Calendar, Holiday, HolidayRule, Observed};
use proptest::prelude::*;
use std::panic::{self, AssertUnwindSafe};
use std::sync::mpsc;
use std::time::Duration;

/// Any `i64`, with its edges drawn as often as the middle. A uniform draw lands among the last few
/// values of a 64-bit range about once in 2^60 draws, so an overflow that waits there is met only
/// when it is asked for by name.
pub(crate) fn edge_i64() -> impl Strategy<Value = i64> {
    prop_oneof![
        4 => any::<i64>(),
        2 => -1_000i64..=1_000,
        1 => (i64::MAX - 8)..=i64::MAX,
        1 => i64::MIN..=(i64::MIN + 8),
    ]
}

/// `around` and the few values either side of it - a limit is wrong on its edge, not far from it.
pub(crate) fn near(around: i64, by: i64) -> impl Strategy<Value = i64> {
    around.saturating_sub(by)..=around.saturating_add(by)
}

/// A year anywhere in the band the engine computes, its two ends and the years just past them
/// drawn as often as the rest.
pub(crate) fn year() -> impl Strategy<Value = i64> {
    prop_oneof![
        4 => 1600i64..=2400,
        2 => crate::CIVIL_YEAR_MIN..=crate::CIVIL_YEAR_MAX,
        1 => near(crate::CIVIL_YEAR_MAX, 2),
        1 => near(crate::CIVIL_YEAR_MIN, 2),
    ]
}

/// A calendar as a data file may write one: any set of weekend days - none, one working day a
/// week, all seven off - any observance, up to six holidays of every rule kind, and a first year
/// or none. The two weekends drawn by name are the two the walks got wrong: a calendar with no
/// working day spun the business-day walk for ever (S-1), and one with a single working day a
/// week ran its budget dry and was refused (R4-N41).
pub(crate) fn calendar() -> impl Strategy<Value = Calendar> {
    let weekend = prop_oneof![
        1 => Just(0x7Fu8),
        1 => (0u8..7).prop_map(|working| 0x7F & !(1u8 << working)),
        4 => any::<u8>().prop_map(|bits| bits & 0x7F),
    ]
    .prop_map(|bits| (0..7u32).filter(|day| bits & (1u8 << day) != 0).collect::<Vec<u32>>());
    let rule = prop_oneof![
        (1u32..=12, 1u32..=31).prop_map(|(month, day)| HolidayRule::Fixed { month, day }),
        (1u32..=12, 0u32..=6, prop_oneof![Just(-1i32), 1i32..=5])
            .prop_map(|(month, weekday, order)| HolidayRule::NthWeekday { month, weekday, order }),
        (-120i32..=120).prop_map(|offset| HolidayRule::EasterOffset { offset }),
    ];
    let observed = prop_oneof![
        Just(Observed::None),
        Just(Observed::SatToFriSunToMon),
        Just(Observed::SunToMon),
        Just(Observed::WeekendToMon),
    ];
    let first_year = prop_oneof![3 => Just(None), 1 => (1900i64..=2100).prop_map(Some)];
    (weekend, observed, first_year, proptest::collection::vec(rule, 0..=6)).prop_map(
        |(weekend, observed, valid_from, rules)| Calendar {
            id: "drawn".into(),
            country: "XX".into(),
            weekend,
            observed,
            valid_from,
            law_as_of: None,
            holidays: rules
                .into_iter()
                .enumerate()
                .map(|(index, rule)| Holiday {
                    id: format!("drawn_{index}"),
                    name_en: format!("drawn {index}"),
                    name_local: format!("drawn {index}"),
                    rule,
                    valid_from: None,
                    valid_to: None,
                    source: "drawn".into(),
                })
                .collect(),
        },
    )
}

/// `run` on a thread of its own, red with `what` in the message when it has not finished in
/// `limit`. proptest bounds a case in time only through a forking runner this workspace does not
/// build, and a case that never ends would hang the whole test binary with nothing said - the
/// business-day walk did exactly that on a calendar without working days (S-1). A panic inside
/// `run` reaches the caller as itself, so proptest still shrinks it.
///
/// 🔴 A `run` that does not finish is LEFT RUNNING - a thread cannot be stopped from outside - and
/// every shrink step that still hangs leaves one more and waits `limit` again. So a property that
/// uses this caps its shrink in time (`max_shrink_time`), as the calendar and the calculator do.
pub(crate) fn within<T: Send + 'static>(
    limit: Duration,
    what: &str,
    run: impl FnOnce() -> T + Send + 'static,
) -> T {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(panic::catch_unwind(AssertUnwindSafe(run)));
    });
    match receiver.recv_timeout(limit) {
        Ok(Ok(value)) => value,
        Ok(Err(payload)) => panic::resume_unwind(payload),
        Err(_) => panic!("{what} did not finish in {limit:?} - a loop with no way out"),
    }
}

mod properties {
    use proptest::prelude::*;

    proptest! {
        /// The canary of the weekly run: red when CHRONO_SEARCH_CANARY is set, and a property that
        /// holds in every other run. Each part runs it first and stops if it does NOT see it fail,
        /// so a part that then finds nothing has shown it would have said so.
        #[test]
        fn the_canary_fails_only_when_asked_to(drawn in any::<u64>()) {
            let asked = std::env::var_os("CHRONO_SEARCH_CANARY").is_some();
            prop_assert!(!asked, "the canary failed, as it was asked to (drew {})", drawn);
        }
    }
}
