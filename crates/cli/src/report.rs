//! What a finished session looks like to a person: the terminal report and the evidence export.
//!
//! Every branch here answers to untouchable rule 4 - the report never claims coverage it did not
//! measure. An unknown key is printed verbatim with its origin rather than smoothed into prose, and a
//! session that never started says so instead of showing a verdict.
//!
//! `detect_runtime_warnings` is the one part that looks at the target rather than the result: some
//! runtimes read elapsed time from a counter this tool leaves alone by default (ADR-2), and saying so
//! before the session is the honest alternative to a report that looks fine and is not.

/// One process's channel coverage as of the latest `coverage` event for its pid. Kept per process
/// and replaced rather than appended, because the core reports a process more than once: a first
/// snapshot when it is discovered, and a final one when the session ends (R2-X8).
pub(crate) struct ProcessCoverage {
    pub(crate) covered: Vec<chrono_proto::CoveredChannel>,
    pub(crate) observed: Vec<chrono_proto::CoveredChannel>,
    pub(crate) uncovered: Vec<String>,
}

/// The captured outcome of a `chrono run` session, rendered as a human report in the
/// non-json path. The core emits stable KEYS - this consumer renders English prose (rule 15:
/// the CLI is English-only, so the key->text table lives here, never in the core - rule 16).
/// The Stage 2 verifier is exactly this: a view over what the core already emits, one that
/// makes a NON-effect as legible as an effect.
pub(crate) struct SessionReport {
    pub(crate) target: String,
    /// Family verdict (verdict token, reason key, process count) - the headline.
    pub(crate) session_verdict: Option<(String, String, u32)>,
    /// Parent/start verdict, used only if no session_verdict arrived (e.g. an older core).
    pub(crate) parent_verdict: Option<(String, String)>,
    /// The target vanished right after injection - an honest non-effect (ADR-4).
    pub(crate) vanished: Option<(String, u64)>,
    /// Error events the core emitted, as (key, origin). The `--json` surface always carried these -
    /// the human report used to drop them on the floor and print `<no verdict emitted>`, so a session
    /// that never started said nothing about WHY (untouchable rule 6). A start failure becomes the
    /// headline - anything the core rejected mid-session (an out-of-range `set_multiplier`, which does
    /// not end the session) is listed below the verdict instead of vanishing.
    pub(crate) errors: Vec<(String, String)>,
    pub(crate) warnings: Vec<String>,
    /// Channels queried but not covered, tagged with the pid that queried them (never summed
    /// across processes - untouchable rule 4).
    pub(crate) uncovered: Vec<(u32, String)>,
    /// Channels covered (substituted), tagged with the pid and the call count. Per-pid, never
    /// summed across processes (untouchable rule 4).
    pub(crate) covered: Vec<(u32, String, u64)>,
    /// Channels hooked but deliberately left real (waits, network, multimedia timers) - their own
    /// bucket so a reader never reads them as substituted.
    pub(crate) observed: Vec<(u32, String, u64)>,
    /// Session duration as the core states it in `ended`: (fake wall reached, real ms elapsed,
    /// fake ms elapsed), or None when `ended` carried no end wall (a session that never started).
    /// Authoritative, not sampled from the heartbeats - one source of truth (3d35a79).
    pub(crate) timing: Option<(String, i64, i64)>,
    /// The target's own exit code from `ended.target_exit_code` - present only when the app exited
    /// on its own. None for a `--ticks` cutoff (the target is still running), for a CDP session (we
    /// close our own instance), and for a session that never started. Informational: it is NOT this
    /// tool's exit code, which is the session verdict (docs/08 section 8).
    pub(crate) target_exit: Option<i32>,
    /// What teardown could not remove, from `ended.residue_keys`. Empty on a native session (its
    /// hooks unhook themselves) - today the only key is a Chromium temp profile that stayed locked.
    pub(crate) residue: Vec<String>,
    /// Whether this was a Chromium (CDP) session: its coverage unit is a JS context, not an OS
    /// process, so the report says "context" instead of "pid".
    pub(crate) cdp: bool,
}

/// One-line English headline for a verdict wire token. Upper-case so success and failure are
/// scannable at a glance.
pub(crate) fn verdict_headline(verdict: &str) -> &'static str {
    match verdict {
        "works" => "WORKS - time substitution took effect",
        "partial" => "PARTIAL - time substitution took effect only in part",
        "fails" => "DID NOT TAKE EFFECT - time was queried but no channel was covered",
        "undetermined" => "UNDETERMINED - coverage could not be established",
        _ => "UNKNOWN",
    }
}

/// English gloss for a reason key. An unknown key yields "" and the caller falls back to the
/// raw key - we never invent an explanation the core did not send.
pub(crate) fn describe_reason(key: &str) -> &'static str {
    match key {
        "session.family_covered" | "coverage.time_channels_covered" => {
            "every process that read time saw the session clock"
        }
        "session.family_partial" | "coverage.time_channels_partial" => {
            "some time channels were covered, some were queried but not covered"
        }
        "session.family_uncovered" | "coverage.time_channels_uncovered" => {
            "time was queried but no channel was covered"
        }
        "session.family_undetermined" | "coverage.undetermined" => {
            "no process read a covered time channel"
        }
        "chromium.contexts_covered" => "every JS context ran on the session clock",
        "chromium.contexts_partial" => "some JS contexts ran on the session clock, some could not be reached",
        "chromium.no_time_calls" => "the shim was installed, but the app called no JS time API",
        "chromium.no_contexts" => "no JS context could be shimmed",
        _ => "",
    }
}

