//! Reading a shipped calendar catalogue off disk and turning it into the core's types.
//!
//! The wire shape (`CalendarDto` and friends) is separate from the engine types on purpose: a
//! catalogue is user-facing data that must be validated before the engine ever sees it, and a bad
//! rule has to name its own field rather than fail somewhere downstream.
//!
//! The catalogue lookup lives here too, including the identifier check that refuses path traversal -
//! an id names a shipped file, never a path.
//!
//! The consumer owns the I/O and serde; the core engine works over already-parsed rules. The JSON
//! schema is the contract (docs/04 section 5) and this is one reader of it. Unknown fields are
//! ignored (additive evolution is safe); an unknown major schema version is refused.


use serde::Deserialize;
#[derive(Deserialize)]
pub(crate) struct CalendarDto {
    schema: String,
    id: String,
    country: String,
    weekend: Vec<String>,
    observed: String,
    holidays: Vec<HolidayDto>,
}

#[derive(Deserialize)]
pub(crate) struct HolidayDto {
    id: String,
    name: NameDto,
    rule: RuleDto,
    #[serde(default)]
    valid_from: Option<i64>,
    #[serde(default)]
    valid_to: Option<i64>,
    source: String,
}

#[derive(Deserialize)]
pub(crate) struct NameDto {
    en: String,
    local: String,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum RuleDto {
    Fixed { month: u32, day: u32 },
    NthWeekday { month: u32, weekday: String, order: i32 },
    EasterOffset { offset: i32 },
}

/// Map a weekday name to a Sunday-based index 0..=6.
pub(crate) fn weekday_index(name: &str) -> Result<u32, String> {
    Ok(match name.to_ascii_lowercase().as_str() {
        "sunday" => 0,
        "monday" => 1,
        "tuesday" => 2,
        "wednesday" => 3,
        "thursday" => 4,
        "friday" => 5,
        "saturday" => 6,
        other => return Err(format!("unknown weekday '{other}'")),
    })
}

pub(crate) fn observed_from(s: &str) -> Result<chrono_core::calendar::Observed, String> {
    use chrono_core::calendar::Observed;
    Ok(match s {
        "none" => Observed::None,
        "sat_to_fri_sun_to_mon" => Observed::SatToFriSunToMon,
        "sun_to_mon" => Observed::SunToMon,
        "weekend_to_mon" => Observed::WeekendToMon,
        other => return Err(format!("unknown observed rule '{other}'")),
    })
}

/// Validate and map one holiday rule. The engine trusts its inputs, and a calendar is a data file from
/// outside the build - the one documented extension point of this tool - so an out-of-range field must be
/// refused HERE, naming the field. Left unchecked it does not fail, which is worse: `days_from_civil`
/// happily rolls month 13 into the next January and day 40 into the following month, so the calendar
/// silently marks the wrong dates as holidays and every business-day answer built on it is quietly wrong.
pub(crate) fn rule_from(id: &str, dto: RuleDto) -> Result<chrono_core::calendar::HolidayRule, String> {
    use chrono_core::calendar::HolidayRule;
    Ok(match dto {
        RuleDto::Fixed { month, day } => {
            check_month(id, month)?;
            // 1..=31 for the day, not the month's real length: a rule may legitimately name Feb 29,
            // and the engine resolves an impossible date per year. Beyond 31 is a typo in any month.
            if !(1..=31).contains(&day) {
                return Err(format!("holiday '{id}': day {day} out of range (1..=31)"));
            }

            HolidayRule::Fixed { month, day }
        }
        RuleDto::NthWeekday { month, weekday, order } => {
            check_month(id, month)?;
            // -1 = "the last such weekday in the month"; 1..=5 counts from the start. A fifth exists
            // only in some months, and the engine now answers "this holiday does not fall in that
            // year" rather than borrowing a day from the next month - which is what it actually did
            // while this comment claimed otherwise (R2-N4). Anything outside the range would walk
            // past the end for every month, so it stays a load error.
            if order != -1 && !(1..=5).contains(&order) {
                return Err(format!(
                    "holiday '{id}': order {order} out of range (-1 for last, or 1..=5)"
                ));
            }

            HolidayRule::NthWeekday { month, weekday: weekday_index(&weekday)?, order }
        }
        RuleDto::EasterOffset { offset } => {
            // A year either side of Easter covers every real observance (Corpus Christi is +60).
            if !(-366..=366).contains(&offset) {
                return Err(format!(
                    "holiday '{id}': easter offset {offset} out of range (-366..=366 days)"
                ));
            }

            HolidayRule::EasterOffset { offset }
        }
    })
}

pub(crate) fn check_month(id: &str, month: u32) -> Result<(), String> {
    if !(1..=12).contains(&month) {
        return Err(format!("holiday '{id}': month {month} out of range (1..=12)"));
    }

    Ok(())
}

/// Locate a calendar file: next to the executable (portable layout), else in ./calendars.
/// Whether a catalogue id (calendar / preset) is safe to turn into a file name: non-empty and made
/// only of letters, digits, '-' and '_'. This rejects any path separator, '..', drive letter, or
/// ADS colon BEFORE the id becomes a path, so `--preset ../../secret` cannot read a file outside the
/// catalogue directory (docs/04 4.1 - a shared catalogue entry can never smuggle a path).
pub(crate) fn is_valid_catalogue_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Locate `<kind>/<id>.json` - the shared lookup behind calendars and presets.
///
/// Next to the executable first, then the working directory. The second is what makes a dev
/// checkout work: `chrono.exe` is built into `target/<triple>/release/`, which has no `calendars/`
/// beside it, so every `cargo run -- calc --calendar` and all 133 harness scenarios resolve through
/// the working directory. It cannot simply be dropped.
///
/// What it must NOT do is rescue an INSTALLED layout. If the folder beside the executable exists but
/// does not hold the file, the answer is "missing", not "here is one from wherever you happened to
/// be standing" - otherwise `chrono calc --calendar us-banking`, run from a directory someone else
/// can write to, silently answers business-day questions from THEIR holidays, in a report a tester
/// then quotes as evidence (untouchable rule 4). Directories are taken as given so all three cases
/// are testable without touching the process's real working directory.
pub(crate) fn find_catalogue_in(exe_dir: Option<&std::path::Path>, cwd: &std::path::Path, kind: &str, id: &str) -> Option<std::path::PathBuf> {
    let name = format!("{id}.json");
    if let Some(dir) = exe_dir {
        let installed = dir.join(kind);
        let candidate = installed.join(&name);
        if candidate.is_file() {
            return Some(candidate);
        }
        if installed.is_dir() {
            return None; // an installed catalogue answers for itself, including "not here"
        }
    }
    let local = cwd.join(kind).join(&name);
    local.is_file().then_some(local)
}

/// [`find_catalogue_in`] against this process's real executable and working directory.
pub(crate) fn find_catalogue_file(kind: &str, id: &str) -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok();
    let exe_dir = exe.as_ref().and_then(|e| e.parent());
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    find_catalogue_in(exe_dir, &cwd, kind, id)
}

