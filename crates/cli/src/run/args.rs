//! What the user typed, turned into a `RunArgs` and nothing more.
//!
//! Parsing lives apart from driving on purpose. Every value the core receives is decided here or in
//! the moment resolver beside it, so a flag can be read, rejected and tested without starting a
//! process. The struct's fields are open to the rest of `run` and to nothing else.

use std::collections::HashMap;

use chrono_proto::{TargetConsole, TargetSpec, TimeModeKind};

use crate::cli::{flag_value, refuse_repeat};
use crate::zone::parse_zone_to_bias;

/// The flags of `run` that take exactly one value, each refused when given twice (R5-P16). `--param`
/// repeats on purpose, one parameter each, and refuses a repeated id on its own.
const ONE_VALUE_FLAGS: [&str; 11] = [
    "--at", "--preset", "--zone", "--mode", "--args", "--cwd", "--ticks", "--timeout", "--set-after", "--jump-after",
    "--report",
];

pub(crate) struct RunArgs {
    pub(super) target: String,
    pub(super) args: Vec<String>,
    /// Working directory for the target (`--cwd`). `None` means "do not ask for one", and the target
    /// then inherits ours - the behaviour every run had before this flag existed.
    pub(super) cwd: Option<String>,
    pub(super) at: Option<String>,
    pub(super) zone_bias_min: Option<i32>,
    /// The time flow on the wire: flow, frozen or multiplier.
    pub(super) mode: TimeModeKind,
    pub(super) multiplier: Option<i64>,
    pub(super) scale_duration: bool,
    /// Also scale QueryPerformanceCounter (ADR-2 reversal, opt-in `--scale-qpc`).
    pub(super) scale_qpc: bool,
    /// Run even when the opening verdict says the substitution did not take effect (`--force`).
    pub(super) force: bool,
    /// Reach the web pages inside the application through its embedded engine's debugging port
    /// (docs/09). On unless `--no-embedded` - the opt-out for a tester who does not want that port
    /// open in their application for the session.
    pub(super) embedded: bool,
    /// Also reach the web pages of an application that runs as administrator (`--elevated-embedded`,
    /// docs/09 section 12.19): the session writes one WebView2 value to the machine registry for its
    /// duration and removes it. Off by default and exclusive of `--no-embedded`.
    pub(super) elevated_embedded: bool,
    /// Start the web engine with its slowdown of timers in hidden windows switched off
    /// (`--keep-background-timers`, R4-N28) - the browser of a Chromium target and the engine inside an
    /// application alike. Off by default: the application then also works at full pace while hidden.
    pub(super) keep_background_timers: bool,
    /// Leave the sandbox of an application on Mozilla's Gecko engine on. On unless `--no-gecko-sandbox`
    /// (ADR-28), which starts the target with that sandbox switched off so the hook enters the processes
    /// its pages run in.
    pub(super) gecko_sandbox: bool,
    /// How many `state` heartbeats to stream before ending. 0 = no cut: the session lasts until the
    /// target, and whatever it started on the session clock, has exited (ADR-16) - or, on the
    /// Chromium path, until the browser closes its debugging connection (R4-S18).
    pub(super) ticks: u64,
    /// After the Nth state heartbeat, send set_multiplier M (in-flight speed change).
    pub(super) set_after: Option<(u64, i64)>,
    /// After the Nth state heartbeat, jump the wall clock to the given moment.
    pub(super) jump_after: Option<(u64, String)>,
    /// Optional path to write the human evidence report to, in addition to stdout.
    pub(super) report: Option<String>,
    /// Write the target's arguments into the evidence file word for word (`--report-args`, R5-D16).
    /// Off by default: an argument can carry a password or a token, and the file is made to be handed
    /// to someone, so without the flag it says only how many there were.
    pub(super) report_args: bool,
    pub(super) json: bool,
    /// A named preset id (docs/04 4.3): the moment AND the time mode come from `presets/<id>.json`
    /// instead of --at/--mode. None = build them from the flags. Exclusive of --at/--mode/--scale-duration.
    pub(super) preset: Option<String>,
    /// Preset parameter values from `--param id=value` (docs/04 4.2). Only meaningful with --preset.
    /// In run, a `target_file_creation` hint also resolves from the target's file date.
    pub(super) params: HashMap<String, String>,
    /// Resolve everything and start nothing (`--dry-run`). The session is described rather than run:
    /// no core process, no target, no evidence file, and no verdict to report.
    pub(super) dry_run: bool,
    /// Give up after this many seconds of wall time, whatever the session is doing (`--timeout`).
    /// None = no ceiling, which stays the default because the normal way to bound a run is
    /// `--ticks`, and a session driving a real app has no business being cut off by surprise.
    pub(super) timeout_secs: Option<u64>,
}

/// A number on the `run` command line - a tick, a count of heartbeats or seconds, a multiplier - read
/// under the rule every number this tool takes from a person goes through (R4-N34): ASCII digits and
/// nothing else. Rust's own parse also takes a leading `+`, so `--ticks +5` ran five heartbeats and
/// `--mode x+60` at sixty, while `calc` refused the same sign (R5-D57). `refusal` opens the message,
/// and a run of digits too large for the type is refused without the digits-only note, which would
/// then be the wrong advice.
fn digits_only<T: std::str::FromStr>(field: &str, refusal: String) -> Result<T, String> {
    if !chrono_core::is_ascii_digits(field) {
        return Err(format!("{refusal} (digits only)"));
    }
    field.parse().map_err(|_| refusal)
}

/// Parse `--mode` into the wire's time flow and optional multiplier.
/// `flow` = real speed, `frozen` = held, `xN` = accelerated N times (N >= 1).
pub(crate) fn parse_mode(raw: &str) -> Result<(TimeModeKind, Option<i64>), String> {
    match raw {
        "flow" => Ok((TimeModeKind::Flow, None)),
        "frozen" => Ok((TimeModeKind::Frozen, None)),
        _ => {
            let n = raw
                .strip_prefix('x')
                .or_else(|| raw.strip_prefix('X'))
                .ok_or_else(|| format!("mode must be flow, frozen, or xN like x60, got '{raw}'"))?;
            let m: i64 = digits_only(n, format!("bad multiplier in mode '{raw}'"))?;
            // The friendly surface keeps its own floor of 1: `x0` here would be a confusing way to
            // spell `--mode frozen`, which already exists. The ceiling is the shared one, so the CLI
            // and the protocol agree on what a session may run at.
            if m < 1 {
                return Err(format!("multiplier must be >= 1, got '{raw}'"));
            }
            if m > chrono_core::MULTIPLIER_MAX {
                return Err(format!(
                    "multiplier must be <= {}, got '{raw}' - past that the fake clock leaves the \
                     representable date range mid-session and the time channels start disagreeing",
                    chrono_core::MULTIPLIER_MAX
                ));
            }
            Ok((TimeModeKind::Multiplier, Some(m)))
        }
    }
}