/// English gloss for an error key the core emitted. Same contract as `describe_reason`: an unknown
/// key yields "" and the caller falls back to the raw key, so a core newer than this driver still
/// says something true rather than something invented.
///
/// Whose fault it is matters more here than anywhere else in the report. `core.hook_dll_missing` and
/// `session.control_failed` are OURS (a broken install, a broken session), `target.launch_failed` is
/// the target's, and `target.inject_failed` is genuinely ambiguous - which is exactly why the missing
/// DLL had to stop being reported as that one (R2-W3).
pub(crate) fn describe_error(key: &str) -> &'static str {
    match key {
        "core.hook_dll_missing" => {
            "chrono_hook.dll is missing next to chrono.exe - this Chrono Mock installation is incomplete"
        }
        "session.control_failed" => "the session's control memory could not be set up",
        "session.already_active" => "another Chrono Mock session is already running - one at a time",
        "target.launch_failed" => "the target application could not be started",
        "target.cwd_missing" => "the working directory asked for does not exist",
        "target.inject_failed" => "the hook could not be injected into the target",
        "target.bitness_mismatch" => {
            "the target and this chrono.exe are different bitness - run the chrono.exe that matches the target"
        }
        "target.attach_failed" => "the target could not be attached to (Chromium/Electron, CDP)",
        "moment.invalid" => "the requested moment is not a valid date and time",
        "time.bad_mode" => "the requested time mode is not one this core knows",
        "time.bad_multiplier" => "the requested speed is outside the range this core accepts",
        "protocol.version_mismatch" => "the client and the core speak different protocol versions",
        "protocol.no_command" | "protocol.bad_command" | "protocol.expected_start" => {
            "the core did not receive a usable start command"
        }
        _ => "",
    }
}

/// English gloss for a warning key, with the key appended for traceability. An unknown key is
/// shown verbatim (honest fallback).
pub(crate) fn describe_warning(key: &str) -> String {
    let text = match key {
        "wait.object_waits_not_scaled" => {
            "object waits are hooked but left real - an I/O or hardware timeout is not shortened"
        }
        "timer.multimedia_not_scaled" => "the multimedia timer (timeSetEvent) is observed but not scaled",
        // The coverage has a cost, so it is named - the same shape as the QPC opt-in's warning. winmm is
        // also the audio path, and the scheduler timeSetEvent stays untouched, so this is about a clock
        // being read, never about when sound is queued.
        "clock.timegettime_scaled_audio_may_shift" => {
            "the winmm clock timeGetTime is being scaled with the rest of the duration axis, so a target that paces itself from it follows the session - but a media application that positions audio from that clock may drift"
        }
        "inheritance.ntcreateuserprocess_child_maybe_uncovered" => {
            "a child spawned directly via NtCreateUserProcess may not be covered"
        }
        "coverage.pid_registry_full" => {
            "this session ran more processes than the audit can track (256), so some ran uncovered and are missing from the process count and the channel lists below"
        }
        "time.fake_clock_clamped" => {
            "the fake clock reached the last date this build can represent (year 30828) and stood there for the rest of the session, so late readings are not the moments the rate would have produced"
        }
        "inheritance.child_not_injected" => {
            "a child process could not be covered and ran on the REAL clock - usually a child of the other bitness; the process count below is short by that many"
        }
        "source.network_at_start" => {
            "the target opened a network connection - it may read time from a server, which no local hook can cover"
        }
        // Says what the port MEANS, not just that there is one. Chromium's debugging port listens on
        // loopback with no authentication, so for as long as the session runs, any other process on
        // this machine can attach to it and drive the app - read its pages, run JavaScript in it,
        // navigate it. That is a fact about the session the tester is entitled to before they point
        // this at something that matters, and the previous wording read as a note about tidiness.
        "chromium.launched_with_debug_port" => {
            "an Electron/Chromium app: launched with a remote-debugging port and a clean isolated profile, not your real one - while the session runs, any other local process can use that port to control the app"
        }
        "chromium.app_closed_before_audit" => {
            "the app closed before the audit could read final call counts - the coverage below may be incomplete"
        }
        "chromium.rate_change_affects_running_timers" => {
            "the speed changed in flight: new timers and the clock reflect it at once, but a setInterval already running keeps its old cadence"
        }
        "runtime.python_monotonic_qpc" => {
            "this Python app measures time with perf_counter and monotonic - both use QueryPerformanceCounter on Python 3.13+, which is left real, so a timer built on them does not scale (time.time and the wall clock do)"
        }
        "runtime.python_perfcounter_qpc" => {
            "this Python app's perf_counter uses QueryPerformanceCounter, which is left real, so a perf_counter timer does not scale - on Python 3.13+ monotonic uses QPC too (time.time and the wall clock do scale)"
        }
        "runtime.dotnet_stopwatch_qpc" => {
            "this .NET app's Stopwatch uses QueryPerformanceCounter, which is left real, so a Stopwatch timer does not scale (DateTime and Environment.TickCount do)"
        }
        "runtime.java_nanotime_qpc" => {
            "this Java app's System.nanoTime uses QueryPerformanceCounter, which is left real, so a nanoTime timer does not scale (System.currentTimeMillis does)"
        }
        "qpc.scaled_render_may_distort" => {
            "QueryPerformanceCounter is being scaled (--scale-qpc), so a QPC-timed monotonic/elapsed clock accelerates - but a target that times its rendering or animation off QPC may look distorted"
        }
        _ => "",
    };
    if text.is_empty() {
        key.to_string()
    } else {
        format!("{text} ({key})")
    }
}

