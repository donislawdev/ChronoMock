//! `chrono run --dry-run`: what the session would be, worked out without starting anything.
//!
//! The tool's ordinary act is to launch someone else's application and inject a library into it.
//! There was no way to see what that session would be without performing it, and for a preset there
//! still is no other way to learn which date it lands on - `trial-first-day-after` counts from the
//! target's own file date, so the answer is not in the command line.
//!
//! Everything here is read off values that were already decided: the `TimeSpec` that would have gone
//! on the wire, the `TimeOrigin` the resolver produced beside it, and the same two functions the core
//! itself uses to pick a mechanism and to fingerprint a target. Nothing is resolved a second time,
//! because a second resolution is free to disagree with the one that would have run.
//!
//! What it refuses to claim is as much of the point as what it prints. A dry run has no verdict and
//! says so, and it never guesses a target's bitness - a .NET AnyCPU image lies in its own header,
//! which is why `chrono-mech` asks the running process instead.
//!
//! Where it does answer, the answer is measured. The first draft of this file assumed Windows would
//! resolve a bare target name through PATH and so declined to judge one. It does not: the target
//! goes to `CreateProcessW` as `lpApplicationName`, which never searches PATH, and the canary in
//! `tests/dry_run.rs` failed on `cmd.exe` until this said so.

use std::path::{Path, PathBuf};

use chrono_proto::TimeSpec;

use super::args::RunArgs;
use super::moment::TimeOrigin;
use crate::cdp;
use crate::cli::this_bitness;
use crate::report::{describe_warning, detect_runtime_warnings, mode_label};
use crate::zone::format_bias;

pub(crate) const PLAN_SCHEMA: &str = "chronomock.plan/1";

/// Width of the label column in the human plan. One place, so the block stays a block.
const LABEL: usize = 12;

/// What the filesystem says about the target, and nothing beyond it.
///
/// The states follow the way each mechanism actually resolves a target, which was measured rather
/// than assumed - the first draft of this file had it backwards.
///
/// The native mechanism hands the target to `CreateProcessW` as `lpApplicationName`, and Microsoft
/// documents that parameter as resolving against the current drive and directory only: "The function
/// will not use the search path. This parameter must include the file name extension - no default
/// extension is assumed."
/// (<https://learn.microsoft.com/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessw>)
/// Measured to match on this machine: `chrono run notepad` exits 2 with `target.launch_failed`,
/// though notepad.exe is on PATH twice over. So a name that is not a file here is not a file for the
/// session either, whether or not it looks like a path.
///
/// The Chromium path is the one exception, and only for a bare name. It launches through Rust's
/// `Command`, which is documented to search PATH in an OS-defined way when the program is not an
/// absolute path, so a bare name there is a question this cannot answer and does not try to.
#[derive(Debug, PartialEq, Eq)]
enum TargetPath {
    /// A file is there, at the absolute path the session would use.
    Found(PathBuf),
    /// No file of that name, and the mechanism that would run it does not search anywhere else.
    Missing,
    /// A bare name on the Chromium path, which resolves it through PATH itself.
    Unchecked,
}

impl TargetPath {
    /// The stable token the machine surface carries.
    fn key(&self) -> &'static str {
        match self {
            TargetPath::Found(_) => "found",
            TargetPath::Missing => "missing",
            TargetPath::Unchecked => "unchecked",
        }
    }
}

/// Whether the target names a directory as well as a file, which decides nothing about existence and
/// everything about who resolves it.
fn is_bare_name(target: &str) -> bool {
    !target.contains('\\') && !target.contains('/')
}

/// Look at the target without touching it. `chromium` is the mechanism the core would choose, which
/// is what decides who resolves the name.
fn inspect_target(target: &str, chromium: bool) -> TargetPath {
    let path = Path::new(target);
    if path.is_file() {
        // The absolute path, so the plan names the file the session would open rather than whatever
        // the shell's current directory made of it.
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        return TargetPath::Found(readable(canonical));
    }
    if chromium && is_bare_name(target) {
        TargetPath::Unchecked
    } else {
        TargetPath::Missing
    }
}

