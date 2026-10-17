//! Chrono Mock logical core - pure time-substitution logic.
//!
//! Rule 16 (untouchable): the core knows nothing of CLI, GUI, I/O, or language.
//! It takes plain data in and returns plain values out - no side effects
//! (zasady/06 section 7). Everything here is unit-testable without a target
//! process or an operating system.

/// Date-arithmetic engine: the canonical step model shared by the calculator and
/// (later) the substitution moment grammar. A child module, so it may reuse the
/// crate-private civil-date primitives below without widening their visibility.
pub mod calc;

/// Business-day and holiday calendar engine (pure, data-driven). Like `calc`, a child
/// module reusing the crate-private civil primitives.
pub mod calendar;

/// Which characters do not show as themselves - judged once for the readers of calendars and presets
/// and for quoting what a target says (R5-B1).
pub mod text;

/// What the random properties share - edges, a drawn calendar, a deadline, the weekly canary.
#[cfg(test)]
mod search;

/// How the session expresses the target moment.
///
/// Semantics (docs/01 section 4, untouchable rule 2): the entered moment is in
/// the SESSION zone, everything is UTC internally, and the zone is always explicit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moment {
    /// Local wall-clock text in the session zone, e.g. "2038-01-19T03:14:07".
    pub local: String,
    /// Session zone bias in minutes (UTC = local + bias), no DST.
    ///
    /// A session never runs without one: [`session_instant`] refuses `None` (untouchable rule 2, the
    /// zone is always explicit). This used to promise "`None` = host zone" while every consumer
    /// computed UTC instead, so a client that left the field out got a clock hours away from the one
    /// the documentation described. The plain conversion [`moment_to_filetime_utc`] still reads `None`
    /// as UTC, for the calculator paths that state their zone some other way.
    pub tz_bias_min: Option<i32>,
}

/// How time flows once the moment is set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimeMode {
    /// Time flows from the moment at real speed.
    Flow,
    /// Time is frozen at the moment.
    Frozen,
    /// Time is accelerated (or slowed) by an integer multiplier.
    Multiplier(i64),
}

/// The largest accepted time multiplier.
///
/// Chosen from the measured distance between a typical session moment and the end of the FILETIME
/// range, not from taste. Starting at 2038 there are ~9.1e18 ticks of headroom, which the fake clock
/// burns through in:
///
/// | multiplier | real time until the fake clock leaves the FILETIME range |
/// |---|---|
/// | 1440 (a day per minute - the largest documented use) | ~20 years |
/// | 86_400 (a day per second) | ~122 days |
/// | **1_000_000 (this limit)** | **~10.5 days** |
/// | 10_000_000 | ~1.1 days - inside a session left over a weekend |
///
/// So this is where the headroom stops outliving any plausible session while still allowing 694x
/// more than anyone has ever asked for. Past the range the substitution does not merely saturate,
/// it comes apart: measured at x1e15, `GetSystemTimeAsFileTime` returned a wrapped nonsense instant
/// while `GetSystemTime` silently fell back to the REAL clock (its conversion rejects the instant),
/// and successive reads jumped backwards and forwards between centuries - two epochs in one process
/// and a non-monotonic clock, against untouchable rules 2 and 3, with the audit still saying `works`.
pub const MULTIPLIER_MAX: i64 = 1_000_000;

/// The smallest accepted time multiplier. Zero is the wire spelling of "freeze" that the GUI's
/// freeze button sends, so it is a value, not an error. Anything negative would run the wall clock
/// CONTINUOUSLY BACKWARD, which the time model does not have: going back is a `jump`, an event, never
/// a rate (docs/01 section 5.1, untouchable rule 3). Measured before this bound existed: a multiplier
/// of -1_000_000 accepted over the protocol had the target read 31, then 29, then 28, then 26
/// December - and the session still reported `works`.
pub const MULTIPLIER_MIN: i64 = 0;

/// Whether a multiplier is one the session may run at. The single gate both surfaces use, so the CLI
/// and the protocol cannot drift apart on what they accept (they had: the CLI required `>= 1` while
/// the protocol accepted anything at all).
pub fn multiplier_in_range(m: i64) -> bool {
    (MULTIPLIER_MIN..=MULTIPLIER_MAX).contains(&m)
}

/// The widest session zone offset accepted, in minutes either side of UTC: 14:59.
///
/// It is the band the CLI's `--zone` parser has always had (hours 0..=14, minutes 0..=59), now stated
/// once for every surface. The protocol took any `i32` and nothing checked it (R4-S9), which broke the
/// assumption the clock's range is built on: [`FAKE_WALL_MAX`] stops fifteen hours short of the end of
/// the FILETIME range precisely so that `fake - bias` still fits for every accepted zone. A bias past
/// that can make the hook's local-time channels overflow and fall back to the REAL clock while the
/// UTC channels stay fake - the failure R2-X7 measured at the top of the range.
pub const ZONE_BIAS_MAX_MIN: i32 = 14 * 60 + 59;

/// Whether a session-zone bias (minutes, UTC = local + bias) is one a session may run in. The single
/// gate the start command, the absolute jump and both mechanisms use, like [`multiplier_in_range`].
pub fn zone_bias_in_range(bias_min: i32) -> bool {
    (-ZONE_BIAS_MAX_MIN..=ZONE_BIAS_MAX_MIN).contains(&bias_min)
}

/// The last instant a session clock may name, as a UTC FILETIME (100 ns ticks since 1601).
///
/// A mirror of `chrono_ctl::FAKE_WALL_MAX`, where the reasoning lives: the last whole second that
/// leaves room for the largest zone bias, so the local-time channels can still express it. It is
/// restated here rather than imported because this crate depends on nothing (rule 16), and
/// `chrono-mech`, which sees both crates, asserts at compile time that the two are equal and that
/// [`ZONE_BIAS_MAX_MIN`] fits the headroom - so they cannot drift apart without the build failing.
pub const FAKE_WALL_MAX: i64 = {
    // Fifteen hours: more than the widest accepted zone (14:59), as `chrono_ctl::MAX_ZONE_BIAS_TICKS`.
    const ZONE_HEADROOM_TICKS: i64 = 15 * 3600 * 10_000_000;
    let raw = i64::MAX - ZONE_HEADROOM_TICKS;
    raw - (raw % 10_000_000)
};