/// Split an `--args` value into arguments, honouring double-quotes so one argument may contain
/// spaces (`--args '"a b" c'` -> ["a b", "c"]). Whitespace outside quotes separates arguments - a
/// quote toggles quoting and is dropped. Plain space-separated values behave exactly as before, so
/// existing usage is unchanged - `mech`'s quoting re-quotes each token for the target's CRT (P9).
pub(crate) fn split_args(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut has_token = false;
    for c in raw.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                has_token = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if has_token {
                    out.push(std::mem::take(&mut cur));
                    has_token = false;
                }
            }
            c => {
                cur.push(c);
                has_token = true;
            }
        }
    }
    if has_token {
        out.push(cur);
    }
    out
}

/// What the driver puts in `start.target`. Separated from the command so a test can see it: the
/// three fields used to be written inline with `cwd` hard-coded to `None`, and putting that back
/// changed nothing any test could notice - measured, by reverting it.
pub(crate) fn target_spec_for(ra: &RunArgs) -> TargetSpec {
    TargetSpec {
        path: ra.target.clone(),
        args: ra.args.clone(),
        cwd: ra.cwd.clone(),
        embedded: ra.embedded,
        elevated_embedded: ra.elevated_embedded,
        keep_background_timers: ra.keep_background_timers,
        gecko_sandbox: ra.gecko_sandbox,
        // The terminal `chrono run` was started from: its console for input, its stderr for output
        // (R4-D15). Named rather than left to the default, so the driver's choice is visible here.
        console: TargetConsole::Shared,
    }
}

/// What stops `--elevated-embedded` before anything starts, or `None`. The option writes the machine
/// registry, which only an administrator can do, so from a shell that is not one it is refused with the
/// same code a bad flag gets - and a dry run refuses it too, because a plan that described a command
/// the run would then refuse would be describing nothing. A token that could not be read counts as not
/// elevated: the refusal is the safe side of not knowing.
pub(crate) fn elevation_problem(ra: &RunArgs, elevated: Option<bool>) -> Option<&'static str> {
    if ra.elevated_embedded && elevated != Some(true) {
        Some("--elevated-embedded needs Chrono Mock to run as administrator, and it is not: it writes a WebView2 value to the machine registry")
    } else {
        None
    }
}

/// The value of `--set-after`: `<tick>:<multiplier>`, with the multiplier under the same invariant as
/// `--mode xN` (`parse_mode`) - an in-flight multiplier is >= 1 and within the clock's range. A zero or
/// negative value would freeze or run the wall clock backward as a silent side effect of an
/// unvalidated surface (rule 4) - freezing in flight is not a feature here.
fn parse_set_after(raw: &str) -> Result<(u64, i64), String> {
    let (t, m) = raw.split_once(':').ok_or("--set-after must be <tick>:<multiplier>")?;
    let tick: u64 = digits_only(t, format!("bad tick in '{raw}'"))?;
    let mult: i64 = digits_only(m, format!("bad multiplier in '{raw}'"))?;
    if mult < 1 {
        return Err(format!("--set-after multiplier must be >= 1, got '{raw}'"));
    }
    if mult > chrono_core::MULTIPLIER_MAX {
        return Err(format!("--set-after multiplier must be <= {}, got '{raw}'", chrono_core::MULTIPLIER_MAX));
    }
    Ok((tick, mult))
}

/// The flags about the web engines a session reaches. A type of their own so `parse_run_args`, whose
/// length is the workspace's ceiling, takes them in one line however many there are.
#[derive(Debug, Clone, Copy)]
struct EngineFlags {
    embedded: bool,
    elevated_embedded: bool,
    keep_background_timers: bool,
    gecko_sandbox: bool,
}

impl EngineFlags {
    /// Take `flag` if it is one of these, and say whether it was.
    fn take(&mut self, flag: &str) -> bool {
        match flag {
            "--no-embedded" => self.embedded = false,
            "--elevated-embedded" => self.elevated_embedded = true,
            "--keep-background-timers" => self.keep_background_timers = true,
            "--no-gecko-sandbox" => self.gecko_sandbox = false,
            _ => return false,
        }
        true
    }
}

/// The flags that take no value and only switch something on, apart from the engine's own
/// (`EngineFlags`). A type of their own for the same reason: `parse_run_args` takes them in one line.
#[derive(Debug, Default, Clone, Copy)]
struct Switches {
    scale_qpc: bool,
    force: bool,
    dry_run: bool,
    json: bool,
    report_args: bool,
}

impl Switches {
    /// Take `flag` if it is one of these, and say whether it was.
    fn take(&mut self, flag: &str) -> bool {
        let slot = match flag {
            // ADR-2 reversal, opt-in. NOT a preset-exclusive time flag: a preset carries its own
            // scale_duration but never scale_qpc, so --scale-qpc is the only source and composes with
            // --preset (unlike --scale-duration, which would double a preset's own setting).
            "--scale-qpc" => &mut self.scale_qpc,
            "--force" => &mut self.force,
            "--dry-run" => &mut self.dry_run,
            "--json" => &mut self.json,
            "--report-args" => &mut self.report_args,
            _ => return false,
        };
        *slot = true;
        true
    }
}

