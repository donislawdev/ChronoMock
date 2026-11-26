//! The step grammar: one parser for `--shift`, `--snap`, `--nearest`, `--set-time` and `--base`.
//!
//! Shared on purpose between the calculator flags and the preset reader, and the code said so long
//! before it had a file: "so there is one grammar, not two". Keeping it here is what makes that
//! sentence structural instead of a promise - the two callers cannot drift apart without meeting
//! here first.

use chronomock::calc::{Base, NearestTarget, Sign, SnapTarget, Step, Unit};

/// Parse a `--base` value: the keywords `today`/`now`, or an absolute civil date-time.
pub(crate) fn parse_base(raw: &str) -> Result<Base, String> {
    match raw {
        "today" => Ok(Base::Today),
        "now" => Ok(Base::Now),
        _ => Ok(Base::Absolute(chronomock::calc::parse_civil_datetime(raw)?)),
    }
}

/// Parse a `--shift` value `±N<unit>` into a shift step. The sign is mandatory - the
/// unit accepts short codes and full names. Minute stays `m` - month is `mo`, never `m`.
pub(crate) fn parse_shift(raw: &str) -> Result<Step, String> {
    let sign = match raw.as_bytes().first() {
        Some(b'+') => Sign::Plus,
        Some(b'-') => Sign::Minus,
        _ => return Err(format!("shift must start with + or -, got '{raw}'")),
    };
    let rest = &raw[1..];
    let split = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    let (num, unit_str) = rest.split_at(split);
    if num.is_empty() {
        return Err(format!("shift needs a number, got '{raw}'"));
    }
    let amount: i64 = num.parse().map_err(|_| format!("bad number in shift '{raw}'"))?;
    let unit = parse_unit(unit_str).ok_or_else(|| format!("{} in shift '{raw}'", unknown_unit(unit_str)))?;
    Ok(Step::Shift { sign, amount, unit })
}

/// Every unit, in the order a refusal lists them.
const UNITS: [Unit; 9] = [
    Unit::Seconds,
    Unit::Minutes,
    Unit::Hours,
    Unit::Days,
    Unit::Weeks,
    Unit::Months,
    Unit::Quarters,
    Unit::Years,
    Unit::BusinessDays,
];

/// The refusal of a unit this grammar does not read, with the units it does (R5-D5, item 3). `snap`
/// and `nearest` already listed their targets, and `--shift +2mth` left the reader to guess. The list
/// comes from [`unit_code`], so a unit added there is listed here too.
pub(crate) fn unknown_unit(token: &str) -> String {
    let codes: Vec<&str> = UNITS.iter().map(|&u| unit_code(u)).collect();
    format!("unknown unit '{token}' (use {} - m is minutes, mo is months)", codes.join(", "))
}

/// Map a unit token (short code or full name) to a canonical unit. 🔴 `m` is minutes,
/// `mo` is months - never conflate them (the substitution `--at` delta already uses `m`
/// for minutes, so calc keeps the same convention).
pub(crate) fn parse_unit(s: &str) -> Option<Unit> {
    Some(match s {
        "s" | "sec" | "secs" | "seconds" => Unit::Seconds,
        "m" | "min" | "mins" | "minutes" => Unit::Minutes,
        "h" | "hr" | "hrs" | "hours" => Unit::Hours,
        "d" | "day" | "days" => Unit::Days,
        "w" | "week" | "weeks" => Unit::Weeks,
        "mo" | "month" | "months" => Unit::Months,
        "q" | "quarter" | "quarters" => Unit::Quarters,
        "y" | "yr" | "yrs" | "year" | "years" => Unit::Years,
        "bd" | "business_days" | "businessdays" => Unit::BusinessDays,
        _ => return None,
    })
}

/// The sign a shift is written with - the inverse of the first character [`parse_shift`] reads.
pub(crate) fn sign_code(sign: Sign) -> &'static str {
    match sign {
        Sign::Plus => "+",
        Sign::Minus => "-",
    }
}