/// English text for a cleanup residue key from `ended.residue_keys`. Same shape as
/// `describe_warning`: a known key gets prose plus the key, an unknown one is printed literally
/// rather than swallowed, because a key we cannot name is still a mess we left (rule 6).
pub(crate) fn describe_residue(key: &str) -> String {
    let text = match key {
        "cleanup.chromium_profile_left" => {
            "the temporary Chromium profile could not be removed - delete it by hand if it lingers in your temp folder"
        }
        _ => "",
    };
    if text.is_empty() {
        key.to_string()
    } else {
        format!("{text} ({key})")
    }
}

/// Human label for the target's own exit code. `GetExitCodeProcess` yields a u32 that the wire
/// carries as an i32, so a crash arrives as a large negative number - an access violation reads as
/// -1073741819, which tells a tester nothing. A negative code therefore also gets the hexadecimal
/// form (0xC0000005 there), which is the one that can be looked up.
pub(crate) fn exit_code_label(code: i32) -> String {
    if code < 0 {
        format!("{code} (0x{:08X})", code as u32)
    } else {
        code.to_string()
    }
}

/// Statically detect the target's language runtime and warn that its monotonic/elapsed clocks stand on
/// QueryPerformanceCounter, which is left real (ADR-2) and so does not scale - the failure the user hit
/// when a Python/.NET/Java timer stayed still under a fast clock. This reads only file NAMES beside the
/// target (and its PyInstaller `_internal/` folder): no QPC hook (ADR-2 holds), no process inspection.
/// Best-effort: a runtime unpacked at runtime (PyInstaller onefile) or launched as `java -jar` is not
/// caught here, and a false positive only adds a "may not scale" note, never a false verdict (rules 4, 6).
pub(crate) fn detect_runtime_warnings(target_path: &std::path::Path, scale_qpc: bool) -> Vec<String> {
    // Under --scale-qpc the QPC axis IS scaled (A1), so a "monotonic/elapsed does not scale" warning would
    // be WRONG (B1 suppressed - it would contradict the feature). Instead warn once that scaling QPC can
    // distort a target that times its rendering off QPC (games, animation) - the render risk ADR-2 guarded
    // against. This holds for any target, so it does not depend on the runtime fingerprint below.
    if scale_qpc {
        return vec!["qpc.scaled_render_may_distort".to_string()];
    }

    fn add(keys: &mut Vec<String>, key: &str) {
        if !keys.iter().any(|k| k == key) {
            keys.push(key.to_string());
        }
    }

    let mut keys: Vec<String> = Vec::new();

    // The target executable's own name is a strong signal - a plain interpreter launcher.
    if let Some(name) = target_path.file_name().and_then(|n| n.to_str()) {
        match name.to_ascii_lowercase().as_str() {
            "python.exe" | "pythonw.exe" => add(&mut keys, "runtime.python_perfcounter_qpc"),
            "java.exe" | "javaw.exe" => add(&mut keys, "runtime.java_nanotime_qpc"),
            _ => {}
        }
    }

    // Runtime DLLs and manifests shipped beside the exe, and in PyInstaller's `_internal/` folder.
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Some(parent) = target_path.parent() {
        dirs.push(parent.to_path_buf());
        dirs.push(parent.join("_internal"));
    }
    for dir in dirs {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue, // a missing _internal/ is normal, not an error
        };
        for entry in entries.flatten() {
            let name = match entry.file_name().to_str() {
                Some(n) => n.to_ascii_lowercase(),
                None => continue,
            };
            if let Some(minor) = python_dll_minor(&name) {
                // Python 3.13+ moved monotonic onto QPC too (CPython PR 116781) - earlier, only
                // perf_counter is on QPC and monotonic (GetTickCount64) still scales.
                add(
                    &mut keys,
                    if minor >= 13 {
                        "runtime.python_monotonic_qpc"
                    } else {
                        "runtime.python_perfcounter_qpc"
                    },
                );
            } else if name == "python3.dll" || name == "python.dll" {
                add(&mut keys, "runtime.python_perfcounter_qpc"); // stable-ABI dll, version unknown
            } else if name == "coreclr.dll" || name == "clr.dll" || name.ends_with(".deps.json") {
                add(&mut keys, "runtime.dotnet_stopwatch_qpc");
            } else if name == "jvm.dll" {
                add(&mut keys, "runtime.java_nanotime_qpc");
            }
        }
    }

    // A PyInstaller bundle ships BOTH python3.dll (stable ABI, version unknown) and python3XX.dll, so
    // both python keys can fire. The monotonic warning already names perf_counter, so drop the narrower
    // perf_counter-only key when the monotonic one is present - one clear warning, not two overlapping.
    if keys.iter().any(|k| k == "runtime.python_monotonic_qpc") {
        keys.retain(|k| k != "runtime.python_perfcounter_qpc");
    }

    keys
}