/// The canonical path without the extended-length prefix Windows answers `canonicalize` with. That
/// prefix is correct and unreadable, and this line is read by a person first - `\\?\C:\app.exe` names
/// the same file as `C:\app.exe`.
///
/// Only a plain drive path is unwrapped. The same prefix also introduces a UNC path as `\\?\UNC\host\
/// share`, where dropping four characters would leave a path that names nothing, so that one is left
/// exactly as Windows gave it.
fn readable(path: PathBuf) -> PathBuf {
    let Some(rest) = path.to_str().and_then(|s| s.strip_prefix(r"\\?\")) else {
        return path;
    };
    let mut chars = rest.chars();
    let drive_path = matches!(
        (chars.next(), chars.next(), chars.next()),
        (Some(letter), Some(':'), Some('\\')) if letter.is_ascii_alphabetic()
    );
    if drive_path { PathBuf::from(rest) } else { path }
}

/// Everything a dry run says, held by reference to the values that decided it.
struct Plan<'a> {
    ra: &'a RunArgs,
    spec: &'a TimeSpec,
    origin: &'a TimeOrigin,
    target: TargetPath,
    /// Whether the core would take the Chromium path, from the same function the core calls (ADR-9).
    chromium: bool,
    /// The warning keys the core would emit for this target and these flags, from the same detector.
    warning_keys: Vec<String>,
    /// The zone the session would actually run in, whether or not `--zone` named it.
    zone_bias_min: i32,
}

/// Print the plan and start nothing. Exit 0, except for a path that definitely leads to no file,
/// which exits 2 - the code a real run would give for the same fact (docs/08 section 8).
pub(super) fn dry_run(ra: &RunArgs, spec: &TimeSpec, origin: &TimeOrigin, now_bias: i32) -> i32 {
    // The same pure function the core calls, so the plan names the mechanism the core would choose
    // and not one worked out a second way (ADR-9).
    let chromium = cdp::is_chromium_target(&ra.target);
    let plan = Plan {
        ra,
        spec,
        origin,
        chromium,
        target: inspect_target(&ra.target, chromium),
        warning_keys: detect_runtime_warnings(Path::new(&ra.target), ra.scale_qpc),
        zone_bias_min: ra.zone_bias_min.unwrap_or(now_bias),
    };

    if ra.json {
        println!("{}", render_json(&plan));
    } else {
        print!("{}", render_plan(&plan));
    }

    if plan.target == TargetPath::Missing {
        // On stderr, where the rest of the driver's diagnostics go, so the plan on stdout stays one
        // clean document even when the run it describes could not happen.
        eprintln!(
            "chrono: there is no file named '{}' here, so this command would not start anything - a target is resolved as a path, never through PATH",
            ra.target
        );
        return 2;
    }
    0
}

fn line(label: &str, value: &str) -> String {
    format!("  {label:<LABEL$}{value}\n")
}

/// A continuation under a label, for a second thing to say about the same row.
fn note(value: &str) -> String {
    format!("  {:<LABEL$}{value}\n", "")
}

/// What the plan says about the target itself.
fn target_block(p: &Plan) -> String {
    let mut out = String::new();
    match &p.target {
        TargetPath::Found(path) => out.push_str(&line("target", &path.display().to_string())),
        TargetPath::Missing => {
            out.push_str(&line("target", &p.ra.target));
            out.push_str(&note("there is no file here by that name"));
            if is_bare_name(&p.ra.target) {
                // Worth spelling out, because this is the case a reader expects to work: a target is
                // resolved as a path and never through PATH, so a bare name means "in this folder".
                out.push_str(&note("a target is resolved as a path, never through PATH - give the full path"));
            }
        }
        TargetPath::Unchecked => {
            out.push_str(&line("target", &p.ra.target));
            out.push_str(&note("a bare name on the Chromium path, which resolves it through PATH itself - not checked here"));
        }
    }
    out.push_str(&line(
        "arguments",
        &if p.ra.args.is_empty() { "(none)".to_string() } else { p.ra.args.join(" | ") },
    ));
    out.push_str(&line(
        "directory",
        p.ra.cwd.as_deref().unwrap_or("inherited from this shell"),
    ));
    out.push_str(&line("mechanism", &mechanism_text(p)));
    out
}