/// Why a moment cannot become the clock of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionMomentError {
    /// The zone is missing, or outside [`ZONE_BIAS_MAX_MIN`] either side of UTC.
    BadZone,
    /// The text is not a date and time at all. Carries the parser's reason for the human half.
    NotAMoment(String),
    /// A real date the session clock cannot hold: before 1601-01-01 in UTC or in the session zone, or
    /// after [`FAKE_WALL_MAX`].
    OutOfRange,
}

impl std::fmt::Display for SessionMomentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionMomentError::BadZone => write!(
                f,
                "the session zone is missing or outside -14:59..=+14:59 (bias -{ZONE_BIAS_MAX_MIN}..={ZONE_BIAS_MAX_MIN} minutes)"
            ),
            SessionMomentError::NotAMoment(reason) => f.write_str(reason),
            SessionMomentError::OutOfRange => {
                f.write_str("the moment is outside the range a session clock can hold (years 1601 to 30828)")
            }
        }
    }
}

/// Whether a UTC instant can be the session clock in a zone: at or after 1601-01-01 both in UTC and
/// as local time in that zone, and not after [`FAKE_WALL_MAX`].
///
/// Both floors, because the hook serves the same instant twice. The UTC channels read the FILETIME,
/// the local ones read `FILETIME - bias`, and a FILETIME below zero is refused by the conversion every
/// calendar channel goes through, which then answers with the REAL clock (R4-S7d). Checking UTC alone
/// let `1600-12-31T20:00:00` at UTC-05:00 through: fine in UTC, before 1601 locally, and
/// `GetLocalTime` in the target showed today's date. The top needs no local check, because the
/// headroom under [`FAKE_WALL_MAX`] is there for exactly that.
pub fn instant_in_session_range(ft_utc: i64, bias_min: i32) -> bool {
    let local = ft_utc.checked_sub((bias_min as i64) * 60 * 10_000_000);
    (0..=FAKE_WALL_MAX).contains(&ft_utc) && local.is_some_and(|l| l >= 0)
}

/// The UTC FILETIME a session clock starts at, or is jumped to, with the zone it was read in: the one
/// gate every moment passes before it can become a session clock (R4-S7, R4-S9).
///
/// [`moment_to_filetime_utc`] answers a narrower question - does this text name an instant FILETIME
/// can hold - and it was the only check. So a moment past [`FAKE_WALL_MAX`] started a session whose
/// first rate change wrapped the clock to 1601, a zone far outside any real one reached the hook
/// unchecked, and a moment before 1601 in the session zone ran the local-time channels on the real
/// clock. This checks the zone first (a moment cannot be read without one), then the text, then the
/// instant against both ends of the session range.
pub fn session_instant(moment: &Moment) -> Result<(i64, i32), SessionMomentError> {
    let bias = moment
        .tz_bias_min
        .filter(|&b| zone_bias_in_range(b))
        .ok_or(SessionMomentError::BadZone)?;
    let (y, mo, d, h, mi, s) = parse_civil(&moment.local).map_err(SessionMomentError::NotAMoment)?;
    let ft = civil_fields_to_filetime_utc(y, mo, d, h, mi, s, bias as i64)
        .filter(|&ft| instant_in_session_range(ft, bias))
        .ok_or(SessionMomentError::OutOfRange)?;
    Ok((ft, bias))
}

/// Everything needed to define a session. Pure data, no I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSpec {
    pub moment: Moment,
    pub mode: TimeMode,
    /// Whether the monotonic duration axis is scaled too (E4 / ADR-2).
    pub scale_duration: bool,
    /// Whether QueryPerformanceCounter is scaled too (ADR-2 reversal, opt-in). Separate from
    /// scale_duration because scaling QPC also scales a target's QPC-timed rendering (a risk the tick
    /// axis does not carry), so it is a deliberate, separate choice.
    pub scale_qpc: bool,
}

/// The verdict about whether time substitution took effect (chrono-mock.md 7.1).
///
/// `Undetermined` is genuine product vocabulary, not a placeholder: it is the
/// honest state when coverage cannot be established (mechanism not applied, target
/// vanished, audit incomplete). The core NEVER reports `Works` when it does not
/// know - that is the whole promise (untouchable rule 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Works,
    Partial,
    Fails,
    Undetermined,
}

impl Verdict {
    /// Stable wire token for this verdict (docs/08).
    pub fn wire(&self) -> &'static str {
        match self {
            Verdict::Works => "works",
            Verdict::Partial => "partial",
            Verdict::Fails => "fails",
            Verdict::Undetermined => "undetermined",
        }
    }

    /// Process exit code carried by this verdict (docs/08 section 8).
    pub fn exit_code(&self) -> i32 {
        match self {
            Verdict::Works => 0,
            Verdict::Partial => 10,
            Verdict::Fails => 11,
            Verdict::Undetermined => 4,
        }
    }

    /// Combine two verdicts into the family (session-wide) verdict. Each verdict encodes
    /// exactly the pair `(has_covered, has_uncovered)` - the family ORs those bits across
    /// processes, so the family `works` only when something is covered and nothing is left
    /// uncovered anywhere (untouchable rule 4 at the session level). This aggregates
    /// JUDGMENTS, never call counts - per-process reports stay separate (slice 11). OR
    /// is commutative and associative, so accumulation order does not matter, and coverage
    /// only grows, so the fold is monotonic.
    pub fn combine(self, other: Verdict) -> Verdict {
        let (c1, u1) = self.coverage_bits();
        let (c2, u2) = other.coverage_bits();
        Verdict::from_coverage_bits(c1 || c2, u1 || u2)
    }

    /// The `(has_covered, has_uncovered)` pair this verdict encodes.
    fn coverage_bits(self) -> (bool, bool) {
        match self {
            Verdict::Undetermined => (false, false),
            Verdict::Works => (true, false),
            Verdict::Fails => (false, true),
            Verdict::Partial => (true, true),
        }
    }

    /// The verdict for a coverage-flag pair - the single source of the coverage->verdict
    /// table, shared by `verdict_from_coverage` and the family fold (`combine`).
    fn from_coverage_bits(has_covered: bool, has_uncovered: bool) -> Verdict {
        match (has_covered, has_uncovered) {
            (false, false) => Verdict::Undetermined,
            (true, false) => Verdict::Works,
            (false, true) => Verdict::Fails,
            (true, true) => Verdict::Partial,
        }
    }
}