/// Parse the CPython minor version from a versioned dll name: "python314.dll" -> Some(14), "python39.dll"
/// -> Some(9). Returns None for "python3.dll" (stable ABI, no minor) or any non-matching name.
pub(crate) fn python_dll_minor(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("python3").and_then(|s| s.strip_suffix(".dll"))?;
    if rest.is_empty() {
        return None; // "python3.dll" - stable ABI, minor unknown
    }
    rest.parse::<u32>().ok()
}

/// "1 call" vs "N calls" - a bare plural reads wrong in a report a user pastes into a bug ticket.
pub(crate) fn calls_label(n: u64) -> String {
    if n == 1 {
        "1 call".to_string()
    } else {
        format!("{n} calls")
    }
}

/// Render the session outcome as a human report (English). Failure and non-effect are the
/// point: the Stage 2 gate is recognising when substitution did NOT take effect.
pub(crate) fn render_report(r: &SessionReport) -> String {
    let mut out = String::from("Chrono Mock - session report\n");
    out.push_str(&format!("  target:   {}\n", r.target));
    // A Chromium session's coverage unit is a JS context, not an OS process (rule 4 - never sum
    // across units either way).
    let unit = if r.cdp { "context" } else { "pid" };
    let units = if r.cdp { "contexts" } else { "processes" };

    // Headline priority: a vanish is an honest non-effect, then the family verdict, then the
    // parent verdict as a fallback for an older core, then nothing.
    if let Some((reason_key, lived_ms)) = &r.vanished {
        out.push_str("  verdict:  DID NOT TAKE EFFECT - the target vanished right after injection\n");
        out.push_str(&format!(
            "            (suspected single-instance app: {reason_key}; lived {lived_ms} ms)\n"
        ));
    } else if let Some((verdict, reason_key, count)) = &r.session_verdict {
        out.push_str(&format!(
            "  verdict:  {}  ({units}: {count})\n",
            verdict_headline(verdict)
        ));
        let why = describe_reason(reason_key);
        if why.is_empty() {
            out.push_str(&format!("            reason: {reason_key}\n"));
        } else {
            out.push_str(&format!("            {why}\n"));
        }
    } else if let Some((verdict, reason_key)) = &r.parent_verdict {
        out.push_str(&format!("  verdict:  {}\n", verdict_headline(verdict)));
        out.push_str(&format!("            reason: {reason_key}\n"));
    } else if let Some((key, origin)) = r.errors.first() {
        // No verdict at all AND an error: the session never started. Say why, and say whose side it
        // came from - `<no verdict emitted>` alone left the tester to guess between a broken install,
        // a target that would not launch, and a tool that did nothing (untouchable rule 6).
        out.push_str("  verdict:  DID NOT START - the session never began\n");
        let why = describe_error(key);
        if why.is_empty() {
            out.push_str(&format!("            reason: {key} (from {origin})\n"));
        } else {
            out.push_str(&format!("            {why} [{key}]\n"));
        }
    } else {
        out.push_str("  verdict:  <no verdict emitted>\n");
    }

    // Errors the headline did not consume: a session that DID start and then had a command rejected
    // (an out-of-range set_multiplier, which by design does not end the session). Those used to be
    // visible only under --json.
    let trailing = if r.session_verdict.is_some() || r.parent_verdict.is_some() || r.vanished.is_some()
    {
        &r.errors[..]
    } else {
        r.errors.get(1..).unwrap_or(&[])
    };
    if !trailing.is_empty() {
        out.push_str("  errors:\n");
        for (key, origin) in trailing {
            let why = describe_error(key);
            if why.is_empty() {
                out.push_str(&format!("            - {key} (from {origin})\n"));
            } else {
                out.push_str(&format!("            - {why} [{key}]\n"));
            }
        }
    }

    if let Some((fake_wall, real_ms, fake_ms)) = &r.timing {
        out.push_str(&format!("  session:  fake clock reached {fake_wall}\n"));
        out.push_str(&format!(
            "            real elapsed {:.1}s, fake elapsed {:.1}s\n",
            *real_ms as f64 / 1000.0,
            *fake_ms as f64 / 1000.0
        ));
    }

    // The target's own exit code, when the app closed itself. Spelled out rather than printed bare,
    // because the report already carries a second number that means something else entirely: this
    // tool's exit code is the verdict (docs/08 section 8), never the target's.
    if let Some(code) = r.target_exit {
        out.push_str(&format!(
            "  exited:   the target closed itself with code {}\n",
            exit_code_label(code)
        ));
    }

    if !r.covered.is_empty() {
        out.push_str("  covered channels (substituted, with call counts):\n");
        for (pid, ch, calls) in &r.covered {
            out.push_str(&format!("            - {unit} {pid}: {ch} ({})\n", calls_label(*calls)));
        }
    }

    if !r.observed.is_empty() {
        out.push_str("  observed channels (hooked but left real):\n");
        for (pid, ch, calls) in &r.observed {
            out.push_str(&format!("            - {unit} {pid}: {ch} ({})\n", calls_label(*calls)));
        }
    }

    if !r.uncovered.is_empty() {
        out.push_str("  uncovered channels (queried but not covered):\n");
        for (pid, ch) in &r.uncovered {
            out.push_str(&format!("            - {unit} {pid}: {ch}\n"));
        }
    }

    if !r.warnings.is_empty() {
        out.push_str("  warnings:\n");
        for w in &r.warnings {
            out.push_str(&format!("            - {}\n", describe_warning(w)));
        }
    }

    // Mess the session left behind. A run that could not clean up after itself says so rather than
    // ending on a tidy-looking report (rule 6) - the same list the GUI panel shows.
    if !r.residue.is_empty() {
        out.push_str("  not fully cleaned up:\n");
        for key in &r.residue {
            out.push_str(&format!("            - {}\n", describe_residue(key)));
        }
    }

    out
}