/// Which of the two mechanisms the core would take, and why.
fn mechanism_text(p: &Plan) -> String {
    if p.target == TargetPath::Missing {
        return "not decided - a mechanism is chosen from the target's own folder".to_string();
    }
    if p.chromium {
        return "Chromium or Electron over CDP - the folder carries the Chromium runtime".to_string();
    }
    // The bitness named here is THIS executable's, never the target's. The target's is read from the
    // running process by chrono-mech, because a .NET AnyCPU image carries IMAGE_FILE_MACHINE_I386 in
    // its header and runs 64-bit anyway - a plan that read the header would refuse targets that work.
    format!(
        "native injection, from the {} core - a 32-bit target needs the other build",
        this_bitness()
    )
}

/// What the plan says about time: the moment, the zone it is read in, where it came from, the mode.
fn time_block(p: &Plan) -> String {
    let mut out = String::new();
    let zone = match p.ra.zone_bias_min {
        Some(bias) => format_bias(bias),
        None => format!("{} (host default)", format_bias(p.zone_bias_min)),
    };
    let moment = p.spec.moment.local.as_deref().unwrap_or("(none)");
    out.push_str(&line("moment", &format!("{moment} in {zone}")));
    out.push_str(&origin_block(p.origin));

    let mut mode = mode_label(&p.spec.mode, p.spec.multiplier);
    if p.spec.scale_duration {
        mode.push_str(", duration clocks scaled");
    }
    if p.spec.scale_qpc {
        mode.push_str(", high-resolution counter scaled");
    }
    out.push_str(&line("mode", &mode));
    out
}

/// Where that moment came from. For a preset this is the half the resolved moment cannot show.
fn origin_block(origin: &TimeOrigin) -> String {
    match origin {
        TimeOrigin::At(raw) => note(&format!("from --at {raw}")),
        TimeOrigin::Now => note("from the real current time, since neither --at nor --preset was given"),
        TimeOrigin::Preset { id, parameters } => {
            let mut out = note(&format!("from preset {id}"));
            for (param, value, source) in parameters {
                out.push_str(&note(&format!("  {param} = {value}, from {source}")));
            }
            out
        }
    }
}

/// What the plan says about the shape of the session: how it would end, and what would happen along
/// the way.
fn session_block(p: &Plan) -> String {
    let mut out = String::new();
    out.push_str(&line(
        "session",
        &match p.ra.ticks {
            0 => "runs until the target exits".to_string(),
            n => format!("ends after {n} state heartbeats, about {n} real seconds"),
        },
    ));
    if let Some(secs) = p.ra.timeout_secs {
        out.push_str(&note(&format!("gives up after {secs}s and exits 6, with no verdict")));
    }
    if let Some((tick, m)) = p.ra.set_after {
        out.push_str(&note(&format!("at heartbeat {tick}, changes the rate to x{m}")));
    }
    if let Some((tick, moment)) = &p.ra.jump_after {
        out.push_str(&note(&format!("at heartbeat {tick}, jumps the clock to {moment}")));
    }
    if p.ra.force {
        out.push_str(&note("--force: carries on even if the opening verdict says the substitution did not take effect"));
    }
    if let Some(path) = &p.ra.report {
        out.push_str(&line("evidence", &format!("would be written to {path}")));
    }
    out
}

/// The whole plan as a person reads it.
fn render_plan(p: &Plan) -> String {
    let mut out = String::from("chrono run - dry run. Nothing was started and nothing was written.\n\n");
    out.push_str(&target_block(p));
    out.push_str(&time_block(p));
    out.push_str(&session_block(p));

    if !p.warning_keys.is_empty() {
        out.push_str("\nWhat is already known about this target:\n");
        for key in &p.warning_keys {
            out.push_str(&format!("  - {}\n", describe_warning(key)));
        }
    }

    // The last word, and the one that keeps this from reading like a result. A plan is a description
    // of a command line - the verdict is a fact about a target, and only a real session has one
    // (untouchable rule 4).
    out.push_str(
        "\nA dry run resolves the command line and stops there. It started no process, wrote no file,\n\
         and proves nothing about the target - only a real session carries a verdict.\n",
    );
    out
}

// ---------------------------------------------------------------------------------------------
// The machine surface: chronomock.plan/1
// ---------------------------------------------------------------------------------------------
//
// Same envelope shape as chronomock.calc/1 next door - a schema field first, then the document. The
// keys are public names (rule 17), so a change to their meaning needs a schema version.