/// Where the lookup actually looked, for the "not found" message. The two layouts differ, and naming
/// `./<kind>` for an installed one that never consulted it would send the reader to fix the wrong
/// folder - in the single message they have to act on (rule 6).
pub(crate) fn catalogue_search_places(kind: &str) -> String {
    let installed = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join(kind)))
        .is_some_and(|d| d.is_dir());
    if installed {
        format!("looked in <exe>/{kind}")
    } else {
        format!("looked in <exe>/{kind} and ./{kind}")
    }
}

pub(crate) fn find_calendar_file(id: &str) -> Result<std::path::PathBuf, String> {
    if !is_valid_catalogue_id(id) {
        return Err(format!("invalid calendar id '{id}' (use letters, digits, '-' or '_')"));
    }
    find_catalogue_file("calendars", id)
        .ok_or_else(|| format!("calendar '{id}' not found ({})", catalogue_search_places("calendars")))
}

/// Load and validate a calendar by id, mapping the JSON schema to the core engine's types.
pub(crate) fn load_calendar(id: &str) -> Result<chrono_core::calendar::Calendar, String> {
    let path = find_calendar_file(id)?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    calendar_from_text(&text).map_err(|e| format!("{e} (in {})", path.display()))
}

/// Parse and validate a calendar from its JSON text, mapping the `chronomock.calendar/1` schema to the
/// engine's types. Separated from the on-disk lookup (symmetry with `parse_preset`) so the shipped
/// calendars can be golden-tested against the real engine without the file-resolution step.
pub(crate) fn calendar_from_text(text: &str) -> Result<chrono_core::calendar::Calendar, String> {
    let dto: CalendarDto = serde_json::from_str(text).map_err(|e| format!("bad calendar JSON: {e}"))?;
    // An unknown major schema version is refused, not half-understood (docs/04 section 3.1).
    if dto.schema != "chronomock.calendar/1" {
        return Err(format!(
            "unsupported calendar schema '{}' (this build reads chronomock.calendar/1)",
            dto.schema
        ));
    }
    let mut weekend = dto.weekend.iter().map(|w| weekday_index(w)).collect::<Result<Vec<_>, _>>()?;
    // Duplicates are harmless to the engine (membership is a contains) but they hide a typo, and
    // they make the count below meaningless - so fold them away before counting.
    weekend.sort_unstable();
    weekend.dedup();
    // A week with no working day leaves "+1 business day" with nothing to land on. The engine now
    // bounds its walk instead of hanging (S-1), but a file this broken should never reach it: say
    // which field is wrong, here, where the author can fix it.
    if weekend.len() >= 7 {
        return Err(
            "calendar 'weekend' lists all seven days - no business day would ever exist".to_string()
        );
    }
    let mut seen_ids: Vec<String> = Vec::new();
    let holidays = dto
        .holidays
        .into_iter()
        .map(|h| {
            // A duplicate id makes the audit ambiguous - `holiday_on` names one of them and the reader
            // cannot tell which. Cheap to catch, impossible to diagnose later.
            if seen_ids.contains(&h.id) {
                return Err(format!("duplicate holiday id '{}'", h.id));
            }

            // An inverted window silently means "never a holiday", which reads as a missing entry
            // rather than as the mistake it is.
            if let (Some(from), Some(to)) = (h.valid_from, h.valid_to)
                && from > to {
                    return Err(format!(
                        "holiday '{}': valid_from {from} is after valid_to {to}",
                        h.id
                    ));
                }

            seen_ids.push(h.id.clone());
            let rule = rule_from(&h.id, h.rule)?;
            Ok(chrono_core::calendar::Holiday {
                id: h.id,
                name_en: h.name.en,
                name_local: h.name.local,
                rule,
                valid_from: h.valid_from,
                valid_to: h.valid_to,
                source: h.source,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(chrono_core::calendar::Calendar {
        id: dto.id,
        country: dto.country,
        weekend,
        observed: observed_from(&dto.observed)?,
        holidays,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::read_data;

    /// S-1, first line. Calendars are the one catalogue outsiders are invited to write, so a file
    /// that makes "next business day" unanswerable has to be refused where its author can see it -
    /// not walked into by the engine. Duplicated weekend days are folded first, so the check counts
    /// distinct days and a repeated "saturday" is not mistaken for a full week.
    #[test]
    fn calendar_with_every_day_as_weekend_is_refused() {
        let all_week = r#"{"schema":"chronomock.calendar/1","id":"x","country":"XX",
            "weekend":["monday","tuesday","wednesday","thursday","friday","saturday","sunday"],
            "observed":"none","holidays":[]}"#;
        let err = calendar_from_text(all_week).expect_err("a week with no working day is refused");
        assert!(err.contains("weekend"), "the message must name the field: {err}");

        // Duplicates alone are not an error - they fold, and the calendar still works.
        let dupes = r#"{"schema":"chronomock.calendar/1","id":"x","country":"XX",
            "weekend":["saturday","saturday","sunday"],"observed":"none","holidays":[]}"#;
        let cal = calendar_from_text(dupes).expect("duplicate weekend days fold");
        assert_eq!(cal.weekend.len(), 2);
    }

    /// S-17. Every rule field is refused out of range, naming the holiday and the field. Unchecked,
    /// none of these fail loudly - the civil-date maths rolls month 13 into January and day 40 into the
    /// next month, so the calendar quietly marks the WRONG dates and every business-day answer built on
    /// it is wrong with it. Calendars are the documented third-party extension point, so this is the
    /// place where a broken file has to stop.
    #[test]
    fn calendar_rules_out_of_range_are_refused_with_the_field_named() {
        let cal = |rule: &str| {
            format!(
                r#"{{"schema":"chronomock.calendar/1","id":"x","country":"XX","weekend":["saturday","sunday"],
                "observed":"none","holidays":[{{"id":"bad","name":{{"en":"B","local":"B"}},
                "rule":{rule},"source":"test"}}]}}"#
            )
        };

        for (rule, needle) in [
            (r#"{"type":"fixed","month":13,"day":1}"#, "month 13"),
            (r#"{"type":"fixed","month":1,"day":40}"#, "day 40"),
            (r#"{"type":"nth_weekday","month":1,"weekday":"monday","order":9}"#, "order 9"),
            (r#"{"type":"nth_weekday","month":0,"weekday":"monday","order":1}"#, "month 0"),
            (r#"{"type":"easter_offset","offset":5000}"#, "5000"),
        ] {
            let err = calendar_from_text(&cal(rule)).expect_err("out of range must be refused");
            assert!(err.contains("bad"), "the message must name the holiday: {err}");
            assert!(err.contains(needle), "the message must name the value: {err}");
        }

        // The legitimate neighbours of those bounds still parse.
        for rule in [
            r#"{"type":"fixed","month":2,"day":29}"#,
            r#"{"type":"nth_weekday","month":12,"weekday":"monday","order":-1}"#,
            r#"{"type":"nth_weekday","month":5,"weekday":"monday","order":5}"#,
            r#"{"type":"easter_offset","offset":60}"#,
        ] {
            calendar_from_text(&cal(rule)).unwrap_or_else(|e| panic!("{rule} should parse: {e}"));
        }
    }

    /// S-17, the whole-file checks: a repeated id makes the audit ambiguous (holiday_on names one of
    /// them and the reader cannot tell which), and an inverted validity window means "never a holiday",
    /// which reads as a missing entry instead of the mistake it is.
    #[test]
    fn duplicate_holiday_ids_and_inverted_validity_windows_are_refused() {
        let dupes = r#"{"schema":"chronomock.calendar/1","id":"x","country":"XX",
            "weekend":["saturday","sunday"],"observed":"none","holidays":[
            {"id":"same","name":{"en":"A","local":"A"},"rule":{"type":"fixed","month":1,"day":1},"source":"t"},
            {"id":"same","name":{"en":"B","local":"B"},"rule":{"type":"fixed","month":2,"day":2},"source":"t"}]}"#;
        assert!(calendar_from_text(dupes).expect_err("duplicate id").contains("same"));

        let inverted = r#"{"schema":"chronomock.calendar/1","id":"x","country":"XX",
            "weekend":["saturday","sunday"],"observed":"none","holidays":[
            {"id":"w","name":{"en":"A","local":"A"},"rule":{"type":"fixed","month":1,"day":1},
             "valid_from":2030,"valid_to":2020,"source":"t"}]}"#;
        assert!(calendar_from_text(inverted).expect_err("inverted window").contains("valid_from"));
    }

    /// An installed layout answers for its own catalogue, "not here" included. Without that, running
    /// `chrono calc --calendar us-banking` from a directory someone else can write to answered
    /// business-day questions out of THEIR file whenever the shipped one was missing - different
    /// holidays, different answers, no warning, in output a tester quotes as evidence.
    ///
    /// The working-directory fallback itself has to stay: `chrono.exe` is built into
    /// `target/<triple>/release/`, which has no `calendars/` beside it, so a dev checkout and all
    /// 133 harness scenarios resolve that way.
    #[test]
    fn an_installed_catalogue_is_not_rescued_from_the_working_directory() {
        let root = std::env::temp_dir().join(format!("chrono-catalogue-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let exe_dir = root.join("app");
        let cwd = root.join("elsewhere");
        std::fs::create_dir_all(exe_dir.join("calendars")).unwrap();
        std::fs::create_dir_all(cwd.join("calendars")).unwrap();
        std::fs::write(cwd.join("calendars").join("us-banking.json"), "{}").unwrap();

        // Installed layout, file absent from it: the one in the working directory is NOT used.
        assert_eq!(find_catalogue_in(Some(&exe_dir), &cwd, "calendars", "us-banking"), None);

        // Installed layout that does have it: that copy wins.
        let shipped = exe_dir.join("calendars").join("us-banking.json");
        std::fs::write(&shipped, "{}").unwrap();
        assert_eq!(find_catalogue_in(Some(&exe_dir), &cwd, "calendars", "us-banking"), Some(shipped));

        // No catalogue beside the executable at all - a dev checkout. The fallback still works, and
        // this is the case the harness runs in.
        let bare = root.join("bare");
        std::fs::create_dir_all(&bare).unwrap();
        assert_eq!(
            find_catalogue_in(Some(&bare), &cwd, "calendars", "us-banking"),
            Some(cwd.join("calendars").join("us-banking.json"))
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn shipped_calendars_parse_and_hit_golden_dates() {
        use chrono_core::calc::CivilDateTime;
        use chrono_core::calendar::{holiday_on, is_business_day};
        let d = |y: i64, m: u32, day: u32| CivilDateTime {
            year: y,
            month: m,
            day,
            hour: 0,
            minute: 0,
            second: 0,
        };

        let pl = calendar_from_text(&read_data("calendars/pl.json")).expect("pl parses");
        // Easter Monday 2026 = 2026-04-06 (Easter Sunday 2026-04-05): the easter_offset computus.
        assert_eq!(holiday_on(&d(2026, 4, 6), &pl).map(|h| h.id.as_str()), Some("easter_monday"));
        // Corpus Christi 2026 = Easter + 60 days = 2026-06-04: a larger easter_offset.
        assert_eq!(holiday_on(&d(2026, 6, 4), &pl).map(|h| h.id.as_str()), Some("corpus_christi"));
        // Epiphany was restored as a Polish public holiday from 2011 (valid_from:2011; law of 24 Sept
        // 2010, abolished 1960): a holiday in 2026, NOT in 2010.
        assert_eq!(holiday_on(&d(2026, 1, 6), &pl).map(|h| h.id.as_str()), Some("epiphany"));
        assert!(holiday_on(&d(2010, 1, 6), &pl).is_none());
        // Christmas Eve became a non-working day from 2025 (valid_from:2025): a holiday in 2025, not 2024.
        assert_eq!(holiday_on(&d(2025, 12, 24), &pl).map(|h| h.id.as_str()), Some("christmas_eve"));
        assert!(holiday_on(&d(2024, 12, 24), &pl).is_none());

        let fed = calendar_from_text(&read_data("calendars/us-federal.json")).expect("us-federal parses");
        let bank = calendar_from_text(&read_data("calendars/us-banking.json")).expect("us-banking parses");
        // One country, two calendars: Independence Day 2026 falls on Saturday (Jul 4), so it is observed
        // on Friday Jul 3 federally (sat->fri) - not a business day - while banking (sun->mon only) does
        // not shift a Saturday holiday, so the same Friday IS a business day.
        assert!(!is_business_day(&d(2026, 7, 3), &fed));
        assert!(is_business_day(&d(2026, 7, 3), &bank));
        // Juneteenth is a federal holiday from 2021 (valid_from:2021): a holiday in 2021, not in 2020.
        assert_eq!(holiday_on(&d(2021, 6, 19), &fed).map(|h| h.id.as_str()), Some("juneteenth"));
        assert!(holiday_on(&d(2020, 6, 19), &fed).is_none());
        // MLK Day 2026 = the third Monday of January = 2026-01-19: an nth_weekday rule.
        assert_eq!(holiday_on(&d(2026, 1, 19), &fed).map(|h| h.id.as_str()), Some("mlk_day"));
        // And it is a holiday from 1986 (valid_from:1986). Public Law 98-144 was signed in 1983 with
        // effect from the first 1 January falling after a two-year period, so the third Monday of
        // January 1986 (the 20th) is the first observance and the third Monday of January 1985 (the
        // 21st) is an ordinary working day. Both calendars carry it, so both are asserted - the
        // entry read "always" until 2026-09-07, which made every date back to year 1 wrong.
        assert_eq!(holiday_on(&d(1986, 1, 20), &fed).map(|h| h.id.as_str()), Some("mlk_day"));
        assert!(holiday_on(&d(1985, 1, 21), &fed).is_none());
        assert_eq!(holiday_on(&d(1986, 1, 20), &bank).map(|h| h.id.as_str()), Some("mlk_day"));
        assert!(holiday_on(&d(1985, 1, 21), &bank).is_none());
        // Banking observes a Sunday holiday on the Monday: New Year 2023-01-01 (Sun) -> Mon 2023-01-02.
        assert!(!is_business_day(&d(2023, 1, 2), &bank));
    }

    #[test]
    fn catalogue_id_rejects_path_traversal() {
        // A catalogue id must never become a path escape: separators, '..', a drive colon, or empty
        // are refused before the id is turned into a file name (docs/04 4.1).
        assert!(is_valid_catalogue_id("month-end"));
        assert!(is_valid_catalogue_id("us_banking"));
        assert!(is_valid_catalogue_id("year-2038"));
        assert!(!is_valid_catalogue_id(""));
        assert!(!is_valid_catalogue_id(".."));
        assert!(!is_valid_catalogue_id("../secret"));
        assert!(!is_valid_catalogue_id("a/b"));
        assert!(!is_valid_catalogue_id("a\\b"));
        assert!(!is_valid_catalogue_id("c:evil"));
    }

    #[test]
    fn calendar_loader_maps_weekdays_and_observed() {
        assert_eq!(weekday_index("Monday").unwrap(), 1);
        assert_eq!(weekday_index("sunday").unwrap(), 0);
        assert!(weekday_index("funday").is_err());
        assert!(matches!(observed_from("sun_to_mon").unwrap(), chrono_core::calendar::Observed::SunToMon));
        assert!(observed_from("whenever").is_err());
    }
}