/// The short code a unit is written with in this grammar - the inverse of [`parse_unit`], and the
/// token the window's step builder offers. The preset catalogue writes every unit this way, so the
/// window never has to know that `month`, `months` and `mo` are one unit (R4-S22). No catch-all arm:
/// a new unit cannot be added without its code.
pub(crate) fn unit_code(unit: Unit) -> &'static str {
    match unit {
        Unit::Seconds => "s",
        Unit::Minutes => "m",
        Unit::Hours => "h",
        Unit::Days => "d",
        Unit::Weeks => "w",
        Unit::Months => "mo",
        Unit::Quarters => "q",
        Unit::Years => "y",
        Unit::BusinessDays => "bd",
    }
}

/// The code a snap target is written with - the inverse of [`parse_snap`], for the same reason as
/// [`unit_code`].
pub(crate) fn snap_code(target: SnapTarget) -> &'static str {
    match target {
        SnapTarget::StartOfMonth => "som",
        SnapTarget::EndOfMonth => "eom",
        SnapTarget::StartOfQuarter => "soq",
        SnapTarget::EndOfQuarter => "eoq",
        SnapTarget::StartOfYear => "soy",
        SnapTarget::EndOfYear => "eoy",
    }
}

/// The code a nearest target is written with - the inverse of [`parse_nearest`]. The leap day keeps
/// its long form, which is the one the window's builder has always sent.
pub(crate) fn nearest_code(target: NearestTarget) -> &'static str {
    match target {
        NearestTarget::NextBusinessDay => "nbd",
        NearestTarget::PrevBusinessDay => "pbd",
        NearestTarget::NextLeapDay => "next-leap-day",
    }
}

/// Parse a `--snap` target token (full or short form) into a typed target.
pub(crate) fn parse_snap(raw: &str) -> Result<SnapTarget, String> {
    Ok(match raw {
        "start-of-month" | "som" => SnapTarget::StartOfMonth,
        "end-of-month" | "eom" => SnapTarget::EndOfMonth,
        "start-of-quarter" | "soq" => SnapTarget::StartOfQuarter,
        "end-of-quarter" | "eoq" => SnapTarget::EndOfQuarter,
        "start-of-year" | "soy" => SnapTarget::StartOfYear,
        "end-of-year" | "eoy" => SnapTarget::EndOfYear,
        other => {
            return Err(format!("unknown snap target '{other}' (use start-of/end-of month|quarter|year)"))
        }
    })
}

/// Parse a `--nearest` target token into a typed target.
pub(crate) fn parse_nearest(raw: &str) -> Result<NearestTarget, String> {
    Ok(match raw {
        "next-business-day" | "nbd" => NearestTarget::NextBusinessDay,
        "prev-business-day" | "previous-business-day" | "pbd" => NearestTarget::PrevBusinessDay,
        "next-leap-day" | "next-feb-29" | "nld" => NearestTarget::NextLeapDay,
        other => {
            return Err(format!(
                "unknown nearest target '{other}' (next-business-day, prev-business-day, next-leap-day)"
            ))
        }
    })
}