#[derive(serde::Serialize)]
struct PlanJson<'a> {
    schema: &'static str,
    /// Always false. The one field a pipeline needs in order not to read this as a result.
    started: bool,
    target: TargetJson<'a>,
    mechanism: &'static str,
    /// The bitness of THIS core, not of the target - the target's is only knowable once it runs.
    core_bitness: &'static str,
    time: TimeJson<'a>,
    origin: OriginJson<'a>,
    session: SessionJson<'a>,
    /// Warning keys, not prose. The core emits keys and each consumer renders its own language
    /// (rule 15), and this surface has consumers that are not people.
    warnings: &'a [String],
}

#[derive(serde::Serialize)]
struct TargetJson<'a> {
    path: &'a str,
    /// The absolute path, when there is a file to point at.
    resolved: Option<String>,
    /// `found` / `missing` / `unchecked`.
    state: &'static str,
    args: &'a [String],
    cwd: Option<&'a str>,
}

#[derive(serde::Serialize)]
struct TimeJson<'a> {
    moment: Option<&'a str>,
    /// Session-zone bias in minutes, on the protocol's convention: UTC = local + bias, so UTC+02:00
    /// is -120. The same sign the wire and `chronomock.calc/1` use, which is the point of repeating
    /// it here - the human line beside it reads "+02:00" and the two must not be taken for one value.
    zone_bias_min: i32,
    /// Whether `--zone` named that bias, as against it being this machine's.
    zone_from_flag: bool,
    mode: &'a str,
    multiplier: Option<i64>,
    scale_duration: bool,
    scale_qpc: bool,
}

#[derive(serde::Serialize)]
struct OriginJson<'a> {
    /// `at` / `now` / `preset`.
    kind: &'static str,
    /// The `--at` value as typed, for the `at` kind.
    at: Option<&'a str>,
    preset: Option<&'a str>,
    parameters: Vec<ParameterJson<'a>>,
}

#[derive(serde::Serialize)]
struct ParameterJson<'a> {
    id: &'a str,
    value: &'a str,
    source: &'a str,
}

#[derive(serde::Serialize)]
struct SessionJson<'a> {
    ticks: u64,
    timeout_secs: Option<u64>,
    set_after: Option<[i64; 2]>,
    jump_after: Option<(u64, &'a str)>,
    force: bool,
    /// The path `--report` named. A dry run does not write it.
    report: Option<&'a str>,
}

fn origin_json(origin: &TimeOrigin) -> OriginJson<'_> {
    match origin {
        TimeOrigin::At(raw) => {
            OriginJson { kind: "at", at: Some(raw), preset: None, parameters: Vec::new() }
        }
        TimeOrigin::Now => OriginJson { kind: "now", at: None, preset: None, parameters: Vec::new() },
        TimeOrigin::Preset { id, parameters } => OriginJson {
            kind: "preset",
            at: None,
            preset: Some(id),
            parameters: parameters
                .iter()
                .map(|(param, value, source)| ParameterJson { id: param, value, source })
                .collect(),
        },
    }
}