/// Session parameters echoed into an evidence export so the file stands alone as proof (8.8).
pub(crate) struct EvidenceParams {
    pub(crate) moment: String,
    pub(crate) zone: String,
    pub(crate) mode: String,
}

/// A clean WORKS session (no vanish, no partial/fails). Anything else must carry the unreliable
/// banner in an evidence export - evidence that hides doubt is worse than none (8.8).
pub(crate) fn session_is_reliable(r: &SessionReport) -> bool {
    if r.vanished.is_some() {
        return false;
    }
    if let Some((v, _, _)) = &r.session_verdict {
        return v == "works";
    }
    if let Some((v, _)) = &r.parent_verdict {
        return v == "works";
    }
    false
}

/// Human label for the requested time mode.
pub(crate) fn mode_label(mode: &str, multiplier: Option<i64>) -> String {
    match mode {
        "multiplier" => format!("x{}", multiplier.unwrap_or(1)),
        other => other.to_string(),
    }
}

/// Compose the evidence file: an unreliable banner for any non-WORKS session (8.8), then the human
/// report, then the requested parameters so the file is self-contained proof.
pub(crate) fn render_evidence(r: &SessionReport, p: &EvidenceParams) -> String {
    let mut out = String::new();
    if !session_is_reliable(r) {
        out.push_str(
            "!! UNRELIABLE EVIDENCE - the time substitution did not fully take effect. \
             Do not cite this as proof of behavior.\n\n",
        );
    }
    out.push_str(&render_report(r));
    out.push_str(&format!(
        "  requested:  {} (zone {}, mode {})\n",
        p.moment, p.zone, p.mode
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::map_prepare_error;
    use crate::testutil::unique_temp_dir;

    fn empty_report() -> SessionReport {
        SessionReport {
            target: "app.exe".into(),
            session_verdict: None,
            parent_verdict: None,
            vanished: None,
            errors: vec![],
            warnings: vec![],
            uncovered: vec![],
            covered: vec![],
            observed: vec![],
            timing: None,
            target_exit: None,
            residue: vec![],
            cdp: false,
        }
    }

    #[test]
    fn cdp_report_labels_the_unit_as_context() {
        let r = SessionReport {
            session_verdict: Some(("works".into(), "chromium.contexts_covered".into(), 2)),
            covered: vec![(1, "page setInterval".into(), 5)],
            warnings: vec!["chromium.launched_with_debug_port".into()],
            cdp: true,
            ..empty_report()
        };
        let out = render_report(&r);
        assert!(out.contains("contexts: 2"), "got:\n{out}");
        assert!(out.contains("- context 1: page setInterval"), "got:\n{out}");
        assert!(out.contains("JS context ran on the session clock"), "got:\n{out}");
        assert!(out.contains("remote-debugging port"), "got:\n{out}");
    }

    #[test]
    fn works_headline_is_scannable() {
        let r = SessionReport {
            session_verdict: Some(("works".into(), "session.family_covered".into(), 2)),
            ..empty_report()
        };
        let out = render_report(&r);
        assert!(out.contains("WORKS"), "got:\n{out}");
        assert!(out.contains("processes: 2"), "got:\n{out}");
        assert!(out.contains("saw the session clock"), "got:\n{out}");
    }

    #[test]
    fn vanish_reads_as_not_taking_effect() {
        let r = SessionReport {
            vanished: Some(("target.single_instance_suspected".into(), 1500)),
            ..empty_report()
        };
        let out = render_report(&r);
        assert!(out.contains("DID NOT TAKE EFFECT"), "got:\n{out}");
        assert!(out.contains("vanished"), "got:\n{out}");
    }

    #[test]
    fn a_bitness_mismatch_is_a_usage_error_and_names_both_sides() {
        // R2-S1: exit 1, not 2. Nothing is wrong with the application - the wrong build of the tool
        // was pointed at it, and the message has to say which one to run instead.
        let (code, key, origin, detail) =
            map_prepare_error(chrono_mech::PrepareError::BitnessMismatch("x86", "x64"));
        assert_eq!(code, 1);
        assert_eq!(key, "target.bitness_mismatch");
        assert_eq!(origin, "mechanism");
        assert!(detail.contains("runs as x86"), "got: {detail}");
        assert!(detail.contains("this core is x64"), "got: {detail}");
        assert!(detail.contains("run the x86 chrono.exe"), "got: {detail}");
        // And it reads as its own thing in the report, not as an injection failure.
        assert_ne!(describe_error(key), describe_error("target.inject_failed"));
        assert!(!describe_error(key).is_empty());
    }

    #[test]
    fn a_session_that_never_started_says_why_not_no_verdict_emitted() {
        // R2-W3, second half. A broken install now reaches `core.hook_dll_missing` instead of being
        // called `target.inject_failed` - but the human report used to drop every error event, so the
        // reachable diagnosis would still have arrived only under --json.
        let r = SessionReport {
            errors: vec![("core.hook_dll_missing".into(), "core".into())],
            ..empty_report()
        };
        let out = render_report(&r);
        assert!(out.contains("DID NOT START"), "got:\n{out}");
        assert!(out.contains("installation is incomplete"), "got:\n{out}");
        assert!(out.contains("[core.hook_dll_missing]"), "got:\n{out}");
        assert!(!out.contains("<no verdict emitted>"), "got:\n{out}");
        // The headline consumed the only error, so no duplicate list underneath it.
        assert!(!out.contains("  errors:"), "got:\n{out}");
    }

    #[test]
    fn an_unknown_error_key_is_shown_verbatim_with_its_origin() {
        // Same contract as the reason and warning tables: a core newer than this driver says something
        // true rather than something invented.
        let r = SessionReport {
            errors: vec![("some.future_key".into(), "mechanism".into())],
            ..empty_report()
        };
        let out = render_report(&r);
        assert!(out.contains("some.future_key (from mechanism)"), "got:\n{out}");
    }

    #[test]
    fn an_error_after_the_session_started_is_listed_not_swallowed() {
        // A rejected in-flight command (an out-of-range set_multiplier) deliberately does not end the
        // session, so it must not become the headline - but it must not disappear either.
        let r = SessionReport {
            session_verdict: Some(("works".into(), "session.family_covered".into(), 1)),
            errors: vec![("time.bad_multiplier".into(), "core".into())],
            ..empty_report()
        };
        let out = render_report(&r);
        assert!(out.contains("WORKS"), "got:\n{out}");
        assert!(out.contains("  errors:"), "got:\n{out}");
        assert!(out.contains("outside the range"), "got:\n{out}");
    }

    #[test]
    fn uncovered_and_warnings_are_surfaced_unknown_key_verbatim() {
        let r = SessionReport {
            session_verdict: Some(("partial".into(), "session.family_partial".into(), 1)),
            uncovered: vec![(1234, "KUSER_SHARED_DATA".into())],
            warnings: vec!["wait.object_waits_not_scaled".into(), "some.unknown_key".into()],
            ..empty_report()
        };
        let out = render_report(&r);
        assert!(out.contains("PARTIAL"), "got:\n{out}");
        assert!(out.contains("pid 1234: KUSER_SHARED_DATA"), "got:\n{out}");
        assert!(out.contains("object waits are hooked"), "got:\n{out}");
        // An unknown warning key is shown verbatim - we never invent an explanation.
        assert!(out.contains("some.unknown_key"), "got:\n{out}");
    }

    #[test]
    fn covered_and_observed_channels_are_shown_with_counts_per_pid() {
        let r = SessionReport {
            session_verdict: Some(("works".into(), "session.family_covered".into(), 1)),
            covered: vec![(1234, "GetSystemTime".into(), 7)],
            observed: vec![(1234, "WaitForSingleObject".into(), 1)],
            ..empty_report()
        };
        let out = render_report(&r);
        assert!(out.contains("covered channels"), "got:\n{out}");
        assert!(out.contains("pid 1234: GetSystemTime (7 calls)"), "got:\n{out}");
        assert!(out.contains("observed channels (hooked but left real)"), "got:\n{out}");
        // Singular reads correctly (1 call, not "1 calls").
        assert!(out.contains("pid 1234: WaitForSingleObject (1 call)"), "got:\n{out}");
    }

    #[test]
    fn target_exit_code_is_shown_as_the_apps_own() {
        // The wire has carried `ended.target_exit_code` all along and the GUI panel has shown it
        // since 7446a59 - the CLI report dropped it, so a plain run could not tell "the app failed
        // with code 3" from "we ended the session" (rule 6). Spelled out, because this report also
        // carries a verdict whose code means something else entirely (docs/08 section 8).
        let r = SessionReport {
            session_verdict: Some(("works".into(), "session.family_covered".into(), 1)),
            target_exit: Some(3),
            ..empty_report()
        };
        let out = render_report(&r);
        assert!(out.contains("the target closed itself with code 3"), "got:\n{out}");
    }

    #[test]
    fn a_target_that_did_not_close_itself_gets_no_exit_line() {
        // No exit code means the app did not close itself: a --ticks cutoff (it is still running),
        // a CDP session (we close our own instance), or a session that never started. Printing 0
        // there would state a result the session never observed (untouchable rule 4).
        let r = SessionReport {
            session_verdict: Some(("works".into(), "session.family_covered".into(), 1)),
            target_exit: None,
            ..empty_report()
        };
        let out = render_report(&r);
        assert!(!out.contains("closed itself"), "got:\n{out}");
        assert!(!out.contains("exited:"), "got:\n{out}");
    }

    #[test]
    fn a_crash_exit_code_carries_the_hex_form() {
        // GetExitCodeProcess yields a u32 the wire carries as an i32, so an access violation arrives
        // as -1073741819 - a number nobody recognises until it is written 0xC0000005.
        let r = SessionReport {
            session_verdict: Some(("works".into(), "session.family_covered".into(), 1)),
            target_exit: Some(-1073741819),
            ..empty_report()
        };
        let out = render_report(&r);
        assert!(out.contains("-1073741819 (0xC0000005)"), "got:\n{out}");
    }

    #[test]
    fn cleanup_residue_is_reported_unknown_key_verbatim() {
        let r = SessionReport {
            session_verdict: Some(("works".into(), "chromium.contexts_covered".into(), 1)),
            residue: vec!["cleanup.chromium_profile_left".into(), "cleanup.something_new".into()],
            cdp: true,
            ..empty_report()
        };
        let out = render_report(&r);
        assert!(out.contains("not fully cleaned up"), "got:\n{out}");
        assert!(out.contains("temporary Chromium profile"), "got:\n{out}");
        assert!(out.contains("cleanup.chromium_profile_left"), "got:\n{out}");
        // Same rule as warnings: a key we cannot name is still shown, never swallowed.
        assert!(out.contains("cleanup.something_new"), "got:\n{out}");
    }

    #[test]
    fn evidence_export_carries_the_target_exit_code() {
        // docs/08 section 9: --report writes "the same readable report", so the file a tester cites
        // as proof must not be missing what the console showed.
        let r = SessionReport {
            session_verdict: Some(("works".into(), "session.family_covered".into(), 1)),
            target_exit: Some(2),
            ..empty_report()
        };
        let p = EvidenceParams {
            moment: "2038-01-19T03:14:07".into(),
            zone: "+00:00".into(),
            mode: "x60".into(),
        };
        let out = render_evidence(&r, &p);
        assert!(out.contains("the target closed itself with code 2"), "got:\n{out}");
    }

    #[test]
    fn evidence_from_works_has_no_unreliable_banner_and_echoes_params() {
        let r = SessionReport {
            session_verdict: Some(("works".into(), "session.family_covered".into(), 1)),
            covered: vec![(1, "GetSystemTime".into(), 2)],
            ..empty_report()
        };
        let p = EvidenceParams {
            moment: "2038-01-19T03:14:07".into(),
            zone: "+00:00".into(),
            mode: "x60".into(),
        };
        let out = render_evidence(&r, &p);
        assert!(!out.contains("UNRELIABLE"), "a works session must not be flagged, got:\n{out}");
        assert!(out.contains("WORKS"), "got:\n{out}");
        assert!(
            out.contains("requested:  2038-01-19T03:14:07 (zone +00:00, mode x60)"),
            "got:\n{out}"
        );
    }

    #[test]
    fn evidence_from_vanish_leads_with_the_unreliable_banner() {
        let r = SessionReport {
            vanished: Some(("target.single_instance_suspected".into(), 1500)),
            ..empty_report()
        };
        let p = EvidenceParams {
            moment: "2038-01-19T03:14:07".into(),
            zone: "+00:00".into(),
            mode: "flow".into(),
        };
        let out = render_evidence(&r, &p);
        assert!(out.starts_with("!! UNRELIABLE EVIDENCE"), "non-works must lead with the banner, got:\n{out}");
        assert!(out.contains("DID NOT TAKE EFFECT"), "got:\n{out}");
    }

    #[test]
    fn session_timing_section_shows_reached_clock_and_elapsed() {
        let r = SessionReport {
            session_verdict: Some(("works".into(), "session.family_covered".into(), 1)),
            timing: Some(("2038-01-19 03:15:07".into(), 1500, 90000)),
            ..empty_report()
        };
        let out = render_report(&r);
        assert!(out.contains("session:  fake clock reached 2038-01-19 03:15:07"), "got:\n{out}");
        // 1.5s real mapped to 90s fake - the x60 acceleration is visible in the report.
        assert!(out.contains("real elapsed 1.5s, fake elapsed 90.0s"), "got:\n{out}");
    }

    #[test]
    fn python_dll_minor_parses_versioned_names() {
        assert_eq!(python_dll_minor("python314.dll"), Some(14));
        assert_eq!(python_dll_minor("python313.dll"), Some(13));
        assert_eq!(python_dll_minor("python39.dll"), Some(9));
        assert_eq!(python_dll_minor("python3.dll"), None); // stable ABI, no minor
        assert_eq!(python_dll_minor("python.dll"), None);
        assert_eq!(python_dll_minor("kernel32.dll"), None);
    }

    #[test]
    fn detect_runtime_flags_python_313_plus_monotonic() {
        // PyInstaller onedir ships BOTH python3.dll (stable ABI) and python3XX.dll, exactly like the real
        // BeanNetworkTester bundle. Python 3.14 -> monotonic warning, and the narrower perf_counter-only
        // key (from python3.dll) is dropped so there is one clear warning, not two overlapping ones.
        let dir = unique_temp_dir("chrono-rt-py313");
        std::fs::create_dir_all(dir.join("_internal")).unwrap();
        std::fs::write(dir.join("_internal").join("python3.dll"), b"").unwrap();
        std::fs::write(dir.join("_internal").join("python314.dll"), b"").unwrap();
        let target = dir.join("App.exe");
        std::fs::write(&target, b"").unwrap();

        let keys = detect_runtime_warnings(&target, false);
        assert!(keys.iter().any(|k| k == "runtime.python_monotonic_qpc"));
        assert!(!keys.iter().any(|k| k == "runtime.python_perfcounter_qpc"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn detect_runtime_flags_python_312_perfcounter_only() {
        // Python 3.12: only perf_counter is on QPC - monotonic (GetTickCount64) still scales.
        let dir = unique_temp_dir("chrono-rt-py312");
        std::fs::create_dir_all(dir.join("_internal")).unwrap();
        std::fs::write(dir.join("_internal").join("python312.dll"), b"").unwrap();
        let target = dir.join("App.exe");
        std::fs::write(&target, b"").unwrap();

        let keys = detect_runtime_warnings(&target, false);
        assert!(keys.iter().any(|k| k == "runtime.python_perfcounter_qpc"));
        assert!(!keys.iter().any(|k| k == "runtime.python_monotonic_qpc"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn detect_runtime_flags_dotnet_java_and_stays_silent_on_native() {
        // .NET via a sibling manifest.
        let net = unique_temp_dir("chrono-rt-net");
        std::fs::create_dir_all(&net).unwrap();
        std::fs::write(net.join("App.deps.json"), b"{}").unwrap();
        let net_target = net.join("App.exe");
        std::fs::write(&net_target, b"").unwrap();
        assert!(detect_runtime_warnings(&net_target, false).iter().any(|k| k == "runtime.dotnet_stopwatch_qpc"));
        std::fs::remove_dir_all(&net).ok();

        // Java via the target exe name (a plain launcher).
        let java = unique_temp_dir("chrono-rt-java");
        std::fs::create_dir_all(&java).unwrap();
        let java_target = java.join("java.exe");
        std::fs::write(&java_target, b"").unwrap();
        assert!(detect_runtime_warnings(&java_target, false).iter().any(|k| k == "runtime.java_nanotime_qpc"));
        std::fs::remove_dir_all(&java).ok();

        // A native target with no runtime markers gets no warning (honest silence, rule 4).
        let native = unique_temp_dir("chrono-rt-native");
        std::fs::create_dir_all(&native).unwrap();
        let native_target = native.join("Native.exe");
        std::fs::write(&native_target, b"").unwrap();
        assert!(detect_runtime_warnings(&native_target, false).is_empty());
        std::fs::remove_dir_all(&native).ok();
    }

    #[test]
    fn scale_qpc_replaces_runtime_warning_with_a_render_caution() {
        // A2: under --scale-qpc the QPC axis scales, so the "does not scale" warning would be wrong. Even a
        // Python target that would otherwise get runtime.python_monotonic_qpc gets ONLY the render caution.
        let dir = unique_temp_dir("chrono-rt-qpc");
        std::fs::create_dir_all(dir.join("_internal")).unwrap();
        std::fs::write(dir.join("_internal").join("python314.dll"), b"").unwrap();
        let target = dir.join("App.exe");
        std::fs::write(&target, b"").unwrap();

        let keys = detect_runtime_warnings(&target, true);
        assert_eq!(keys, vec!["qpc.scaled_render_may_distort".to_string()]);
        assert!(!keys.iter().any(|k| k == "runtime.python_monotonic_qpc"));

        // And a native target under --scale-qpc still gets the render caution (QPC scales for any target).
        let native = unique_temp_dir("chrono-rt-qpc-native");
        std::fs::create_dir_all(&native).unwrap();
        let native_target = native.join("Native.exe");
        std::fs::write(&native_target, b"").unwrap();
        assert_eq!(
            detect_runtime_warnings(&native_target, true),
            vec!["qpc.scaled_render_may_distort".to_string()]
        );

        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&native).ok();
    }
}