/// One time channel's coverage under a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelCoverage {
    pub channel: String,
    pub calls: u64,
    /// The count stopped at the hook's ceiling (`chrono_ctl::CALLS_CAP`, R5-WY11), so `calls` is a floor:
    /// the application called the channel at least that many times. Said beside the number rather than
    /// left for a reader to recognise the ceiling, which is an internal value that may change.
    pub capped: bool,
}

/// Audit result: channels `covered` (time substituted), `observed` (hooked and counted but
/// deliberately left real - ADR-7 class B object waits), `uncovered` (queried but not covered),
/// and any warning keys. Gathering (mechanism layer) is separated from evaluation here
/// (zasady/06 section 14).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Coverage {
    pub covered: Vec<ChannelCoverage>,
    pub observed: Vec<ChannelCoverage>,
    pub uncovered: Vec<String>,
    /// Channels this session meant to WATCH and could not hook. Its own bucket rather than
    /// `uncovered`, because the two answer different questions and only one of them is a verdict:
    /// an uncovered channel means the target asked for time we did not substitute, while one of
    /// these means we are not watching something we said we would. Folding them together would
    /// turn a failed observer into a `Partial` session, which would be a wrong answer to
    /// "did the substitution take effect".
    ///
    /// It exists because the alternative was silence. An observed channel that failed to install
    /// used to produce NO line at all - not covered, not uncovered, absent - so the report looked
    /// complete while a watch it promised was simply not running (untouchable rules 4 and 6).
    pub unobserved: Vec<String>,
    /// Channels that came under the fake clock only once their module loaded (ADR-10), by name.
    ///
    /// Every one of them is also in `covered` or `observed` with its count - this does not add a
    /// channel, it says which counts are floors and which scaled clocks jumped once when they joined.
    /// It exists because the warning alone said "a channel" and left the reader to guess which.
    pub installed_late: Vec<String>,
    pub warning_keys: Vec<String>,
}

/// Compute the verdict from gathered coverage.
///
/// An uncovered-but-queried channel is the CAUSE of a `Partial` verdict - it does
/// not have a separate exit code (docs/08 section 8). No evidence at all yields
/// `Undetermined`, never a fake `Works`. `observed` channels (ADR-7 class B, hooked
/// but deliberately left real) never sway the verdict - they are an honest side-channel
/// carried by their own warning, so an object wait left real neither makes nor breaks `works`.
pub fn verdict_from_coverage(cov: &Coverage) -> Verdict {
    Verdict::from_coverage_bits(!cov.covered.is_empty(), !cov.uncovered.is_empty())
}

// --- Anchor math: a session moment -> UTC FILETIME (100 ns ticks since 1601) ---
//
// The entered moment is wall-clock time in the SESSION zone - internally everything
// is UTC (untouchable rule 2). `tz_bias_min` follows the Win32 convention
// UTC = local + bias. A `None` bias treats the moment as UTC (host-zone handling
// and full DST/leap validation are a later slice - docs/08 open item).

/// The proleptic Gregorian year band this build works in, stated once, for everyone.
///
/// [`days_from_civil`] below does unchecked i64 math internally, so a year far enough out overflows
/// it. Measured: at year 25 252 734 927 766 555 `chrono calc` died with `attempt to add with
/// overflow` and exit 101 instead of answering, on `--base` alone - metadata is computed for every
/// result, so no extra step was needed to reach it.
///
/// The guard used to live inside [`moment_to_filetime_utc`] and nowhere else, which made the
/// substitution path safe and the calculator not: `run --at 300000-01-01T00:00:00` was refused
/// while `calc --base 300000-01-01T00:00:00` was accepted, so a date the calculator produced could
/// not be carried across its own "use in substitution" bridge. One band, checked wherever a year
/// enters or is computed, removes both the panic and that split.
///
/// FILETIME saturates near year 30828, far inside this band, so out-of-instant moments are still
/// dropped honestly long before this limit matters: the band keeps the CIVIL arithmetic sound, it
/// is not a statement about what can be represented as an instant.
pub const CIVIL_YEAR_MIN: i64 = -262_143;
/// Upper end of the civil year band - see [`CIVIL_YEAR_MIN`].
pub const CIVIL_YEAR_MAX: i64 = 262_143;

/// Whether a year is inside the band this build can compute on.
///
/// Checked at both kinds of entry, because they are genuinely different doors: parsing (`--base`,
/// `--at`, `--analyze`, presets) and each calculator step that COMPUTES a year. A step builds its
/// year arithmetically and never returns through the parser, so a guard at the parser alone leaves
/// the second door open - measured: `--base 2026-01-01T00:00:00 --shift +25252734927766000y`
/// panicked from a base that was perfectly legal.
pub fn civil_year_in_band(year: i64) -> bool {
    (CIVIL_YEAR_MIN..=CIVIL_YEAR_MAX).contains(&year)
}

/// Days from 1970-01-01 for a civil date (Howard Hinnant's branchless algorithm).
///
/// Precondition: `y` is inside [`CIVIL_YEAR_MIN`]..=[`CIVIL_YEAR_MAX`] - see [`civil_year_in_band`].
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Proleptic Gregorian leap-year test. A shared civil primitive next to `days_from_civil` -
/// the `calc` submodule reuses it, so the leap rule lives in exactly one place (rule 6).
fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Last valid day of a month (1..=12): 28/29/30/31. Returns 0 for a month outside 1..=12,
/// which every caller rejects upstream before using the result.
fn last_day_of_month(year: i64, month: i64) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap(year) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// The longest a month (1..=12) is in ANY year: 31/30/29, and 0 for a month outside 1..=12.
/// February answers 29, because a rule naming the leap day is legitimate - it simply does not
/// happen every year. Derived from `last_day_of_month` in a leap year rather than restating the
/// month lengths, so the calendar knowledge stays in one place (rule 6). Public because the
/// calendar loader needs it to tell a day that is impossible in SOME years (Feb 29) from one
/// impossible in EVERY year (April 31), and that loader lives in another crate.
pub fn max_day_in_month(month: u32) -> u32 {
    // 2024 is a leap year, so February answers 29 - the check is "possible in any year at all".
    last_day_of_month(2024, month as i64)
}

/// Whether `s` is one or more ASCII digits and nothing else - no sign, no space, no other script's
/// digits.
///
/// The gate every numeric FIELD of this tool's shapes goes through before it is parsed (a moment, a
/// time of day, a zone offset). Rust's integer parsers accept a leading `+`, so parsing alone let
/// `+05:+30` through as a zone and `2026-+1-05` as a date (R4-N34). One function, because the three
/// parsers had each been written to reject a sign and each had missed the same one.
pub fn is_ascii_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// Parse "YYYY-MM-DDTHH:MM:SS" (a space may replace the `T`). Strict on shape and
/// on field ranges - deeper calendar validation comes later.
fn parse_civil(local: &str) -> Result<(i64, i64, i64, i64, i64, i64), String> {
    parse_civil_as(local, local)
}