/// The flags that cannot stand together, refused rather than resolved by picking one silently.
fn check_combinations(
    has_preset: bool,
    saw_time_flag: bool,
    has_params: bool,
    engine: EngineFlags,
    report_args_without_report: bool,
) -> Result<(), String> {
    let EngineFlags { embedded, elevated_embedded, .. } = engine;
    // A preset supplies both the moment and the time mode, so combining it with --at/--mode/
    // --scale-duration would mean two sources.
    if has_preset && saw_time_flag {
        return Err("--preset supplies the moment and mode; it cannot be combined with \
                    --at/--mode/--scale-duration"
            .into());
    }
    // --param only makes sense with --preset (it fills a preset's declared parameters).
    if has_params && !has_preset {
        return Err("--param needs --preset (parameters belong to a preset)".into());
    }
    // One asks for the pages of an application that runs as administrator to be reached, the other
    // for the pages to be left alone - saying both is a contradiction, not a setting.
    if elevated_embedded && !embedded {
        return Err("--elevated-embedded cannot be combined with --no-embedded".into());
    }
    // The arguments go into the evidence file, and without --report there is no file to put them in.
    if report_args_without_report {
        return Err("--report-args needs --report (it says what goes into the evidence file)".into());
    }
    Ok(())
}

pub(crate) fn parse_run_args(argv: &[String]) -> Result<RunArgs, String> {
    let mut target: Option<String> = None;
    let mut args: Vec<String> = Vec::new();
    let mut cwd: Option<String> = None;
    let mut at: Option<String> = None;
    let mut zone_bias_min: Option<i32> = None;
    let mut mode = TimeModeKind::Flow;
    let mut multiplier: Option<i64> = None;
    let mut scale_duration = false;
    let mut engine = EngineFlags { embedded: true, elevated_embedded: false, keep_background_timers: false, gecko_sandbox: true };
    let mut switches = Switches::default();
    let mut ticks: u64 = 0;
    let mut timeout_secs: Option<u64> = None;
    let mut set_after: Option<(u64, i64)> = None;
    let mut jump_after: Option<(u64, String)> = None;
    let mut report: Option<String> = None;
    let mut preset: Option<String> = None;
    let mut params: HashMap<String, String> = HashMap::new();
    // Whether any moment/mode flag appeared, so `--preset` (which supplies both) can reject being
    // combined with them instead of silently ignoring one source.
    let mut saw_time_flag = false;

    // The flags of ONE_VALUE_FLAGS seen so far, so a second one is refused instead of winning.
    let mut given: Vec<&str> = Vec::new();

    let mut i = 0;
    while i < argv.len() {
        refuse_repeat(&mut given, argv[i].as_str(), &ONE_VALUE_FLAGS)?;
        match argv[i].as_str() {
            "--at" => {
                i += 1;
                at = Some(flag_value(argv, i, "--at", "a moment like 2026-01-31T00:00:00 or +3d")?.to_string());
                saw_time_flag = true;
            }
            "--preset" => {
                i += 1;
                preset = Some(flag_value(argv, i, "--preset", "an id like month-end")?.to_string());
            }
            "--param" => {
                i += 1;
                let raw = flag_value(argv, i, "--param", "id=value like start_date=2026-01-01")?;
                let (id, value) = raw
                    .split_once('=')
                    .ok_or_else(|| format!("--param must be id=value, got '{raw}'"))?;
                if id.is_empty() {
                    return Err(format!("--param needs a non-empty id, got '{raw}'"));
                }
                if params.insert(id.to_string(), value.to_string()).is_some() {
                    return Err(format!("--param {id} is given twice - pass each parameter once"));
                }
            }
            "--zone" => {
                i += 1;
                let raw = flag_value(argv, i, "--zone", "a value like +02:00")?;
                zone_bias_min = Some(parse_zone_to_bias(raw)?);
            }
            "--mode" => {
                i += 1;
                let raw = flag_value(argv, i, "--mode", "a value like x60")?;
                let (m, mult) = parse_mode(raw)?;
                mode = m;
                multiplier = mult;
                saw_time_flag = true;
            }
            "--args" => {
                i += 1;
                // Not through `flag_value`: the target's own command line is made of flags.
                let raw = argv.get(i).ok_or("--args needs a value")?;
                args = split_args(raw);
            }
            "--cwd" => {
                i += 1;
                let raw = flag_value(argv, i, "--cwd", "a folder")?;
                // An explicitly empty value is a usage error, not "no directory". The two mean
                // different things on the wire (absent vs present-and-empty) and only one of them is
                // something CreateProcessW can be given.
                if raw.trim().is_empty() {
                    return Err("--cwd needs a directory - omit the flag to start where the tool does".into());
                }
                cwd = Some(raw.to_string());
            }
            "--scale-duration" => {
                scale_duration = true;
                saw_time_flag = true;
            }
            flag if engine.take(flag) || switches.take(flag) => {}
            "--ticks" => {
                i += 1;
                let raw = flag_value(argv, i, "--ticks", "a number of heartbeats")?;
                ticks = digits_only(raw, format!("bad --ticks value '{raw}'"))?;
            }
            "--timeout" => {
                i += 1;
                let raw = flag_value(argv, i, "--timeout", "a value in seconds")?;
                let secs: u64 = digits_only(raw, format!("bad --timeout value '{raw}'"))?;
                if secs == 0 {
                    return Err("--timeout must be at least 1 second".into());
                }
                timeout_secs = Some(secs);
            }
            "--set-after" => {
                i += 1;
                let raw = flag_value(argv, i, "--set-after", "<tick>:<multiplier>")?;
                set_after = Some(parse_set_after(raw)?);
            }
            "--jump-after" => {
                i += 1;
                let raw = flag_value(argv, i, "--jump-after", "<tick>:<moment>")?;
                let (t, mom) = raw
                    .split_once(':')
                    .ok_or("--jump-after must be <tick>:<moment>")?;
                jump_after = Some((digits_only(t, format!("bad tick in '{raw}'"))?, mom.to_string()));
            }
            "--report" => {
                i += 1;
                report = Some(flag_value(argv, i, "--report", "a path for the evidence file")?.to_string());
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag '{other}'"));
            }
            other => {
                if target.is_none() {
                    target = Some(other.to_string());
                } else {
                    return Err(format!("unexpected argument '{other}'"));
                }
            }
        }
        i += 1;
    }

    check_combinations(preset.is_some(), saw_time_flag, !params.is_empty(), engine, switches.report_args && report.is_none())?;

    Ok(RunArgs {
        target: target.ok_or("missing <target>")?,
        args,
        cwd,
        at,
        zone_bias_min,
        mode,
        multiplier,
        scale_duration,
        scale_qpc: switches.scale_qpc,
        force: switches.force,
        embedded: engine.embedded,
        elevated_embedded: engine.elevated_embedded,
        keep_background_timers: engine.keep_background_timers,
        gecko_sandbox: engine.gecko_sandbox,
        dry_run: switches.dry_run,
        ticks,
        timeout_secs,
        set_after,
        jump_after,
        report,
        report_args: switches.report_args,
        json: switches.json,
        preset,
        params,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_flow_and_frozen_carry_no_multiplier() {
        assert_eq!(parse_mode("flow").unwrap(), (TimeModeKind::Flow, None));
        assert_eq!(parse_mode("frozen").unwrap(), (TimeModeKind::Frozen, None));
    }

    #[test]
    fn mode_accepts_xn_either_case() {
        assert_eq!(parse_mode("x60").unwrap(), (TimeModeKind::Multiplier, Some(60)));
        assert_eq!(parse_mode("X1440").unwrap(), (TimeModeKind::Multiplier, Some(1440)));
    }

    #[test]
    fn mode_rejects_unknown_and_nonpositive() {
        assert!(parse_mode("fast").is_err());
        assert!(parse_mode("x0").is_err());
        assert!(parse_mode("x-5").is_err());
        assert!(parse_mode("xabc").is_err());
        // Upper bound (R2-K2): the largest accepted rate passes, one past it does not. Above this
        // the fake clock leaves the representable range mid-session and the channels disagree.
        assert!(parse_mode(&format!("x{}", chrono_core::MULTIPLIER_MAX)).is_ok());
        assert!(parse_mode(&format!("x{}", chrono_core::MULTIPLIER_MAX + 1)).is_err());
    }

    /// `--report-args` says what goes into the evidence file, so without `--report` it is refused like
    /// `--param` without `--preset`, and it is off unless given (R5-D16).
    #[test]
    fn report_args_needs_a_report_to_write_into() {
        let argv = |s: &[&str]| s.iter().map(|a| a.to_string()).collect::<Vec<_>>();
        let err = parse_run_args(&argv(&["t", "--report-args"])).err().expect("refused without --report");
        assert!(err.contains("--report-args needs --report"), "{err}");
        assert!(parse_run_args(&argv(&["t", "--report", "e.txt", "--report-args"])).unwrap().report_args);
        assert!(!parse_run_args(&argv(&["t", "--report", "e.txt"])).unwrap().report_args, "on without being asked");
    }

    /// Each switch moved into `Switches` still lands in its own field, and none of them sets another.
    #[test]
    fn every_switch_sets_its_own_field_and_no_other() {
        let argv = |s: &[&str]| s.iter().map(|a| a.to_string()).collect::<Vec<_>>();
        let none = parse_run_args(&argv(&["t"])).unwrap();
        assert!(!(none.scale_qpc || none.force || none.dry_run || none.json || none.report_args));
        let fields = |ra: &RunArgs| [ra.scale_qpc, ra.force, ra.dry_run, ra.json];
        for (i, flag) in ["--scale-qpc", "--force", "--dry-run", "--json"].into_iter().enumerate() {
            let set = fields(&parse_run_args(&argv(&["t", flag])).unwrap());
            let mut want = [false; 4];
            want[i] = true;
            assert_eq!(set, want, "{flag}");
        }
    }

    #[test]
    fn set_after_multiplier_must_be_at_least_one() {
        // The in-flight multiplier gets the same >= 1 rule as --mode xN, instead of silently
        // accepting a zero/negative that would freeze or reverse the wall clock.
        let ok = parse_run_args(&["t".into(), "--set-after".into(), "5:60".into()]).unwrap();
        assert_eq!(ok.set_after, Some((5, 60)));
        assert!(parse_run_args(&["t".into(), "--set-after".into(), "5:0".into()]).is_err());
        assert!(parse_run_args(&["t".into(), "--set-after".into(), "5:-3".into()]).is_err());
    }

    /// `--cwd` closes an asymmetry that lasted from the day the protocol was written: `TargetSpec.cwd`
    /// has always been on the wire and `chrono-mech` has always handed it to CreateProcessW, but no
    /// surface offered it - the CLI had `--args` and nothing else, and the driver hard-coded `cwd: None`.
    #[test]
    fn cwd_is_parsed_from_the_command_line() {
        let ok = parse_run_args(&["app.exe".into(), "--cwd".into(), r"C:\work".into()]).unwrap();
        assert_eq!(ok.cwd.as_deref(), Some(r"C:\work"));

        // Absent stays absent: "do not ask for a directory" is not the same as asking for one.
        let none = parse_run_args(&["app.exe".into()]).unwrap();
        assert_eq!(none.cwd, None);
    }

    /// The half the parser test does NOT cover. The driver used to write `cwd: None` inline, so a
    /// parser that read the flag perfectly still sent nothing - and reverting that line failed no
    /// test at all until this one existed.
    #[test]
    fn the_driver_puts_the_parsed_cwd_on_the_wire() {
        let ra = parse_run_args(&["app.exe".into(), "--cwd".into(), "C:/work".into()]).unwrap();
        assert_eq!(target_spec_for(&ra).cwd.as_deref(), Some("C:/work"));

        let bare = parse_run_args(&["app.exe".into()]).unwrap();
        assert_eq!(target_spec_for(&bare).cwd, None);
    }

    /// Reaching the web pages inside the application is on unless the tester opts out - the flag is
    /// the one thing the driver has to send, and a bare run carries the default to the wire.
    #[test]
    fn no_embedded_is_the_opt_out_and_the_default_reaches_the_pages() {
        let bare = parse_run_args(&["app.exe".into()]).unwrap();
        assert!(target_spec_for(&bare).embedded, "a bare run reaches the pages inside the app");

        let off = parse_run_args(&["app.exe".into(), "--no-embedded".into()]).unwrap();
        assert!(!off.embedded);
        assert!(!target_spec_for(&off).embedded, "the opt-out has to reach the wire");
    }

    /// The engine switches that change how the application behaves while hidden are off unless asked
    /// for, and the request reaches the wire (R4-N28).
    #[test]
    fn keep_background_timers_is_opt_in_and_reaches_the_wire() {
        let bare = parse_run_args(&["app.exe".into()]).unwrap();
        assert!(!bare.keep_background_timers);
        assert!(!target_spec_for(&bare).keep_background_timers);
        let on = parse_run_args(&["app.exe".into(), "--keep-background-timers".into()]).unwrap();
        assert!(target_spec_for(&on).keep_background_timers, "the opt-in has to reach the wire");
        assert!(target_spec_for(&on).embedded, "and it does not turn the pages off");
    }

    /// The sandbox of a Gecko application is left on unless the tester switches it off, and the request
    /// reaches the wire (ADR-28). It asks nothing of the other engine switches.
    #[test]
    fn the_gecko_sandbox_is_switched_off_only_when_asked_and_reaches_the_wire() {
        let bare = parse_run_args(&["app.exe".into()]).unwrap();
        assert!(bare.gecko_sandbox && target_spec_for(&bare).gecko_sandbox);
        let off = parse_run_args(&["app.exe".into(), "--no-gecko-sandbox".into()]).unwrap();
        assert!(!target_spec_for(&off).gecko_sandbox, "the opt-in has to reach the wire");
        let spec = target_spec_for(&off);
        assert!(spec.embedded && !spec.elevated_embedded && !spec.keep_background_timers, "and it changes no other switch");
    }

    /// The option that writes the machine registry is off unless asked for, says so on the wire, and
    /// cannot be asked for together with the flag that leaves the pages alone.
    #[test]
    fn elevated_embedded_is_opt_in_and_reaches_the_wire() {
        let bare = parse_run_args(&["app.exe".into()]).unwrap();
        assert!(!bare.elevated_embedded, "a bare run writes nothing to the registry");
        assert!(!target_spec_for(&bare).elevated_embedded);

        let on = parse_run_args(&["app.exe".into(), "--elevated-embedded".into()]).unwrap();
        assert!(on.elevated_embedded);
        assert!(target_spec_for(&on).elevated_embedded, "the opt-in has to reach the wire");
        assert!(target_spec_for(&on).embedded, "and it does not turn the channel off");

        let both = parse_run_args(&["app.exe".into(), "--elevated-embedded".into(), "--no-embedded".into()]);
        assert!(both.err().unwrap().contains("cannot be combined"), "either order is a contradiction");
        assert!(parse_run_args(&["app.exe".into(), "--no-embedded".into(), "--elevated-embedded".into()]).is_err());
    }

    /// A shell that is not an administrator is refused, a dry run included, and one whose token could not
    /// be read is treated as not being one. Without the option nothing is asked of the token at all.
    #[test]
    fn elevated_embedded_is_refused_unless_the_shell_is_known_to_be_elevated() {
        let on = parse_run_args(&["app.exe".into(), "--elevated-embedded".into()]).unwrap();
        assert!(elevation_problem(&on, Some(true)).is_none());
        assert!(elevation_problem(&on, Some(false)).is_some());
        assert!(elevation_problem(&on, None).is_some(), "unknown is not elevated");
        let off = parse_run_args(&["app.exe".into()]).unwrap();
        for token in [Some(true), Some(false), None] {
            assert!(elevation_problem(&off, token).is_none(), "without the option there is nothing to refuse");
        }
    }

    /// An explicitly empty value is a usage error rather than a quiet "no directory". The two are
    /// different on the wire (absent vs present-and-empty), and only absent is something the
    /// mechanism can act on - CreateProcessW cannot be given an empty directory.
    #[test]
    fn an_empty_cwd_is_refused_rather_than_read_as_no_directory() {
        assert!(parse_run_args(&["app.exe".into(), "--cwd".into(), String::new()]).is_err());
        assert!(parse_run_args(&["app.exe".into(), "--cwd".into(), "   ".into()]).is_err());
        assert!(parse_run_args(&["app.exe".into(), "--cwd".into()]).is_err());
    }

    #[test]
    fn args_split_plain_and_quoted() {
        let s = |x: &str| x.to_string();
        // Plain space-separated values behave exactly as before (backward compatible).
        assert_eq!(split_args("a b c"), vec![s("a"), s("b"), s("c")]);
        assert_eq!(split_args("  x   y "), vec![s("x"), s("y")]);
        assert_eq!(split_args("out 100 10"), vec![s("out"), s("100"), s("10")]);
        // Double-quotes group a single argument that contains spaces (the P9 fix).
        assert_eq!(split_args("\"a b\" c"), vec![s("a b"), s("c")]);
        assert!(split_args("").is_empty());
        assert_eq!(split_args("\"\""), vec![s("")]); // an explicit empty argument
    }

    /// `run --preset` supplies the moment and mode, so combining it with a time flag is a usage
    /// error - alone (with a target) it parses and carries the id.
    #[test]
    fn run_preset_flag_is_exclusive_of_time_flags() {
        let ok = parse_run_args(&["--preset".into(), "month-end".into(), "app.exe".into()]).unwrap();
        assert_eq!(ok.preset.as_deref(), Some("month-end"));
        assert_eq!(ok.target, "app.exe");
        assert!(parse_run_args(&["--preset".into(), "m".into(), "--at".into(), "2020-01-01T00:00:00".into(), "app.exe".into()]).is_err());
        assert!(parse_run_args(&["--preset".into(), "m".into(), "--mode".into(), "x60".into(), "app.exe".into()]).is_err());
        assert!(parse_run_args(&["--preset".into(), "m".into(), "--scale-duration".into(), "app.exe".into()]).is_err());
    }

    /// --param in run needs --preset too.
    #[test]
    fn run_param_needs_preset() {
        assert!(parse_run_args(&["--param".into(), "start_date=2026-01-01".into(), "app.exe".into()]).is_err());
        let ok = parse_run_args(&[
            "--preset".into(),
            "trial-first-day-after".into(),
            "--param".into(),
            "start_date=2026-01-01".into(),
            "app.exe".into(),
        ])
        .unwrap();
        assert_eq!(ok.params.get("start_date").map(String::as_str), Some("2026-01-01"));
    }

    fn words(s: &[&str]) -> Vec<String> {
        s.iter().map(|w| w.to_string()).collect()
    }

    /// The refusal of a command line that must be refused.
    fn refusal(s: &[&str]) -> String {
        match parse_run_args(&words(s)) {
            Err(e) => e,
            Ok(_) => panic!("accepted {s:?}"),
        }
    }

    /// A flag that takes a value no longer swallows the flag after it (R5-P16): `--report --dry-run`
    /// ran the target and wrote the evidence into a file named `--dry-run`, and `--report
    /// --no-embedded` dropped the opt-out. The refusal names the flag it would have taken.
    #[test]
    fn a_value_flag_refuses_the_next_flag_as_its_value() {
        for flag in ["--report", "--at", "--preset", "--zone", "--mode", "--cwd", "--ticks", "--timeout", "--set-after", "--jump-after"] {
            let err = refusal(&["app.exe", flag, "--dry-run"]);
            assert!(err.contains(&format!("{flag} needs")) && err.contains("'--dry-run'"), "{flag}: {err}");
        }
        let err = refusal(&["app.exe", "--report", "--no-embedded"]);
        assert!(err.contains("'--no-embedded'"), "{err}");
        let err = refusal(&["--param", "--json", "--preset", "m", "app.exe"]);
        assert!(err.contains("--param needs") && err.contains("'--json'"), "{err}");
    }

    /// `--args` is the one value made of flags - the target's own command line - and keeps taking them.
    /// A folder or path that starts with two dashes is still reachable, written from the current folder.
    #[test]
    fn the_target_arguments_and_a_dotted_path_still_pass() {
        let ra = parse_run_args(&words(&["app.exe", "--args", "--help --verbose", "--dry-run"])).unwrap();
        assert_eq!(ra.args, vec!["--help", "--verbose"]);
        assert!(ra.dry_run, "the flag after the value is still read as a flag");
        let ra = parse_run_args(&words(&["app.exe", "--report", r".\--evidence.txt"])).unwrap();
        assert_eq!(ra.report.as_deref(), Some(r".\--evidence.txt"));
    }

    /// A flag that takes one value is refused when given twice, where the last one used to win in
    /// silence, and so is a parameter id given twice (R5-P16, the same rule `calc` has had since R4-N32).
    #[test]
    fn a_one_value_flag_or_a_parameter_given_twice_is_refused() {
        let err = refusal(&["app.exe", "--report", "a.txt", "--report", "b.txt"]);
        assert!(err.contains("--report is given twice"), "{err}");
        let err = refusal(&["--at", "2026-01-01T00:00:00", "app.exe", "--at", "2027-01-01T00:00:00"]);
        assert!(err.contains("--at is given twice"), "{err}");
        let err = refusal(&["--preset", "m", "--param", "a=1", "--param", "a=2", "app.exe"]);
        assert!(err.contains("--param a is given twice"), "{err}");
        let two = parse_run_args(&words(&["--preset", "m", "--param", "a=1", "--param", "b=2", "app.exe"])).unwrap();
        assert_eq!(two.params.len(), 2, "two different parameters are two values, not a repeat");
        // A value word is never read as a flag, so `--args` carrying `--report` repeats nothing.
        let ra = parse_run_args(&words(&["app.exe", "--args", "--report", "--report", "r.txt"])).unwrap();
        assert_eq!(ra.report.as_deref(), Some("r.txt"));
    }

    /// R5-D57. A number on the `run` command line is digits and nothing else, as it is everywhere else in
    /// the tool (R4-N34): a leading `+` was taken, so `--ticks +5` ran five heartbeats and `x+60` sixty
    /// times faster, while `calc` refused the same sign. Each refusal says why, and a number that is only
    /// too large is not told it has the wrong characters.
    #[test]
    fn a_sign_before_a_run_number_is_refused_as_calc_refuses_it() {
        let refused = |words: &[&str]| -> String {
            let line: Vec<String> = ["app.exe"].iter().chain(words).map(|w| w.to_string()).collect();
            parse_run_args(&line).err().unwrap_or_else(|| panic!("{words:?} was taken"))
        };
        for words in [
            ["--ticks", "+5"], ["--timeout", "+30"], ["--mode", "x+60"], ["--set-after", "+3:60"], ["--set-after", "3:+60"],
            ["--jump-after", "+3:2026-01-01T00:00:00"], ["--mode", "x-5"], ["--ticks", " 5"], ["--ticks", ""],
        ] {
            let message = refused(&words);
            assert!(message.ends_with("(digits only)"), "{words:?}: {message}");
        }
        let too_large = refused(&["--ticks", "18446744073709551616"]);
        assert_eq!(too_large, "bad --ticks value '18446744073709551616'");
        // The digits themselves still run.
        let line: Vec<String> = ["app.exe", "--ticks", "05", "--mode", "x60", "--set-after", "3:60"].map(String::from).to_vec();
        let taken = parse_run_args(&line).unwrap();
        assert_eq!((taken.ticks, taken.multiplier, taken.set_after), (5, Some(60), Some((3, 60))));
    }

    mod properties {
        use super::*;
        use proptest::prelude::*;

        /// What one part of a `run` command line sets when the parser takes it.
        #[derive(Debug, Clone, PartialEq)]
        enum Sets {
            Target(String),
            At(String),
            Preset(String),
            Zone(i32),
            Mode(TimeModeKind, Option<i64>),
            Args(Vec<String>),
            Cwd(String),
            Ticks(u64),
            Timeout(u64),
            SetAfter(u64, i64),
            JumpAfter(u64, String),
            Report(String),
            Param(String, String),
            Switch(&'static str),
        }

        /// One part of a command line as typed: its words, and what it sets, `None` for a part the
        /// parser has to refuse - a value it does not take, a flag it does not know, a flag at the
        /// end with no value.
        #[derive(Debug, Clone)]
        struct Part {
            words: Vec<String>,
            sets: Option<Sets>,
        }

        /// A flag and one of its values, each value with what it sets or `None` when it is refused.
        fn valued(flag: &'static str, values: Vec<(String, Option<Sets>)>) -> impl Strategy<Value = Part> {
            prop::sample::select(values).prop_map(move |(value, sets)| Part { words: vec![flag.to_string(), value], sets })
        }

        fn values<const N: usize>(table: [(&str, Option<Sets>); N]) -> Vec<(String, Option<Sets>)> {
            table.into_iter().map(|(value, sets)| (value.to_string(), sets)).collect()
        }

        /// An argument as a target receives it: any text without a double quote, blanks of every
        /// kind inside it, a flag of the target's own, a path with spaces, nothing at all.
        fn argument() -> impl Strategy<Value = String> {
            let blank = prop_oneof![Just(" "), Just("\t"), Just("\u{a0}"), Just("\u{2028}"), Just("\u{3000}")];
            prop_oneof![
                4 => "[a-z0-9./=-]{1,8}",
                2 => ("[a-z]{1,4}", blank, "[a-z]{0,4}").prop_map(|(a, blank, b)| format!("{a}{blank}{b}")),
                1 => prop_oneof![Just("--help".to_string()), Just(r"C:\Program Files\x".to_string()), Just(String::new())],
            ]
        }

        /// The `--args` value written from `args` the way the flag documents: an argument that is empty
        /// or holds a blank goes inside double quotes, and `between` stands between two of them.
        fn written(args: &[String], between: &str) -> String {
            let quoted = |a: &String| if a.is_empty() || a.chars().any(char::is_whitespace) { format!("\"{a}\"") } else { a.clone() };
            args.iter().map(quoted).collect::<Vec<_>>().join(between)
        }

        fn between() -> impl Strategy<Value = &'static str> {
            prop_oneof![4 => Just(" "), 1 => Just("  "), 1 => Just("\t"), 1 => Just(" \t ")]
        }

        /// Any one part of a `run` command line: every flag with values it takes and values it
        /// refuses, drawn by name at the edges (the largest multiplier and one past it, a tick count
        /// past 64 bits, a zone a minute past the range), every switch, flags it does not know, and
        /// a target, an ordinary one and one written with a single dash.
        fn part() -> impl Strategy<Value = Part> {
            let max = chrono_core::MULTIPLIER_MAX;
            let switches = prop::sample::select(vec![
                "--scale-duration", "--no-embedded", "--elevated-embedded", "--keep-background-timers", "--scale-qpc",
                "--force", "--dry-run", "--json", "--report-args", "--no-gecko-sandbox",
            ])
            .prop_map(|flag| Part { words: vec![flag.to_string()], sets: Some(Sets::Switch(flag)) });
            let unknown = prop::sample::select(vec!["--AT", "--at=2026-01-01T00:00:00", "--unknown", "--"])
                .prop_map(|flag| Part { words: vec![flag.to_string()], sets: None });
            let target = prop::sample::select(vec!["app.exe", r"C:\Program Files\a b.exe", "-x"])
                .prop_map(|t| Part { words: vec![t.to_string()], sets: Some(Sets::Target(t.to_string())) });
            let args = (prop::collection::vec(argument(), 0..4), between()).prop_map(|(args, between)| Part {
                words: vec!["--args".to_string(), written(&args, between)],
                sets: Some(Sets::Args(args)),
            });
            let at = |v: &str| Some(Sets::At(v.to_string()));
            let mode = |m: TimeModeKind, n: Option<i64>| Some(Sets::Mode(m, n));
            let flags = prop_oneof![
                valued("--at", values([("2026-01-31T00:00:00", at("2026-01-31T00:00:00")), ("+3d", at("+3d")), ("--3d", None)])),
                valued("--preset", values([("month-end", Some(Sets::Preset("month-end".into()))), ("--x", None)])),
                valued("--zone", values([
                    ("+02:00", Some(Sets::Zone(-120))), ("-05:30", Some(Sets::Zone(330))), ("+14:59", Some(Sets::Zone(-899))),
                    ("+15:00", None), ("+05:+30", None), ("abc", None),
                ])),
                valued("--mode", vec![
                    ("flow".into(), mode(TimeModeKind::Flow, None)), ("frozen".into(), mode(TimeModeKind::Frozen, None)),
                    ("x60".into(), mode(TimeModeKind::Multiplier, Some(60))), ("X2".into(), mode(TimeModeKind::Multiplier, Some(2))),
                    (format!("x{max}"), mode(TimeModeKind::Multiplier, Some(max))), (format!("x{}", max + 1), None),
                    ("x0".into(), None), ("x-5".into(), None), ("x+60".into(), None), ("fast".into(), None), ("x".into(), None),
                ]),
                args,
                valued("--cwd", values([
                    ("work", Some(Sets::Cwd("work".into()))), (r"C:\work dir", Some(Sets::Cwd(r"C:\work dir".into()))),
                    ("", None), ("   ", None), ("--x", None),
                ])),
                valued("--ticks", vec![
                    ("0".into(), Some(Sets::Ticks(0))), ("5".into(), Some(Sets::Ticks(5))),
                    (u64::MAX.to_string(), Some(Sets::Ticks(u64::MAX))), ((u128::from(u64::MAX) + 1).to_string(), None),
                    ("-1".into(), None), ("+5".into(), None), ("x".into(), None),
                ]),
                valued("--timeout", values([
                    ("1", Some(Sets::Timeout(1))), ("30", Some(Sets::Timeout(30))), ("0", None), ("-1", None), ("+30", None),
                ])),
                valued("--set-after", vec![
                    ("3:60".into(), Some(Sets::SetAfter(3, 60))), ("0:1".into(), Some(Sets::SetAfter(0, 1))),
                    (format!("1:{max}"), Some(Sets::SetAfter(1, max))), (format!("1:{}", max + 1), None),
                    ("3:0".into(), None), ("3".into(), None), ("x:60".into(), None), ("+3:60".into(), None), ("3:+60".into(), None),
                ]),
                valued("--jump-after", values([
                    ("3:2026-01-01T00:00:00", Some(Sets::JumpAfter(3, "2026-01-01T00:00:00".into()))),
                    ("0:+1d", Some(Sets::JumpAfter(0, "+1d".into()))), ("3", None), ("x:2026", None), ("+3:+1d", None),
                ])),
                valued("--report", values([("out.md", Some(Sets::Report("out.md".into()))), ("--x", None)])),
                valued("--param", values([
                    ("start_date=2026-01-01", Some(Sets::Param("start_date".into(), "2026-01-01".into()))),
                    ("trial_length=30days", Some(Sets::Param("trial_length".into(), "30days".into()))),
                    ("a==b", Some(Sets::Param("a".into(), "=b".into()))), ("novalue", None), ("=5", None),
                ])),
            ];
            prop_oneof![8 => flags, 3 => switches, 1 => unknown, 2 => target]
        }

        /// A whole command line: a target or not, any parts in any order, now and then one part
        /// given twice (R5-P16), and now and then a flag that takes a value standing last with none.
        fn command_line() -> impl Strategy<Value = Vec<Part>> {
            let target = Part { words: vec!["app.exe".into()], sets: Some(Sets::Target("app.exe".into())) };
            let bare = prop::sample::select(vec!["--at", "--zone", "--mode", "--args", "--report", "--param"])
                .prop_map(|flag| Part { words: vec![flag.to_string()], sets: None });
            (
                prop::collection::vec(part(), 0..7),
                prop::bool::weighted(0.85),
                prop::option::weighted(0.25, any::<prop::sample::Index>()),
                prop::option::weighted(0.05, bare),
            )
                .prop_flat_map(move |(mut parts, with_target, twice, bare)| {
                    if with_target {
                        parts.push(target.clone());
                    }
                    if let Some(index) = twice.filter(|_| !parts.is_empty()) {
                        parts.push(parts[index.index(parts.len())].clone());
                    }
                    (Just(parts).prop_shuffle(), Just(bare))
                })
                .prop_map(|(mut parts, bare)| {
                    parts.extend(bare);
                    parts
                })
        }

        /// The fields a parse is judged by, from the parser and from the model alike.
        #[derive(Debug, Clone, PartialEq)]
        struct Seen {
            target: String,
            args: Vec<String>,
            cwd: Option<String>,
            at: Option<String>,
            zone: Option<i32>,
            mode: TimeModeKind,
            multiplier: Option<i64>,
            ticks: u64,
            timeout: Option<u64>,
            set_after: Option<(u64, i64)>,
            jump_after: Option<(u64, String)>,
            report: Option<String>,
            preset: Option<String>,
            params: Vec<(String, String)>,
            /// scale-duration, embedded, elevated, background timers, scale-qpc, force, dry run, json, report args.
            switches: [bool; 10],
        }

        fn seen(ra: &RunArgs) -> Seen {
            let mut params: Vec<(String, String)> = ra.params.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            params.sort();
            Seen {
                target: ra.target.clone(),
                args: ra.args.clone(),
                cwd: ra.cwd.clone(),
                at: ra.at.clone(),
                zone: ra.zone_bias_min,
                mode: ra.mode,
                multiplier: ra.multiplier,
                ticks: ra.ticks,
                timeout: ra.timeout_secs,
                set_after: ra.set_after,
                jump_after: ra.jump_after.clone(),
                report: ra.report.clone(),
                preset: ra.preset.clone(),
                params,
                switches: [
                    ra.scale_duration, ra.embedded, ra.elevated_embedded, ra.keep_background_timers, ra.scale_qpc, ra.force,
                    ra.dry_run, ra.json, ra.report_args, ra.gecko_sandbox,
                ],
            }
        }

        /// What the documented rules of `run` make of the parts: every part taken, a flag that takes
        /// one value given once (`--param` once per id), one target, and none of the pairs that
        /// contradict each other. `None` when the line has to be refused.
        fn owed(parts: &[Part]) -> Option<Seen> {
            let mut owed = Seen {
                target: String::new(), args: Vec::new(), cwd: None, at: None, zone: None, mode: TimeModeKind::Flow, multiplier: None,
                ticks: 0, timeout: None, set_after: None, jump_after: None, report: None, preset: None, params: Vec::new(),
                switches: [false, true, false, false, false, false, false, false, false, true],
            };
            let (mut targets, mut flags_once, mut time_flag) = (0, Vec::new(), false);
            for part in parts {
                // A flag with its value, other than `--param`, is a flag that takes one value.
                let word = part.words[0].as_str();
                if part.words.len() == 2 && word != "--param" {
                    if flags_once.contains(&word) {
                        return None;
                    }
                    flags_once.push(word);
                }
                match part.sets.clone()? {
                    Sets::Target(t) => (targets, owed.target) = (targets + 1, t),
                    Sets::At(at) => (owed.at, time_flag) = (Some(at), true),
                    Sets::Preset(id) => owed.preset = Some(id),
                    Sets::Zone(bias) => owed.zone = Some(bias),
                    Sets::Mode(mode, n) => (owed.mode, owed.multiplier, time_flag) = (mode, n, true),
                    Sets::Args(args) => owed.args = args,
                    Sets::Cwd(dir) => owed.cwd = Some(dir),
                    Sets::Ticks(n) => owed.ticks = n,
                    Sets::Timeout(n) => owed.timeout = Some(n),
                    Sets::SetAfter(tick, n) => owed.set_after = Some((tick, n)),
                    Sets::JumpAfter(tick, at) => owed.jump_after = Some((tick, at)),
                    Sets::Report(path) => owed.report = Some(path),
                    Sets::Param(id, value) => {
                        if owed.params.iter().any(|(seen, _)| *seen == id) {
                            return None;
                        }
                        owed.params.push((id, value));
                    }
                    Sets::Switch(flag) => {
                        let at = ["--scale-duration", "--no-embedded", "--elevated-embedded", "--keep-background-timers",
                            "--scale-qpc", "--force", "--dry-run", "--json", "--report-args", "--no-gecko-sandbox"]
                            .iter()
                            .position(|f| *f == flag)?;
                        // A `--no-` switch turns off what is on by default.
                        owed.switches[at] = !flag.starts_with("--no-");
                        time_flag |= flag == "--scale-duration";
                    }
                }
            }
            owed.params.sort();
            let [_, embedded, elevated, _, _, _, _, _, report_args, _] = owed.switches;
            let contradiction = (owed.preset.is_some() && time_flag)
                || (!owed.params.is_empty() && owed.preset.is_none())
                || (elevated && !embedded)
                || (report_args && owed.report.is_none());
            (targets == 1 && !contradiction).then_some(owed)
        }

        proptest! {
            /// R5-P16. A `run` command line is taken exactly when the documented rules take it, with
            /// every field what the line says: each part a flag takes or refuses, a flag that takes
            /// one value given once, one target, and no pair of flags that contradict each other.
            #[test]
            fn a_run_command_line_is_taken_exactly_when_its_rules_take_it(parts in command_line()) {
                let line: Vec<String> = parts.iter().flat_map(|part| part.words.clone()).collect();
                let parsed = parse_run_args(&line);
                prop_assert_eq!(parsed.as_ref().ok().map(seen), owed(&parts), "{:?} gave {:?}", line, parsed.as_ref().err());
            }

            /// An `--args` value splits into the arguments it was written from: an argument with any
            /// blank in it, or empty, inside double quotes, and blanks of any width between them.
            #[test]
            fn an_args_value_splits_into_the_arguments_it_was_written_from(
                args in prop::collection::vec(argument(), 0..6),
                between in between(),
            ) {
                let value = written(&args, between);
                prop_assert_eq!(split_args(&value), args, "{:?}", value);
            }
        }
    }
}