fn render_json(p: &Plan) -> String {
    let doc = PlanJson {
        schema: PLAN_SCHEMA,
        started: false,
        target: TargetJson {
            path: &p.ra.target,
            resolved: match &p.target {
                TargetPath::Found(path) => Some(path.display().to_string()),
                _ => None,
            },
            state: p.target.key(),
            args: &p.ra.args,
            cwd: p.ra.cwd.as_deref(),
        },
        mechanism: if p.chromium { "chromium-cdp" } else { "native" },
        core_bitness: this_bitness(),
        time: TimeJson {
            moment: p.spec.moment.local.as_deref(),
            zone_bias_min: p.zone_bias_min,
            zone_from_flag: p.ra.zone_bias_min.is_some(),
            mode: &p.spec.mode,
            multiplier: p.spec.multiplier,
            scale_duration: p.spec.scale_duration,
            scale_qpc: p.spec.scale_qpc,
        },
        origin: origin_json(p.origin),
        session: SessionJson {
            ticks: p.ra.ticks,
            timeout_secs: p.ra.timeout_secs,
            set_after: p.ra.set_after.map(|(tick, m)| [tick as i64, m]),
            jump_after: p.ra.jump_after.as_ref().map(|(tick, moment)| (*tick, moment.as_str())),
            force: p.ra.force,
            report: p.ra.report.as_deref(),
        },
        warnings: &p.warning_keys,
    };
    serde_json::to_string(&doc).unwrap_or_else(|_| format!("{{\"schema\":\"{PLAN_SCHEMA}\"}}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::args::parse_run_args;
    use crate::run::moment::resolve_time_spec;

    /// Build a plan the way `dry_run` does, from a real command line, so the tests exercise the same
    /// path the flag takes rather than a hand-built struct that could describe nothing.
    fn plan_for(argv: &[&str], f: impl FnOnce(&Plan)) {
        let owned: Vec<String> = argv.iter().map(|a| (*a).to_string()).collect();
        let ra = parse_run_args(&owned).expect("the command line must parse");
        let resolved = resolve_time_spec(&ra, 120).expect("an absolute moment needs no catalogue");
        let plan = Plan {
            ra: &ra,
            spec: &resolved.spec,
            origin: &resolved.origin,
            target: inspect_target(&ra.target, cdp::is_chromium_target(&ra.target)),
            chromium: cdp::is_chromium_target(&ra.target),
            warning_keys: Vec::new(),
            zone_bias_min: ra.zone_bias_min.unwrap_or(120),
        };
        f(&plan);
    }

    /// The sentence the whole flag answers to. A plan that read like a result would be the tool
    /// claiming an outcome it never measured (untouchable rule 4), and this is the line that stops
    /// it - in both renderings, since a pipeline reads only one of them.
    #[test]
    fn the_plan_says_in_both_renderings_that_nothing_ran() {
        plan_for(&["app.exe", "--at", "2038-01-19T03:14:07"], |plan| {
            let text = render_plan(plan);
            assert!(text.contains("Nothing was started and nothing was written"), "{text}");
            assert!(text.contains("proves nothing about the target"), "{text}");
            assert!(!text.to_lowercase().contains("verdict:"), "a plan must not print a verdict: {text}");

            let json: serde_json::Value = serde_json::from_str(&render_json(plan)).expect("valid JSON");
            assert_eq!(json["schema"], "chronomock.plan/1");
            assert_eq!(json["started"], false, "the machine surface must say nothing ran");
        });
    }

    /// The moment and the mode are read off the very `TimeSpec` the core would have received, so the
    /// plan cannot describe a session other than the one the same command line would run.
    #[test]
    fn the_plan_describes_the_spec_that_would_have_gone_on_the_wire() {
        plan_for(
            &["app.exe", "--at", "2038-01-19T03:14:07", "--mode", "x60", "--scale-duration", "--zone", "+02:00"],
            |plan| {
                let text = render_plan(plan);
                assert!(text.contains("2038-01-19T03:14:07"), "{text}");
                assert!(text.contains("+02:00"), "{text}");
                assert!(text.contains("x60"), "{text}");
                assert!(text.contains("duration clocks scaled"), "{text}");
                assert!(text.contains("from --at 2038-01-19T03:14:07"), "{text}");

                let json: serde_json::Value = serde_json::from_str(&render_json(plan)).expect("valid JSON");
                assert_eq!(json["time"]["moment"], "2038-01-19T03:14:07");
                assert_eq!(json["time"]["mode"], "multiplier");
                assert_eq!(json["time"]["multiplier"], 60);
                assert_eq!(json["time"]["scale_duration"], true);
                assert_eq!(json["time"]["zone_from_flag"], true);
                assert_eq!(json["origin"]["kind"], "at");
            },
        );
    }

    /// With no moment flag at all the session starts at the real current time, and the plan says which
    /// of the three sources it was - a resolved absolute moment looks identical in all three.
    #[test]
    fn a_session_with_no_moment_flag_names_the_clock_as_its_source() {
        plan_for(&["app.exe"], |plan| {
            assert!(render_plan(plan).contains("from the real current time"), "{}", render_plan(plan));
            let json: serde_json::Value = serde_json::from_str(&render_json(plan)).expect("valid JSON");
            assert_eq!(json["origin"]["kind"], "now");
        });
    }

    /// A name that is not a file here is not a file for the session either, path or no path.
    ///
    /// Measured against the first draft of this module, which assumed Windows would resolve a bare
    /// name through PATH and so declined to judge one. It does not: the native mechanism passes the
    /// target to CreateProcessW as lpApplicationName, which Microsoft documents as never using the
    /// search path, and `chrono run notepad` exits 2 on this machine with notepad.exe on PATH twice.
    #[test]
    fn a_name_that_is_not_a_file_here_is_missing_whether_or_not_it_looks_like_a_path() {
        assert_eq!(inspect_target(r"C:\definitely\not\here\nothing.exe", false), TargetPath::Missing);
        assert_eq!(inspect_target("./neither/is/this.exe", false), TargetPath::Missing);
        assert_eq!(
            inspect_target("notepad", false),
            TargetPath::Missing,
            "notepad.exe is on PATH, and the native mechanism does not look there"
        );

        // The one exception, and the reason the third state survives: the Chromium launcher goes
        // through Rust's Command, which does search PATH for a bare name. A path is still a path
        // there, so only the bare name is left unjudged.
        assert_eq!(inspect_target("some-electron-app.exe", true), TargetPath::Unchecked);
        assert_eq!(inspect_target(r"C:\nowhere\some-electron-app.exe", true), TargetPath::Missing);

        let exe = std::env::current_exe().expect("this test has an executable");
        let found = inspect_target(&exe.display().to_string(), false);
        let TargetPath::Found(path) = &found else {
            panic!("a file that is there must be found, got {found:?}");
        };
        // The extended-length prefix is stripped from a drive path, because the plan is read by a
        // person and `\\?\C:\...` is the same file spelled unreadably.
        assert!(!path.display().to_string().starts_with(r"\\?\"), "{}", path.display());

        // A UNC path keeps its prefix: four fewer characters there would name nothing at all.
        let unc = PathBuf::from(r"\\?\UNC\host\share\app.exe");
        assert_eq!(readable(unc.clone()), unc);

        plan_for(&["notepad"], |plan| {
            let text = render_plan(plan);
            assert!(text.contains("never through PATH"), "the plan must say why a bare name fails: {text}");
            let json: serde_json::Value = serde_json::from_str(&render_json(plan)).expect("valid JSON");
            assert_eq!(json["target"]["state"], "missing");
            assert_eq!(json["target"]["resolved"], serde_json::Value::Null);
        });
    }

    /// `--report` names a file the plan would write and does NOT write. The line says "would", because
    /// an evidence file for a session that never ran is the one artefact this tool must never produce.
    #[test]
    fn the_evidence_path_is_named_as_something_that_would_be_written() {
        let path = std::env::temp_dir().join("chrono-dry-run-must-not-write.txt");
        let _ = std::fs::remove_file(&path);
        let arg = path.display().to_string();

        plan_for(&["notepad", "--report", &arg, "--ticks", "5", "--timeout", "30"], |plan| {
            let text = render_plan(plan);
            assert!(text.contains("would be written to"), "{text}");
            assert!(text.contains("ends after 5 state heartbeats"), "{text}");
            assert!(text.contains("gives up after 30s"), "{text}");
            let json: serde_json::Value = serde_json::from_str(&render_json(plan)).expect("valid JSON");
            assert_eq!(json["session"]["report"], arg);
            assert_eq!(json["session"]["ticks"], 5);
            assert_eq!(json["session"]["timeout_secs"], 30);
        });

        assert!(!path.exists(), "a dry run must not write the evidence file it names");
    }

    /// The bitness in the plan is this core's, and the sentence has to keep saying so. A plan that
    /// named the target's would be guessing: a .NET AnyCPU image carries IMAGE_FILE_MACHINE_I386 in
    /// its header and runs 64-bit, which is why chrono-mech asks the running process instead.
    #[test]
    fn the_mechanism_line_names_this_cores_bitness_and_not_the_targets() {
        // A target that is really there, so the line is the mechanism rather than "not decided".
        let exe = std::env::current_exe().expect("this test has an executable");
        let exe = exe.display().to_string();
        plan_for(&[&exe], |plan| {
            let text = mechanism_text(plan);
            assert!(text.contains("native injection"), "{text}");
            assert!(text.contains(this_bitness()), "the plan must name the core it is: {text}");
            assert!(text.contains("32-bit target needs the other build"), "{text}");
        });

        // With no file to look at, the mechanism is not guessed either - it is chosen from what sits
        // beside the target, and there is nothing beside a target that is not there.
        plan_for(&["notepad"], |plan| {
            assert!(mechanism_text(plan).contains("not decided"), "{}", mechanism_text(plan));
        });
    }
}