/// [`parse_civil`], with every refusal quoting `shown` instead of `local`. The date analyser parses a
/// text it built from the user's (a date alone gets `T00:00:00`, a fraction and an offset come off), and
/// its refusals quoted that text, a moment nobody had typed (R5-D5, item 5).
pub(crate) fn parse_civil_as(local: &str, shown: &str) -> Result<(i64, i64, i64, i64, i64, i64), String> {
    let (date, time) = local
        .split_once(['T', ' '])
        .ok_or_else(|| format!("moment must be YYYY-MM-DDTHH:MM:SS, got '{shown}'"))?;
    // A year before 1 CE carries a leading minus, and splitting on '-' would turn that into a
    // fourth field. The calculator can PRODUCE such a date - any shift back past year 1 does it -
    // so refusing to read one back meant the tool would not accept its own output: `--shift -10y`
    // from 0001-01-01 printed a date that `--base` then rejected (R3-6).
    let (negative_year, date) = match date.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, date),
    };
    let d: Vec<&str> = date.split('-').collect();
    let t: Vec<&str> = time.split(':').collect();
    if d.len() != 3 || t.len() != 3 {
        return Err(format!("moment must be YYYY-MM-DDTHH:MM:SS, got '{shown}'"));
    }
    // Digits only, checked before the parse: `i64::from_str` also takes a leading `+`, so
    // `2026-+1-05T+9:00:00` read as a real moment (R4-N34). A sign has one place in this shape - in
    // front of the year, handled above - and no field of a date the tool prints ever carries one.
    let p = |s: &str, what: &str| -> Result<i64, String> {
        if !is_ascii_digits(s) {
            return Err(format!("bad {what} in moment '{shown}' (digits only)"));
        }
        s.parse::<i64>().map_err(|_| format!("bad {what} in moment '{shown}'"))
    };
    let (year, month, day) = (p(d[0], "year")?, p(d[1], "month")?, p(d[2], "day")?);
    // Rejected here rather than after the field checks: a year outside the band makes the leap-year
    // test below meaningless, and `days_from_civil` further down would overflow on it.
    let year = if negative_year { -year } else { year };
    if !civil_year_in_band(year) {
        return Err(format!(
            "year {year} in moment '{shown}' is outside the range this build computes on \
             ({CIVIL_YEAR_MIN}..={CIVIL_YEAR_MAX})"
        ));
    }
    let (hour, min, sec) = (p(t[0], "hour")?, p(t[1], "minute")?, p(t[2], "second")?);
    if !(1..=12).contains(&month) {
        return Err(format!("month {month} out of range (1..=12) in '{shown}'"));
    }
    // Reject a day that cannot exist in its month (e.g. 2026-02-31): BOTH the substitution moment
    // and the calculator refuse an impossible date here rather than let `days_from_civil` silently
    // roll it into the next month (untouchable rule 4/6 - never a silent wrong moment).
    let last = last_day_of_month(year, month) as i64;
    if !(1..=last).contains(&day) {
        return Err(format!("day {day} out of range for month {month} in '{shown}'"));
    }
    // Field by field, left to right, and each refusal NAMES its field.
    //
    // The order is what was wrong, not the checks. The leap-second refusal came first, so
    // `2026-01-01T25:00:60` was answered "second 60 is a leap second" while the hour was 25 - the
    // reader fixes the second, runs again, and only then learns about the hour. And the catch-all
    // underneath said "time out of range" without saying WHICH of the three, on input where any of
    // them could be the one. A result that is right and cannot be explained is a defect here.
    if !(0..=23).contains(&hour) {
        return Err(format!("hour {hour} out of range (0..=23) in '{shown}'"));
    }
    if !(0..=59).contains(&min) {
        return Err(format!("minute {min} out of range (0..=59) in '{shown}'"));
    }
    // A leap second gets its own refusal, because it used to get silently accepted and then move the
    // DATE. `:60` passed this check as a nod to leap seconds, and `moment_to_filetime_utc` then summed
    // a time-of-day of 86400, which is midnight of the NEXT day - so `2026-06-15T23:59:60` became the
    // 16th with nothing said, and `filetime_utc_to_wall` could not give the input back. In the
    // calculator it was worse than a shift: one result carried two dates, the civil fields saying
    // Monday the 15th while the instant, FILETIME and RFC 1123 said Tuesday the 16th (R2-S11).
    //
    // Neither this model nor FILETIME has leap seconds, so accepting one is a promise we cannot keep.
    // Refuse it the way every other impossible moment above is refused, and name the reason - a tester
    // who pasted a timestamp from a leap-second-aware source needs to know which second to use. It sits
    // BELOW the hour and minute checks so it only ever answers for input whose only fault it is.
    if sec == 60 {
        return Err(format!(
            "second 60 in '{shown}' is a leap second - this model has none (use :59)"
        ));
    }
    if !(0..=59).contains(&sec) {
        return Err(format!("second {sec} out of range (0..=59) in '{shown}'"));
    }
    Ok((year, month, day, hour, min, sec))
}

/// Convert a session moment to a UTC FILETIME (100 ns ticks since 1601-01-01).
pub fn moment_to_filetime_utc(moment: &Moment) -> Result<i64, String> {
    // The year band is enforced by `parse_civil`, so by here the civil math is sound and what can
    // still fail is the instant: FILETIME saturates i64 near year 30828, far inside the band, so
    // the checked chain below is what rejects an out-of-FILETIME-range moment - as an honest Err,
    // never a panic (debug) or a wrapped number (release).
    let (y, mo, d, h, mi, s) = parse_civil(&moment.local)?;
    let bias = moment.tz_bias_min.unwrap_or(0) as i64;
    civil_fields_to_filetime_utc(y, mo, d, h, mi, s, bias)
        .ok_or_else(|| format!("moment '{}' is out of the representable FILETIME range", moment.local))
}