/// Parse a `--set-time` value `HH:MM:SS`. Field ranges are validated in the core
/// evaluator (BadSetTime), so parsing only checks the shape and numeric form here.
pub(crate) fn parse_set_time(raw: &str) -> Result<Step, String> {
    let p: Vec<&str> = raw.split(':').collect();
    if p.len() != 3 {
        return Err(format!("set-time must be HH:MM:SS, got '{raw}'"));
    }
    // Digits only, for the reason `chronomock::is_ascii_digits` gives: the parse alone took `+23` as
    // an hour (R4-N34).
    if !p.iter().all(|f| chronomock::is_ascii_digits(f)) {
        return Err(format!("set-time must be HH:MM:SS in digits, got '{raw}'"));
    }
    let hour = p[0].parse().map_err(|_| format!("bad hour in set-time '{raw}'"))?;
    let minute = p[1].parse().map_err(|_| format!("bad minute in set-time '{raw}'"))?;
    let second = p[2].parse().map_err(|_| format!("bad second in set-time '{raw}'"))?;
    Ok(Step::SetTime { hour, minute, second })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_shift_reads_sign_amount_and_unit() {
        assert_eq!(parse_shift("+18years").unwrap(), Step::Shift { sign: Sign::Plus, amount: 18, unit: Unit::Years });
        assert_eq!(parse_shift("-1d").unwrap(), Step::Shift { sign: Sign::Minus, amount: 1, unit: Unit::Days });
        assert_eq!(parse_shift("+2q").unwrap(), Step::Shift { sign: Sign::Plus, amount: 2, unit: Unit::Quarters });
    }

    #[test]
    fn shift_unit_m_is_minutes_mo_is_months() {
        // The collision that would silently corrupt every month calc if conflated.
        assert_eq!(parse_shift("+5m").unwrap(), Step::Shift { sign: Sign::Plus, amount: 5, unit: Unit::Minutes });
        assert_eq!(parse_shift("+5mo").unwrap(), Step::Shift { sign: Sign::Plus, amount: 5, unit: Unit::Months });
    }

    #[test]
    fn parse_shift_rejects_bad_shapes() {
        assert!(parse_shift("18years").is_err()); // no sign
        assert!(parse_shift("+years").is_err()); // no number
        assert!(parse_shift("+18zz").is_err()); // unknown unit
        // The refusal lists the units it reads (R5-D5, item 3), all of them, from the one table.
        let err = parse_shift("+2mth").unwrap_err();
        assert!(err.contains("unknown unit 'mth'") && err.contains("use s, m, h, d, w, mo, q, y, bd"), "{err}");
        assert!(parse_shift("+").is_err()); // sign only
    }

    #[test]
    fn parse_base_reads_keywords_and_absolute() {
        assert_eq!(parse_base("today").unwrap(), Base::Today);
        assert_eq!(parse_base("now").unwrap(), Base::Now);
        assert!(matches!(parse_base("2025-01-31T12:00:00").unwrap(), Base::Absolute(_)));
        assert!(parse_base("2025-02-31T00:00:00").is_err()); // impossible day rejected, not normalized
    }

    #[test]
    fn parse_set_time_reads_hms() {
        assert_eq!(parse_set_time("23:59:59").unwrap(), Step::SetTime { hour: 23, minute: 59, second: 59 });
        assert!(parse_set_time("23:59").is_err()); // wrong shape
        assert!(parse_set_time("aa:bb:cc").is_err()); // non-numeric
        assert!(parse_set_time("+23:59:59").is_err()); // a sign the integer parse used to accept (R4-N34)
        assert!(parse_set_time("23:+5:00").is_err());
    }

    #[test]
    fn parse_snap_reads_targets_and_rejects_unknown() {
        assert_eq!(parse_snap("end-of-quarter").unwrap(), SnapTarget::EndOfQuarter);
        assert_eq!(parse_snap("eom").unwrap(), SnapTarget::EndOfMonth);
        assert_eq!(parse_snap("start-of-year").unwrap(), SnapTarget::StartOfYear);
        assert!(parse_snap("end-of-week").is_err());
    }

    /// Every code reads back as the value it was written from. The catalogue hands the window these
    /// codes and the window sends them straight back as flags, so a code the parser does not read
    /// would turn a listed preset into a refused calculation.
    #[test]
    fn every_code_reads_back_as_the_value_it_names() {
        use Unit::*;
        for unit in [Seconds, Minutes, Hours, Days, Weeks, Months, Quarters, Years, BusinessDays] {
            assert_eq!(parse_unit(unit_code(unit)), Some(unit), "{unit:?}");
        }
        use SnapTarget::*;
        for target in [StartOfMonth, EndOfMonth, StartOfQuarter, EndOfQuarter, StartOfYear, EndOfYear] {
            assert_eq!(parse_snap(snap_code(target)), Ok(target), "{target:?}");
        }
        use NearestTarget::*;
        for target in [NextBusinessDay, PrevBusinessDay, NextLeapDay] {
            assert_eq!(parse_nearest(nearest_code(target)), Ok(target), "{target:?}");
        }
    }

    #[test]
    fn parse_nearest_reads_targets_and_rejects_unknown() {
        assert_eq!(parse_nearest("next-business-day").unwrap(), NearestTarget::NextBusinessDay);
        assert_eq!(parse_nearest("pbd").unwrap(), NearestTarget::PrevBusinessDay);
        assert!(parse_nearest("next-full-moon").is_err());
    }

    mod properties {
        use super::*;
        use proptest::prelude::*;

        /// Every word the grammar reads for a unit, written out here rather than read from
        /// `parse_unit`, so a word dropped from the parser - or one added to it without a test that
        /// says so - is a red case. The words are a contract: presets and the window send them.
        const UNIT_WORDS: [(&str, Unit); 32] = [
            ("s", Unit::Seconds), ("sec", Unit::Seconds), ("secs", Unit::Seconds), ("seconds", Unit::Seconds),
            ("m", Unit::Minutes), ("min", Unit::Minutes), ("mins", Unit::Minutes), ("minutes", Unit::Minutes),
            ("h", Unit::Hours), ("hr", Unit::Hours), ("hrs", Unit::Hours), ("hours", Unit::Hours),
            ("d", Unit::Days), ("day", Unit::Days), ("days", Unit::Days),
            ("w", Unit::Weeks), ("week", Unit::Weeks), ("weeks", Unit::Weeks),
            ("mo", Unit::Months), ("month", Unit::Months), ("months", Unit::Months),
            ("q", Unit::Quarters), ("quarter", Unit::Quarters), ("quarters", Unit::Quarters),
            ("y", Unit::Years), ("yr", Unit::Years), ("yrs", Unit::Years), ("year", Unit::Years), ("years", Unit::Years),
            ("bd", Unit::BusinessDays), ("business_days", Unit::BusinessDays), ("businessdays", Unit::BusinessDays),
        ];

        const SNAP_WORDS: [(&str, SnapTarget); 12] = [
            ("start-of-month", SnapTarget::StartOfMonth), ("som", SnapTarget::StartOfMonth),
            ("end-of-month", SnapTarget::EndOfMonth), ("eom", SnapTarget::EndOfMonth),
            ("start-of-quarter", SnapTarget::StartOfQuarter), ("soq", SnapTarget::StartOfQuarter),
            ("end-of-quarter", SnapTarget::EndOfQuarter), ("eoq", SnapTarget::EndOfQuarter),
            ("start-of-year", SnapTarget::StartOfYear), ("soy", SnapTarget::StartOfYear),
            ("end-of-year", SnapTarget::EndOfYear), ("eoy", SnapTarget::EndOfYear),
        ];

        const NEAREST_WORDS: [(&str, NearestTarget); 8] = [
            ("next-business-day", NearestTarget::NextBusinessDay), ("nbd", NearestTarget::NextBusinessDay),
            ("prev-business-day", NearestTarget::PrevBusinessDay),
            ("previous-business-day", NearestTarget::PrevBusinessDay), ("pbd", NearestTarget::PrevBusinessDay),
            ("next-leap-day", NearestTarget::NextLeapDay), ("next-feb-29", NearestTarget::NextLeapDay),
            ("nld", NearestTarget::NextLeapDay),
        ];

        /// The refusal of an unknown unit lists every code (R5-D5, item 3).
        const EVERY_CODE: &str = "use s, m, h, d, w, mo, q, y, bd";

        /// Digits in another script, which `char::is_numeric` takes and this grammar does not.
        fn other_script(digits: &str) -> String {
            digits.bytes().filter_map(|b| char::from_u32(0x660 + u32::from(b - b'0'))).collect()
        }

        /// A number as it is typed, and the value it says when it is ASCII digits and fits `T`: any
        /// small one, zero-padded, the largest that fits and one past it, empty, behind a space or a
        /// plus sign, and in another script.
        fn number(largest: u64) -> impl Strategy<Value = (String, Option<u64>)> {
            prop_oneof![
                6 => (0u64..=1000).prop_map(|n| (n.to_string(), Some(n))),
                1 => (0u64..=99).prop_map(|n| (format!("{n:04}"), Some(n))),
                1 => Just((largest.to_string(), Some(largest))),
                1 => Just(((u128::from(largest) + 1).to_string(), None)),
                1 => prop_oneof![Just(String::new()), Just(" 5".to_string()), Just("+5".to_string())].prop_map(|t| (t, None)),
                1 => (0u64..=99).prop_map(|n| (other_script(&n.to_string()), None)),
            ]
        }

        /// A word that is a table's word, or near one: another case, a letter more or less, a blank
        /// beside it, nothing at all. Never starting with a digit, which would join the number before it.
        fn word<T: Copy + std::fmt::Debug + 'static>(table: &'static [(&'static str, T)]) -> impl Strategy<Value = String> {
            let known = prop::sample::select(table).prop_map(|(w, _)| w.to_string());
            let near = (prop::sample::select(table), 0u8..5).prop_map(|((w, _), how)| match how {
                0 => w.to_uppercase(),
                1 => format!("{w}s"),
                2 => w[..w.len() - 1].to_string(),
                3 => format!("{w} "),
                _ => format!(" {w}"),
            });
            prop_oneof![6 => known, 2 => near, 1 => Just(String::new()), 1 => Just("mth".to_string())]
        }

        fn looked_up<T: Copy>(table: &[(&str, T)], word: &str) -> Option<T> {
            table.iter().find(|(w, _)| *w == word).map(|&(_, value)| value)
        }

        proptest! {
            /// R4-N34 and R5-D5 item 3. A step value is read exactly when the grammar writes it so,
            /// and as what it says: a shift is a plus or a minus, ASCII digits that fit 64 bits and
            /// a unit word, a time of day is three fields of ASCII digits, a snap and a nearest
            /// target are words of their tables. An unknown unit is refused with every code listed.
            #[test]
            fn a_step_value_is_read_exactly_when_the_grammar_writes_it_so(
                sign in prop_oneof![4 => Just("+"), 4 => Just("-"), 1 => Just(""), 1 => Just("\u{b1}"), 1 => Just("++"), 1 => Just("+-")],
                (amount, amount_says) in number(i64::MAX as u64),
                unit in word(&UNIT_WORDS),
                time in prop::collection::vec(number(u64::from(u32::MAX)), 2..=4),
                time_joined_by in prop_oneof![6 => Just(":"), 1 => Just("."), 1 => Just(": ")],
                snap in word(&SNAP_WORDS),
                nearest in word(&NEAREST_WORDS),
            ) {
                let shift = format!("{sign}{amount}{unit}");
                // No sign in front of a number written with a plus is a plus and the number.
                let (sign, amount_says) = match (sign, amount.strip_prefix('+')) {
                    ("", Some("5")) => ("+", Some(5)),
                    _ => (sign, amount_says),
                };
                let unit_says = looked_up(&UNIT_WORDS, &unit);
                let sign_says = match sign { "+" => Some(Sign::Plus), "-" => Some(Sign::Minus), _ => None };
                let owed = match (sign_says, amount_says, unit_says) {
                    (Some(sign), Some(amount), Some(unit)) => Some(Step::Shift { sign, amount: amount as i64, unit }),
                    _ => None,
                };
                let read = parse_shift(&shift);
                prop_assert_eq!(read.as_ref().ok(), owed.as_ref(), "--shift {:?} reads as {:?}", shift, read);
                if let (Some(_), Some(_), None, Err(refusal)) = (sign_says, amount_says, unit_says, &read) {
                    prop_assert!(refusal.contains(EVERY_CODE), "--shift {:?}: {}", shift, refusal);
                }

                let set_time = time.iter().map(|(text, _)| text.as_str()).collect::<Vec<_>>().join(time_joined_by);
                let owed = match (time_joined_by, time.as_slice()) {
                    (":", [(_, Some(hour)), (_, Some(minute)), (_, Some(second))]) => {
                        Some(Step::SetTime { hour: *hour as u32, minute: *minute as u32, second: *second as u32 })
                    }
                    _ => None,
                };
                let read = parse_set_time(&set_time);
                prop_assert_eq!(read.as_ref().ok(), owed.as_ref(), "--set-time {:?} reads as {:?}", set_time, read);

                prop_assert_eq!(parse_snap(&snap).ok(), looked_up(&SNAP_WORDS, &snap), "--snap {:?}", snap);
                prop_assert_eq!(parse_nearest(&nearest).ok(), looked_up(&NEAREST_WORDS, &nearest), "--nearest {:?}", nearest);
            }
        }
    }
}