/// Civil fields (already validated) plus a zone bias to a UTC FILETIME, or `None` if the instant
/// falls outside what FILETIME can hold.
///
/// Split out of [`moment_to_filetime_utc`] so a caller that already HAS the fields does not have to
/// format them into a string and parse them straight back. The calculator's `zone` step did exactly
/// that, and it was not merely wasteful: it was the one place where a calculator step could fail on
/// FORMATTING, and every such failure was reported as an arithmetic overflow (R3-6).
///
/// Precondition: `y` is inside the band ([`civil_year_in_band`]), and the time fields are in range
/// - both hold for anything that came through `parse_civil` or a checked calculator step.
pub(crate) fn civil_fields_to_filetime_utc(
    y: i64,
    mo: i64,
    d: i64,
    h: i64,
    mi: i64,
    s: i64,
    bias_min: i64,
) -> Option<i64> {
    let ft = civil_fields_to_epoch_secs(y, mo, d, h, mi, s, bias_min)?
        .checked_add(DAYS_1601_TO_1970 * 86_400)?
        .checked_mul(10_000_000)?;
    // A FILETIME counts 100 ns intervals SINCE 1601-01-01 and Windows types it unsigned, so a
    // moment before that epoch has no FILETIME at all - and until this check existed the tool
    // produced one anyway, as a negative i64, then handed it around as if it were real.
    //
    // Measured, because the consequence is not cosmetic. `chrono run --at 1000-01-01T00:00:00`
    // reported the session as WORKS with a fake wall of 1000-01-01, while inside the target the
    // probe read: raw FILETIME 18257086497710184170 (the negative value read back as the unsigned
    // number it is - a year around 59000), and `GetSystemTime`/`GetLocalTime` showing the REAL
    // clock, because `FileTimeToSystemTime` refuses that value and the channel falls through.
    // One process, two epochs, and an audit calling it a pass - the same failure R2-X2 fixed at the
    // TOP of the range, which had no counterpart at the bottom (untouchable rules 2 and 4).
    (ft >= 0).then_some(ft)
}

/// The same civil fields as a UTC instant in SECONDS since the Unix epoch, which unlike a FILETIME
/// is perfectly happy to be negative.
///
/// Split from the FILETIME conversion because the two answer different questions and only one of
/// them has a floor. Year 1000 has a real instant (-30 610 231 200) and a real RFC 1123 rendering -
/// what it does not have is a FILETIME. Deriving the epoch fields from the FILETIME, as the
/// calculator used to, meant that fixing the FILETIME would have thrown away two correct answers
/// with the wrong one.
pub(crate) fn civil_fields_to_epoch_secs(
    y: i64,
    mo: i64,
    d: i64,
    h: i64,
    mi: i64,
    s: i64,
    bias_min: i64,
) -> Option<i64> {
    // h/mi/s are range-checked before they get here, so this sum is exact (max 86_400).
    let tod = h * 3_600 + mi * 60 + s;
    days_from_civil(y, mo, d)
        .checked_mul(86_400)
        .and_then(|day_secs| day_secs.checked_add(tod))
        .and_then(|local_secs| local_secs.checked_add(bias_min * 60)) // UTC = local + bias
}

/// Civil date `(year, month, day)` from a day count since 1970-01-01 (Howard
/// Hinnant's inverse of `days_from_civil`).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Format a UTC FILETIME (100 ns ticks since 1601) as a session-zone wall-clock
/// string "YYYY-MM-DDTHH:MM:SS". Inverse of `moment_to_filetime_utc` (no DST).
pub fn filetime_utc_to_wall(ft_utc: i64, tz_bias_min: i32) -> String {
    // The calculator's conversion, not a second copy of it (R5-A11): the two were written apart, and
    // copies of this arithmetic had already drifted once. It works in whole SECONDS rather than ticks - the bias is a
    // whole number of minutes, so dividing first gives exactly the same second for every input, and it
    // cannot overflow, which subtracting the bias in ticks did: an instant at the end of the range in a
    // zone east of UTC panicked a Chromium session core on its `state` event, leaving the browser
    // behind with its debugging port open (R4-S8).
    calc::filetime_to_civil(ft_utc, tz_bias_min).to_iso()
}

/// Days from the FILETIME epoch (1601-01-01) to the Unix epoch (1970-01-01). One constant for every
/// conversion between the two counts, which wrote it out four times (R5-A11).
pub const DAYS_1601_TO_1970: i64 = 134_774;

/// The day of the week of a day counted from the Unix epoch: 0 = Sunday .. 6 = Saturday. 1970-01-01
/// was a Thursday. One formula for the calculator and the calendar, which had three copies (R5-A11).
pub(crate) fn weekday(days_since_epoch: i64) -> u32 {
    (days_since_epoch + 4).rem_euclid(7) as u32
}

/// A session bias (minutes, UTC = local + bias) as the `+HH:MM` label every surface prints - the
/// offset is the bias with its sign turned. The command line and the calculator each had a copy.
pub fn bias_label(tz_bias_min: i32) -> String {
    // Widened before the sign turns: i32::MIN has no positive twin, and the label of any bias the
    // type can hold must not take the report down with it (found by the random properties).
    let offset = -i64::from(tz_bias_min);
    let sign = if offset < 0 { '-' } else { '+' };
    format!("{sign}{:02}:{:02}", offset.abs() / 60, offset.abs() % 60)
}

// The relative-delta grammar (fixed-length ticks) was retired into the shared step model:
// fixed units live in `calc::fixed_shift_ticks`, calendar units in `calc::step_target`, and
// parsing in the CLI's `parse_shift`. One grammar for `--at`, `jump`, and the calculator.

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(name: &str) -> ChannelCoverage {
        ChannelCoverage { channel: name.to_string(), calls: 1, capped: false }
    }

    #[test]
    fn multiplier_range_admits_freeze_and_every_documented_speed() {
        // Freeze is a VALUE on the wire, not an error - the GUI's freeze button sends 0.
        assert!(multiplier_in_range(0));
        // Every speed the product actually offers, and the bound itself.
        for m in [1, 10, 60, 1440, 86_400, MULTIPLIER_MAX] {
            assert!(multiplier_in_range(m), "x{m} must be allowed");
        }
        // Backward is a jump, never a rate (rule 3) - past the bound the clock leaves the
        // representable range mid-session (rule 2). Both measured before this gate existed.
        for m in [-1, -1_000_000, i64::MIN, MULTIPLIER_MAX + 1, i64::MAX] {
            assert!(!multiplier_in_range(m), "x{m} must be refused");
        }
    }

    fn moment(local: &str, bias: Option<i32>) -> Moment {
        Moment { local: local.to_string(), tz_bias_min: bias }
    }

    #[test]
    fn unix_epoch_utc_is_known_filetime() {
        // 1970-01-01T00:00:00 UTC == 116444736000000000 in FILETIME ticks.
        let ft = moment_to_filetime_utc(&moment("1970-01-01T00:00:00", Some(0))).unwrap();
        assert_eq!(ft, 116_444_736_000_000_000);
    }

    #[test]
    fn bias_shifts_local_to_utc() {
        // Local 03:14:07 in a UTC-5 session (bias +300) is 08:14:07 UTC.
        let with_bias = moment_to_filetime_utc(&moment("2038-01-19T03:14:07", Some(300))).unwrap();
        let as_utc = moment_to_filetime_utc(&moment("2038-01-19T08:14:07", Some(0))).unwrap();
        assert_eq!(with_bias, as_utc);
    }

    #[test]
    fn wall_round_trips_through_filetime() {
        for (s, bias) in [
            ("2038-01-19T03:14:07", 0),
            ("1970-01-01T00:00:00", 0),
            ("2000-02-29T12:00:00", 300),
            ("2026-08-12T20:30:00", -120),
        ] {
            let ft = moment_to_filetime_utc(&moment(s, Some(bias))).unwrap();
            assert_eq!(filetime_utc_to_wall(ft, bias), s);
        }
    }

    /// The last instant an i64 holds, in the zones at both ends of the accepted band, is written out
    /// rather than panicking - it used to overflow subtracting the bias in ticks for every zone east
    /// of UTC (R4-S8). The value is the known end of the FILETIME range, 30828-09-14T02:48:05 UTC.
    #[test]
    fn the_wall_text_of_the_last_instant_never_overflows() {
        assert_eq!(filetime_utc_to_wall(i64::MAX, 0), "30828-09-14T02:48:05");
        assert_eq!(filetime_utc_to_wall(i64::MAX, -120), "30828-09-14T04:48:05");
        assert_eq!(filetime_utc_to_wall(i64::MAX, -899), "30828-09-14T17:47:05");
        // The other end, in a zone west of UTC, overflowed the same way in the other direction.
        assert!(filetime_utc_to_wall(i64::MIN, 899).starts_with('-'));
        // Dividing to seconds first changes nothing below a second: a tick short of the next second
        // still reads as this one, in a zone on either side of UTC.
        let ft = moment_to_filetime_utc(&moment("2026-08-12T20:30:00", Some(0))).unwrap() + 9_999_999;
        assert_eq!(filetime_utc_to_wall(ft, -120), "2026-08-12T22:30:00");
        assert_eq!(filetime_utc_to_wall(ft, 300), "2026-08-12T15:30:00");
        assert_eq!(filetime_utc_to_wall(-1, 0), "1600-12-31T23:59:59");
    }

    /// A sign inside a field is refused (R4-N34). The integer parse accepted a leading `+`, so
    /// `2026-+1-05` read as 5 January. The one sign this shape has - a minus before the year - still
    /// reads, because the calculator writes years before 1 CE that way.
    #[test]
    fn a_sign_inside_a_field_of_a_moment_is_refused() {
        for bad in [
            "+2026-01-05T00:00:00",
            "2026-+1-05T00:00:00",
            "2026-01-+5T00:00:00",
            "2026-01-05T+9:00:00",
            "2026-01-05T09:+0:00",
            "2026-01-05T09:00:+0",
            "2026-01-05T 9:00:00",
        ] {
            assert!(parse_civil(bad).is_err(), "{bad}");
        }
        assert!(parse_civil("-0009-05-05T00:00:00").is_ok());
        assert!(parse_civil("2026-01-05T09:00:00").is_ok());
        assert!(!is_ascii_digits(""));
        assert!(!is_ascii_digits("+1"));
        assert!(is_ascii_digits("0042"));
    }

    #[test]
    fn space_separator_is_accepted() {
        let a = moment_to_filetime_utc(&moment("2000-02-29 12:00:00", Some(0))).unwrap();
        let b = moment_to_filetime_utc(&moment("2000-02-29T12:00:00", Some(0))).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn bad_moment_is_rejected() {
        assert!(moment_to_filetime_utc(&moment("not-a-date", None)).is_err());
        assert!(moment_to_filetime_utc(&moment("2038-13-01T00:00:00", None)).is_err());
    }

    #[test]
    fn impossible_day_is_rejected_on_the_substitution_path() {
        // The substitution moment must refuse an impossible day just like the calculator does,
        // instead of silently rolling it into the next month (rule 4/6).
        assert!(moment_to_filetime_utc(&moment("2026-02-31T12:00:00", Some(0))).is_err()); // Feb has 28
        assert!(moment_to_filetime_utc(&moment("2025-02-29T00:00:00", Some(0))).is_err()); // 2025 not leap
        assert!(moment_to_filetime_utc(&moment("2026-04-31T00:00:00", Some(0))).is_err()); // Apr has 30
        assert!(moment_to_filetime_utc(&moment("2026-01-00T00:00:00", Some(0))).is_err()); // day 0
        // The valid leap day still converts.
        assert!(moment_to_filetime_utc(&moment("2024-02-29T00:00:00", Some(0))).is_ok());
    }

    #[test]
    fn a_leap_second_is_refused_not_rolled_into_the_next_day() {
        // R2-S11. `:60` used to pass the range check and then sum to a time-of-day of 86400 - midnight
        // of the NEXT day - so the moment moved a full day with nothing said. In the calculator the same
        // input produced ONE result carrying two dates: the civil fields said Monday the 15th while the
        // instant and RFC 1123 said Tuesday the 16th.
        let err = moment_to_filetime_utc(&moment("2026-06-15T23:59:60", Some(0)))
            .expect_err("a leap second is not a moment this model can represent");
        assert!(err.contains("leap second"), "the message must name the reason, got: {err}");

        // Not only at the end of a day: the second field is refused wherever it appears.
        assert!(moment_to_filetime_utc(&moment("2026-06-15T12:00:60", Some(0))).is_err());
        // The calculator shares this parser, so it refuses the same input.
        assert!(calc::parse_civil_datetime("2026-06-15T23:59:60").is_err());

        // And the second before it still converts, to exactly 23:59:59 of the SAME day.
        let ok = moment_to_filetime_utc(&moment("2026-06-15T23:59:59", Some(0))).unwrap();
        let midnight = moment_to_filetime_utc(&moment("2026-06-16T00:00:00", Some(0))).unwrap();
        assert_eq!(midnight - ok, 10_000_000, "one second apart, not one day");
    }

    /// Each time field is refused BY NAME, and in the order it is written.
    ///
    /// The leap-second check used to run first, so `25:00:60` was answered "second 60 is a leap
    /// second" while the hour was 25 - the reader fixes the second, runs again, and only then hears
    /// about the hour. Underneath it, one catch-all said "time out of range" without naming which of
    /// the three was out of it. Both are the same defect: an answer that is correct and cannot be
    /// acted on.
    #[test]
    fn each_time_field_is_refused_by_name_and_in_reading_order() {
        let refusal = |s: &str| {
            moment_to_filetime_utc(&moment(s, Some(0))).expect_err("this moment cannot be represented")
        };

        let hour = refusal("2026-01-01T25:00:60");
        assert!(hour.contains("hour 25"), "the FIRST fault from the left, named: {hour}");
        assert!(!hour.contains("leap second"), "not the second, which is also wrong here: {hour}");

        let minute = refusal("2026-01-01T12:75:00");
        assert!(minute.contains("minute 75"), "got: {minute}");

        let second = refusal("2026-01-01T12:00:99");
        assert!(second.contains("second 99"), "got: {second}");

        // With hour and minute sound, the leap second still gets its own reason rather than the range.
        let leap = refusal("2026-01-01T23:59:60");
        assert!(leap.contains("leap second"), "got: {leap}");
    }

    #[test]
    fn out_of_filetime_range_year_is_an_error_not_a_wrap() {
        // An extreme but syntactically valid year overflows the FILETIME tick math. It must be a
        // reported Err (so callers show "(out of range)"), never a panic (debug) or a wrapped,
        // plausible-but-false number (release) - the promise "never a bad number".
        assert!(moment_to_filetime_utc(&moment("40000-01-01T00:00:00", Some(0))).is_err());
        // An absurd year would overflow days_from_civil's own internal math - the year guard rejects
        // it before that, so there is no panic even here.
        assert!(moment_to_filetime_utc(&moment("9223372036854775807-01-01T00:00:00", Some(0))).is_err());
    }

    /// A moment before 1601 has no FILETIME, and saying otherwise was not harmless.
    ///
    /// This test used to assert the opposite - that `moment_to_filetime_utc` stays `Ok` for such a
    /// date - so that the calculator could still show its epoch. The intent was right and the
    /// mechanism was wrong: the conversion returned a NEGATIVE i64, and a FILETIME counts from
    /// 1601 unsigned. Measured consequence on the substitution path, which is what makes this a
    /// defect rather than a display quirk: `chrono run --at 1000-01-01T00:00:00` reported WORKS
    /// with a fake wall of 1000-01-01, while inside the target the raw FILETIME channel read
    /// 18257086497710184170 and `GetSystemTime`/`GetLocalTime` showed the REAL clock, because
    /// `FileTimeToSystemTime` rejects that value. One process, two epochs, audit calling it a pass.
    ///
    /// The epoch the old test was protecting is still there - it just no longer comes from the
    /// FILETIME (see `formats`), which is what let both answers be right at once.
    #[test]
    fn a_moment_before_the_filetime_epoch_is_refused_not_returned_negative() {
        assert!(moment_to_filetime_utc(&moment("1000-06-15T00:00:00", Some(0))).is_err());
        // The epoch itself is exactly on the boundary and IS representable.
        assert!(moment_to_filetime_utc(&moment("1601-01-01T00:00:00", Some(0))).is_ok());
        // One second earlier is not.
        assert!(moment_to_filetime_utc(&moment("1600-12-31T23:59:59", Some(0))).is_err());
        // And the zone moves that boundary, which is the case a tester actually hits: midnight on
        // the epoch date in a positive-offset session is BEFORE the epoch in UTC.
        assert!(moment_to_filetime_utc(&moment("1601-01-01T00:00:00", Some(-120))).is_err());
        assert!(moment_to_filetime_utc(&moment("1601-01-01T02:00:00", Some(-120))).is_ok());
    }

    /// The property the old test was really after: a date before 1601 still has an instant, and the
    /// calculator still reports it. Refusing the FILETIME must not take the epoch down with it.
    #[test]
    fn a_date_before_the_filetime_epoch_still_has_an_epoch_and_an_rfc_rendering() {
        let civil = calc::parse_civil_datetime("1000-01-01T00:00:00").expect("a valid civil date");
        let f = calc::formats(&civil, -120);
        assert_eq!(f.epoch_seconds, Some(-30_610_231_200));
        assert_eq!(f.epoch_millis, Some(-30_610_231_200_000));
        assert_eq!(f.rfc1123.as_deref(), Some("Tue, 31 Dec 0999 22:00:00 GMT"));
        assert_eq!(f.iso_date, "1000-01-01");
        assert!(f.filetime.is_none(), "a pre-1601 moment has no FILETIME");
    }

    #[test]
    fn no_evidence_is_undetermined_not_works() {
        let cov = Coverage::default();
        assert_eq!(verdict_from_coverage(&cov), Verdict::Undetermined);
    }

    #[test]
    fn all_covered_is_works() {
        let cov = Coverage { covered: vec![ch("GetSystemTimeAsFileTime")], ..Default::default() };
        assert_eq!(verdict_from_coverage(&cov), Verdict::Works);
    }

    #[test]
    fn full_wall_set_is_works() {
        // The whole point of Stage 3: `works` means the full wall-clock set, not one
        // channel. The core stays name-agnostic - it only sees covered vs uncovered.
        let cov = Coverage {
            covered: vec![
                ch("GetSystemTimeAsFileTime"),
                ch("GetSystemTimePreciseAsFileTime"),
                ch("GetSystemTime"),
                ch("GetLocalTime"),
                ch("NtQuerySystemTime"),
            ],
            ..Default::default()
        };
        assert_eq!(verdict_from_coverage(&cov), Verdict::Works);
    }

    #[test]
    fn some_uncovered_is_partial() {
        let cov = Coverage {
            covered: vec![ch("GetSystemTimeAsFileTime")],
            uncovered: vec!["KUSER_SHARED_DATA".to_string()],
            ..Default::default()
        };
        assert_eq!(verdict_from_coverage(&cov), Verdict::Partial);
    }

    #[test]
    fn only_uncovered_is_fails() {
        let cov = Coverage { uncovered: vec!["KUSER_SHARED_DATA".to_string()], ..Default::default() };
        assert_eq!(verdict_from_coverage(&cov), Verdict::Fails);
    }

    #[test]
    fn combine_rolls_up_the_family_verdict() {
        use Verdict::*;
        // Undetermined (a process that touched nothing) is the identity - a launcher whose
        // child does the timekeeping is judged by the child: the family works.
        assert_eq!(Undetermined.combine(Works), Works);
        assert_eq!(Works.combine(Undetermined), Works);
        assert_eq!(Undetermined.combine(Undetermined), Undetermined);
        // Any uncovered anywhere drags a covered family down to partial (rule 4, honest).
        assert_eq!(Works.combine(Fails), Partial);
        assert_eq!(Works.combine(Partial), Partial);
        assert_eq!(Works.combine(Works), Works);
        // Nothing covered anywhere but something queried -> the family fails.
        assert_eq!(Fails.combine(Fails), Fails);
        assert_eq!(Fails.combine(Undetermined), Fails);
        // Commutative and associative, so accumulation order never matters.
        assert_eq!(Fails.combine(Works), Works.combine(Fails));
        assert_eq!(
            Undetermined.combine(Works).combine(Fails),
            Undetermined.combine(Works.combine(Fails))
        );
    }

    #[test]
    fn observed_does_not_sway_verdict() {
        // ADR-7 class B: an object wait left real is counted in `observed`, never covered/uncovered.
        // With the full wall set covered, an observed WaitForSingleObject must not change `works`...
        let works = Coverage {
            covered: vec![ch("GetSystemTimeAsFileTime")],
            observed: vec![ch("WaitForSingleObject")],
            ..Default::default()
        };
        assert_eq!(verdict_from_coverage(&works), Verdict::Works);
        // ...and observed alone (no covered, no uncovered) is not evidence of substitution.
        let observed_only = Coverage { observed: vec![ch("WaitForSingleObject")], ..Default::default() };
        assert_eq!(verdict_from_coverage(&observed_only), Verdict::Undetermined);
    }

    #[test]
    fn exit_codes_match_contract() {
        assert_eq!(Verdict::Works.exit_code(), 0);
        assert_eq!(Verdict::Undetermined.exit_code(), 4);
        assert_eq!(Verdict::Partial.exit_code(), 10);
        assert_eq!(Verdict::Fails.exit_code(), 11);
    }

    /// R4-S9: the zone band is the one `--zone` has always parsed, 14:59 either side, and nothing
    /// past it - `i32` reached the hook unchecked over the protocol before.
    #[test]
    fn the_zone_band_is_fourteen_fifty_nine_either_side() {
        for ok in [0, 1, -1, 899, -899, 300, -120] {
            assert!(zone_bias_in_range(ok), "bias {ok}");
        }
        for bad in [900, -900, 1440, i32::MIN, i32::MAX] {
            assert!(!zone_bias_in_range(bad), "bias {bad}");
        }
    }

    /// The top of the session range is `FAKE_WALL_MAX` = 30828-09-13T11:48:05 UTC, a whole second, and
    /// a moment one second later is refused rather than started and clamped (R4-S7b).
    #[test]
    fn a_session_moment_stops_at_the_last_instant_the_clock_can_hold() {
        assert_eq!(session_instant(&moment("30828-09-13T11:48:05", Some(0))), Ok((FAKE_WALL_MAX, 0)));
        assert_eq!(
            session_instant(&moment("30828-09-13T11:48:06", Some(0))),
            Err(SessionMomentError::OutOfRange)
        );
        // The widest zone east of UTC names the same last instant as local time, and fits: the
        // headroom under the clamp is there for exactly this.
        assert_eq!(session_instant(&moment("30828-09-14T02:47:05", Some(-899))), Ok((FAKE_WALL_MAX, -899)));
        // The report's example: parsed fine, representable as a FILETIME, past the clock's range.
        assert_eq!(
            session_instant(&moment("30828-09-14T02:48:05", Some(0))),
            Err(SessionMomentError::OutOfRange)
        );
    }

    /// R4-S7d: the floor holds in UTC AND as local time in the session zone, because the hook serves
    /// the instant both ways and a negative local FILETIME drops `GetLocalTime` to the real clock.
    #[test]
    fn a_session_moment_starts_no_earlier_than_1601_in_utc_or_in_its_zone() {
        assert_eq!(session_instant(&moment("1601-01-01T00:00:00", Some(0))), Ok((0, 0)));
        // UTC-05:00: local midnight on the first day is 05:00 UTC, and both are inside.
        assert_eq!(
            session_instant(&moment("1601-01-01T00:00:00", Some(300))),
            Ok((5 * 3600 * 10_000_000, 300))
        );
        // The report's example: 01:00 UTC is inside, but the local time is before 1601.
        assert_eq!(
            session_instant(&moment("1600-12-31T20:00:00", Some(300))),
            Err(SessionMomentError::OutOfRange)
        );
        // UTC+02:00: 01:00 local is 23:00 UTC the day before 1601 - out on the UTC side.
        assert_eq!(
            session_instant(&moment("1601-01-01T01:00:00", Some(-120))),
            Err(SessionMomentError::OutOfRange)
        );
        assert_eq!(
            session_instant(&moment("1000-01-01T00:00:00", Some(0))),
            Err(SessionMomentError::OutOfRange)
        );
    }

    /// R4-S9 and the decision on a missing zone: a session never guesses one. The zone is checked
    /// before the text, because a moment cannot be read without it.
    #[test]
    fn a_session_moment_needs_a_zone_inside_the_band() {
        assert_eq!(session_instant(&moment("2030-06-15T12:00:00", None)), Err(SessionMomentError::BadZone));
        assert_eq!(session_instant(&moment("2030-06-15T12:00:00", Some(900))), Err(SessionMomentError::BadZone));
        assert_eq!(
            session_instant(&moment("2030-06-15T12:00:00", Some(i32::MIN))),
            Err(SessionMomentError::BadZone)
        );
        assert_eq!(session_instant(&moment("not a moment", None)), Err(SessionMomentError::BadZone));
        assert!(matches!(
            session_instant(&moment("2026-02-30T00:00:00", Some(0))),
            Err(SessionMomentError::NotAMoment(_))
        ));
        // An ordinary moment comes back as the same instant the plain conversion gives.
        let m = moment("2038-01-19T03:14:07", Some(-120));
        assert_eq!(session_instant(&m), Ok((moment_to_filetime_utc(&m).unwrap(), -120)));
    }
}
