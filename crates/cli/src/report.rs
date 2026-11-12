//! What a finished session looks like to a person: the terminal report and the evidence export.
//!
//! Every branch here answers to untouchable rule 4 - the report never claims coverage it did not
//! measure. An unknown key is printed verbatim with its origin rather than smoothed into prose, and a
//! session that never started says so instead of showing a verdict.
//!
//! `detect_runtime_warnings` is the one part that looks at the target rather than the result: some
//! runtimes read elapsed time from a counter this tool leaves alone by default (ADR-2), and saying so
//! before the session is the honest alternative to a report that looks fine and is not.

/// Which namespace a coverage row's number lives in: an operating-system pid the hook is inside,
/// the index of a JS context reached over the DevTools protocol (`coverage.kind` on the wire), or a
/// kind this build does not know - a core newer than this driver may name one, and calling its rows
/// processes would be a claim the core never made. Processes sort first, so the parent still leads a
/// report that holds all three.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Unit {
    Process,
    Context,
    Unknown,
}

/// A coverage unit as the report names it: `pid 1234` or `context 1`. Two namespaces that a reader
/// must not confuse, and the family of one session can hold both (docs/09 section 12.4), so the
/// number never travels without its kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct UnitId {
    pub(crate) unit: Unit,
    pub(crate) id: u32,
}

impl UnitId {
    /// From a `coverage` event's `kind` and `pid`. The two tokens this build knows name their unit,
    /// and the token an older core never wrote defaults to a process on the wire itself. Anything
    /// else is kept as unknown rather than folded into either: the row stays in the report with its
    /// evidence, labelled as a unit this driver could not name, instead of being called a pid.
    pub(crate) fn from_wire(kind: &str, pid: u32) -> UnitId {
        let unit = match kind {
            chrono_proto::UNIT_PROCESS => Unit::Process,
            chrono_proto::UNIT_CONTEXT => Unit::Context,
            _ => Unit::Unknown,
        };
        UnitId { unit, id: pid }
    }
}

impl std::fmt::Display for UnitId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.unit {
            Unit::Process => write!(f, "pid {}", self.id),
            Unit::Context => write!(f, "context {}", self.id),
            Unit::Unknown => write!(f, "unit {} (of a kind this build does not know)", self.id),
        }
    }
}

/// One process's channel coverage as of the latest `coverage` event for its pid. Kept per process
/// and replaced rather than appended, because the core reports a process more than once: a first
/// snapshot when it is discovered, and a final one when the session ends (R2-X8).
pub(crate) struct ProcessCoverage {
    pub(crate) covered: Vec<chrono_proto::CoveredChannel>,
    pub(crate) observed: Vec<chrono_proto::CoveredChannel>,
    pub(crate) uncovered: Vec<String>,
    pub(crate) unobserved: Vec<String>,
    pub(crate) installed_late: Vec<String>,
}

/// Why a run ended before the session closed by itself (R5-A9). Each one has its own line in the report,
/// and every match on it names all three - while it was a string, a misspelt one read as the idle limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CutShort {
    /// `--timeout` ran out and the driver stopped the core.
    Timeout,
    /// The core sent no event for the idle limit and the driver stopped it.
    Idle,
    /// The core died or was killed before it sent `ended` (R4-W3).
    CoreLost,
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
    /// Channels queried but not covered, tagged with the unit that queried them (never summed
    /// across units - untouchable rule 4).
    pub(crate) uncovered: Vec<(UnitId, String)>,
    /// Channels this session meant to WATCH and could not hook, tagged with the unit. Separate from
    /// `uncovered` because it is not a verdict input: a watch that did not start says nothing about
    /// whether the substitution took effect. It is here because the alternative was no line at all.
    pub(crate) unobserved: Vec<(UnitId, String)>,
    /// Channels hooked only once their module loaded, tagged with the unit. Every one is also listed
    /// above with its count, and this is what says that count is a floor - named, because the warning
    /// alone said "a time channel" and left the reader to guess which.
    pub(crate) installed_late: Vec<(UnitId, String)>,
    /// Channels covered (substituted), tagged with the unit and the call count. Per unit, never
    /// summed across units (untouchable rule 4).
    pub(crate) covered: Vec<(UnitId, String, u64)>,
    /// Channels hooked but deliberately left real (waits, network, multimedia timers) - their own
    /// bucket so a reader never reads them as substituted.
    pub(crate) observed: Vec<(UnitId, String, u64)>,
    /// The covered and observed rows whose count stopped at the hook's ceiling, by unit and channel -
    /// their count is a floor, and the report says "at least" (R5-WY11).
    pub(crate) capped: Vec<(UnitId, String)>,
    /// How long the machine slept during the session, from `session_verdict.host_slept_ms` - 0 when it
    /// did not, or the core did not say (R5-P1).
    pub(crate) host_slept_ms: u64,
    /// What the browser of a Chromium session calls itself, from `session_verdict.browser` - which
    /// engine the evidence is about (R5-D44). `None` for a native session.
    pub(crate) browser: Option<String>,
    /// JS contexts the session covered beside its processes, from `session_verdict.context_count`.
    /// Zero for a native session that reached no embedded engine, and for a Chromium session, whose
    /// contexts already ride `process_count` (docs/08 9b).
    pub(crate) context_count: u32,
    /// The DevTools endpoints the session reached inside the application, one per engine.
    pub(crate) engines: Vec<chrono_proto::ReachedEngine>,
    /// The processes the session went on for after the target closed, from `session_verdict.followed`
    /// (ADR-16). Empty when nothing outlived the target.
    pub(crate) followed: Vec<chrono_proto::FollowedProcess>,
    /// The copies outside the session the target most likely handed its work to, from
    /// `session_verdict.copies_outside` (ADR-4, 2026-11-11). Empty for every other session.
    pub(crate) copies_outside: Vec<chrono_proto::FollowedProcess>,
    /// Set when the opening verdict refused the session and the core ended the application, with the
    /// processes of it that could not be ended, from `verdict.left_running` (R4-S5). `None` for every
    /// session that was not refused.
    pub(crate) refused: Option<Vec<chrono_proto::FollowedProcess>>,
    /// With `refused`, whether the core could not look for all of the application's processes, from
    /// `verdict.family_search_incomplete`. The line that says the application was ended then says that some
    /// of it may still run.
    pub(crate) refusal_incomplete: bool,
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
    /// Processes the family spawned and the hook did not follow into - each ran on the real clock
    /// (SLOWNIK `uncoveredChild`). Named up to the wire cap, with the true total beside them.
    pub(crate) uncovered_children: Vec<chrono_proto::UncoveredChild>,
    pub(crate) uncovered_children_total: u32,
    /// Set when the session did not end by itself: the driver stopped the core at a limit, or the core
    /// stopped before it closed the session ([`CutShort`]). Everything else in this report is
    /// then the LAST SNAPSHOT the core managed to send, not a final account, and the difference is
    /// not small - a real run cut at 30 s reported `QueryPerformanceCounter (634)` for a target that
    /// went on to make more than a million calls. Counts read as totals unless something says
    /// otherwise, so this makes the report say it rather than leaving it to be inferred from an exit
    /// code (untouchable rules 4 and 6).
    pub(crate) stopped_early: Option<CutShort>,
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
        "fails" => "DID NOT TAKE EFFECT - nothing this app read came from the session clock",
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
        "session.family_partial_children" => {
            "the session clock reached the processes listed as covered - the ones this app spawned without the hook ran on the real clock"
        }
        "session.family_uncovered_children" => {
            "no process read a covered time channel, and the ones this app spawned without the hook ran on the real clock"
        }
        "session.family_handed_to_copy" => {
            "the target most likely handed its work to a copy of itself running outside the session, on the real clock"
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
        // The target's own start failed before the hook got its turn (R4-S6). Windows's error window is
        // kept off while the target loads, so each line says what to try next, as the GUI's texts for these
        // keys do, and the two whose error window would name a file or a function say how to see that name.
        "target.loader_dll_not_found" => {
            "the target could not start: Windows did not find a library (DLL) it needs (0xC0000135) - run it once without chrono and Windows names the file"
        }
        "target.loader_entry_missing" => {
            "the target could not start: a library (DLL) it loads lacks a function it needs, usually a different version (0xC0000139) - run it once without chrono and Windows names both"
        }
        "target.loader_bad_image" => {
            "the target could not start: a library (DLL) it needs is built for the other bitness or damaged (0xC000007B) - check that it starts without chrono from the same working directory"
        }
        "target.loader_init_failed" => {
            "the target could not start: a library (DLL) it loads at start-up failed to initialise (0xC0000142) - check that it starts without chrono"
        }
        "target.died_while_loading" => {
            "the target ended while Windows was still loading it - the code it ended with is on the chrono core line on stderr"
        }
        "target.bitness_mismatch" => {
            "the target and this chrono.exe are different bitness - run the chrono.exe that matches the target"
        }
        "target.attach_failed" => "the target could not be attached to (Chromium/Electron, CDP)",
        "moment.invalid" => "the requested moment is not a valid date and time",
        // Both answer a start AND a jump, so neither says which of the two did not happen.
        "moment.out_of_range" => {
            "that date is outside the range a session can represent (years 1601 to 30828), so it was not used"
        }
        "time.bad_zone" => {
            "the time zone sent to the core is missing or outside the range it accepts (-14:59 to +14:59)"
        }
        "time.bad_mode" => "the requested time mode is not one this core knows",
        "time.bad_multiplier" => "the requested speed is outside the range this core accepts",
        "protocol.version_mismatch" => "the client and the core speak different protocol versions",
        "protocol.no_command" | "protocol.bad_command" | "protocol.expected_start" => {
            "the core did not receive a usable start command"
        }
        // A rejected jump leaves the session running on the clock it had, so each says both halves:
        // what was refused, and that nothing moved.
        "moment.needs_calendar" => {
            "that jump counts business days, and a running session has no calendar - it was not made, so jump to a date instead or work the date out with chrono calc first"
        }
        "moment.unsupported_kind" => {
            "that jump named a kind of moment this core does not know, so it was not made and the clock stayed where it was"
        }
        // Two ways a command is dropped without ending anything, and two ways the command stream itself
        // ends. The difference is the whole point for the reader: the first pair leaves the session
        // running, the second pair is why it stopped.
        "protocol.bad_command_ignored" => {
            "the core received a line it could not read as a command and ignored it - the session went on"
        }
        "protocol.unsupported_command" => {
            "the core received a command it does not act on at that point and ignored it - the session went on"
        }
        "protocol.line_too_long" => {
            "a line sent to the core was longer than the protocol allows, so the core stopped reading commands and the session ended"
        }
        "protocol.stream_unreadable" => {
            "what was sent to the core was not text (not UTF-8), so the core stopped reading commands and the session ended"
        }
        // The fallback line `Event::to_ndjson` writes in place of an event it could not serialize, so
        // whichever event that was never arrived.
        "proto.serialize_failed" => "the core could not write one of its messages, so part of this report may be missing",
        _ => "",
    }
}

/// What the report says of a registry value the session set and could not take away. The same words
/// stand in `describe_warning` (a session that ran) and `describe_residue` (a start that never ran):
/// either way the value is still on the machine.
const POLICY_VALUE_LEFT: &str = "Chrono Mock could not remove the WebView2 option it set in the machine registry for this application - it is under HKLM\\SOFTWARE\\Policies\\Microsoft\\Edge\\WebView2\\AdditionalBrowserArguments, in a value named after the application's file, and it keeps a debugging port open for that application in every run until you delete it";

/// The words for a key of the Chromium session (`chromium.*`), also said of the pages an embedded
/// engine shares the mechanism with. Split from `describe_warning`, whose length is the workspace's
/// ceiling, so the keys of one mechanism stand together and the next one does not push it over.
fn describe_chromium_warning(key: &str) -> &'static str {
    match key {
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
        "chromium.zone_is_host" => {
            "a page or worker of the app read this machine's time zone instead of the session's at some point, so a local time it showed could be off by the zone offset - the instant it read was still the session's"
        }
        // A session that outlives the program the tester named is a surprise unless it is explained
        // (R4-S18). Unlike `session.followed_family`, nothing but the debugging connection holds this
        // session, so the text makes no claim about helpers keeping it open.
        "chromium.followed_browser" => {
            "the target closed after handing the application over to another program, so the session went on with that program instead of ending with the target"
        }
        // R5-P6, accepted with the wave (R5-D45): a repeating timer takes a new speed from its next tick.
        "chromium.rate_change_affects_running_timers" => {
            "the speed changed in flight - the clock and new timers took it at once, a repeating timer from its next tick, and a one-time timer that was already waiting finished its wait at the old speed"
        }
        // R5-P6, R5-D7, accepted with the wave (R5-D45).
        "chromium.timer_rate_capped" => {
            "some timers would have to run more often than once every 4 ms at this speed - the browser does not run timers that often, so they ran slower than the chosen speed (the clock itself kept it)"
        }
        // R5-P5, R5-D47, R5-D48, accepted after the frame probe (R5-D49).
        "chromium.frames_unreached" => {
            "some frames inside a page were out of the session's reach - such a frame keeps the clock it started with (the real one, if it was there before the session), does not follow a jump or a change of speed, and its calls are not counted"
        }
        // Accepted by the owner before the code (R4/16, 2026-10-05), with the flag named in it.
        "chromium.background_timers_slowed" => {
            "a window of the app was hidden or minimised while time ran faster, and Chromium slows the timers of a hidden window to about once a second - its timers may have run slower than the session speed while it was hidden (start the session with --keep-background-timers to switch that off)"
        }
        // R4-S19, R4-D4: said rather than reached. The verdict speaks of the pages the audit sees.
        "chromium.main_process_uncovered" => {
            "this is an Electron app: its windows run on the session clock, but its main process (the Node.js part outside the windows) reads the real clock - a date it works out there, a reminder it schedules or a check it makes at start follows the real date and time"
        }
        "chromium.clock_move_missed" => {
            "a web page took a speed change or a jump late or not at all - usually because it was busy - so its clock stands apart from the session clock until the next jump, and reloading it may bring back an older one"
        }
        // R5-P7: covered from the moment the shim arrived, not from the context's start.
        "chromium.context_reached_running" => {
            "some pages or workers were already running when the session reached them - scripts they ran before that may have read the real clock, and a timer they set before keeps its real-time interval"
        }
        // R5-P20, R5-D8: the ceiling is the audit's, not the coverage's - the row this names is many.
        "chromium.contexts_pooled" => {
            "the application opened more than 256 web pages and workers - every one of them ran on the session clock, and the ones past the 256th are counted together in the last context row above"
        }
        // R5-F10, accepted with the slice before the code (R5-D60).
        "chromium.contexts_unread" => {
            "some pages or workers ended before the audit could read them (it reads once a second) - what they called is not in the counts"
        }
        // R5-F9, accepted with the slice before the code (R5-D60), moved in R5/5d.
        "chromium.engine_stamps_real" => {
            "a page read the time the browser stamps on animation frames and events (requestAnimationFrame, Event.timeStamp, document.timeline.currentTime) - those stamps run at real speed while performance.now runs at the session speed, so the two disagree"
        }
        "chromium.animations_real" => {
            "a page played CSS or Web Animations while the session ran faster - animations play at real speed, so they ran slower than the session clock"
        }
        _ => "",
    }
}

/// The words for a key about the session as a whole (`session.*`): how it ended, what it lasted for, and
/// what of the tester's environment it did not hand on to the application. Split from
/// `describe_warning` for the reason `describe_chromium_warning` is: its length is the workspace's
/// ceiling, and the third key here (R5-D3) would have pushed it over.
fn describe_session_warning(key: &str) -> &'static str {
    match key {
        // The session does not stop the application (docs/01 section 8.4), so the tester is left with
        // one that changed clocks under their hands. Says which way each clock went, because "back to
        // real time" alone reads as "the counters went back too" - and it was exactly that, a 316 s
        // step back at x60, that letting go used to do. A repeating timer is the exception, and it is
        // named: its period was shortened once, when it was set, and the release changes only what is
        // read or set from then on.
        "session.left_running" => {
            "the application was still running when the session ended, so it is back on the real date and time now - its tick counts and elapsed-time counters carry on at normal speed from where the session left them instead of jumping back, a repeating timer it set while its timers were sped up keeps the shorter interval it was given, and a restart gives it a clean run on the real clock"
        }
        // A session that outlives the program the tester named is a surprise unless it is explained -
        // a launcher is the case it exists for, a helper that never ends is the case it warns about.
        "session.followed_family" => {
            "the target closed while programs it had started were still running on the session clock, so the session went on for them instead of ending with it - the programs listed under 'followed' are the ones it went on for, and one that keeps running after the application closes keeps the session open too"
        }
        // A member the core could not wait on used to count as ended, and the session could end under
        // it without a word (R5-D3). Said, not fixed: nothing this side of the process can watch it.
        "session.member_unwatchable" => {
            "some processes of this session could not be watched (access denied), so the session may have ended while they still ran - from then on they ran on the real clock"
        }
        // The one finding that says the session may have tested nothing, so it says what to do (ADR-4,
        // 2026-11-11). "Most likely": no process says what it handed on, the situation is what is read.
        "session.handed_to_copy" => {
            "the target ended without ever showing a window, and when it ended a copy of it was running outside the session - a single-instance application most likely handed its work to that copy, listed under 'copies', which runs on the real clock, so this session may not have tested anything - to test the program under the session, close every copy of it and start the session again"
        }
        // The same question left unanswered: said apart from a copy found, and with the same way out.
        "session.copy_search_failed" => {
            "the target ended without ever showing a window, and the process list could not be read, so the session could not check whether it handed its work to a copy of itself running outside the session - if the program did not open, close every copy of it and start the session again"
        }
        // The verdict is about the clock and stays what the family earned, but a crash under a verdict of
        // works read as a session that went well. The session may be the cause, and only a run without it
        // can tell.
        "session.target_crashed" => {
            "the application crashed - it ended with the code Windows gives a program stopped by an error it did not handle (the exit code is above) - the verdict speaks only of the clock it read until then - start it once without Chrono Mock to see whether it crashes there too"
        }
        // Said before the launch: the runtimes that read TZ take their zone from it before the system's,
        // so a TZ of the tester's own would have replaced the session zone in exactly those programs. It is
        // the tester's VALUE that was not passed: a program built on jiff does get TZ, the session's.
        "session.tz_variable_removed" => {
            "the value of TZ set in your environment was not passed to the application - the programs that read TZ (Python and other programs on the C runtime, MSYS2, Node.js) take their time zone from it, so it would have replaced the session zone - a program built on the Rust jiff crate gets the session zone in TZ instead, any other program gets no TZ - your own environment is unchanged, and the value left out is on stderr"
        }
        // ADR-26. Says what the tester sees and what to choose instead, because the cure is a choice of
        // offset or date, not anything in the application.
        "session.zone_unnamed" => {
            "the programs built on ICU (Node.js, Deno, Bun and others) cannot name the session's time zone at this offset on the date the session starts, so some of them show the right time without a time zone name and others show it in UTC - for them to name it, choose a whole-hour offset from -12:00 to +14:00, or the offset of a real Windows time zone without daylight saving time with the session starting on or after the date that zone took that offset (+05:30 from 1945-10-15, for example)"
        }
        // ADR-26. ICU read the zone of the borrowed name with its history, once, so the jump is what moved
        // those programs off the session's offset, and it cannot be undone while they run. The cure is the
        // date of the next jump.
        // ADR-27. The hook could not queue the call that has the C runtimes read the zone again, so a runtime
        // that read it while the program's libraries started keeps it. Whether one did cannot be seen.
        "session.c_runtime_zone_kept" => {
            "the application's C runtime could not be made to read the session's time zone again when it started, so if one of its libraries asked for the time while it loaded, the local time the application shows can follow this machine's time zone and daylight saving rules instead of the session's"
        }
        "session.jump_before_zone_name" => {
            "for the programs built on ICU (Node.js, Deno, Bun and others) to name the session's time zone, the session gave it the registry name of a real Windows time zone with the same offset, and a jump took the clock to a date before that zone had this offset - at such a date those programs can show the offset the real zone had then instead of the session's, so to keep the session's offset in them, jump only to dates on or after the date that zone took it (1945-10-15 for +05:30)"
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
        // The floor is coverage with a cost, so it is named like the other two opt-in costs above it.
        // Without this line the key itself reached the report, which is the raw-jargon shape the GUI
        // side had just been cleaned of.
        "wait.timeout_collapsed" => {
            "some waits were too short to divide by the full speed factor and ran at this tool's \
             shortest step (1 ms) instead - that part of the application did not accelerate"
        }
        "timer.interval_floored" => {
            "some timers were too short to divide by the full speed factor and fired at the shortest \
             interval allowed (10 ms for window timers, set by Windows, 1 ms for the others) - that part \
             of the application ran slower than the rest of the session"
        }
        // Sits next to the object-wait line on purpose: that one says an I/O timeout is not shortened,
        // and for a library that measures its own timeout from the tick count this one says otherwise.
        "wait.network_timeouts_scaled" => {
            "this application opened a network connection - a network library that measures its own \
             timeout from the tick count (WinHTTP, for one) follows the session speed, so a server slower \
             than that timeout divided by the speed makes a request fail, even though the waits \
             underneath stay real"
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
        "inheritance.children_uncovered" => {
            "processes this app spawned ran on the REAL clock - the hook never got into them; they are listed above by name"
        }
        "embedded.web_engine_uncovered" => {
            "one of those is a Chromium renderer - the process web pages run in - so the pages hosted by that renderer read the real clock: a native session cannot reach a sandboxed renderer"
        }
        "embedded.web_engine_processes_uncovered" => {
            "those include processes of an embedded web engine (WebView2 or Qt WebEngine), so part of that engine ran on the real clock - whether its pages did could not be established, because no renderer was among the ones named"
        }
        // The embedded-engine channel (docs/09 section 12): the pages inside a natively hooked
        // application, reached through the engine's own debugging port.
        "embedded.web_engine_reached" => {
            "one of those is a Chromium renderer - the process web pages run in - and its pages were reached through the engine's debugging port and ran on the session clock (the context rows above) - the renderer's own native reads and the engine's helper processes ran on the real clock"
        }
        "embedded.debug_port_open" => {
            "a local debugging port was opened in this application's web engine for the session, which is how its pages were put on the session clock - it stays open to other programs on this computer for as long as the engine runs (the port is listed under the engines reached)"
        }
        // R5-B7, R5-D5: the environment of a process cannot be taken back, so a restart is the remedy.
        "embedded.variables_outlive_session" => {
            "the application is still running with the setting that makes its web engine open a debugging port - a web view it opens from now on opens that port too, reachable by other programs on this computer, until the application is restarted"
        }
        "embedded.engine_unreachable" => {
            "a web engine inside this application opened a debugging port and answered as one, but the session could not attach to it - its pages ran on the real clock"
        }
        "embedded.discovery_unavailable" => {
            "the session could not look for web engines inside this application (no loopback port to reserve, or the table of listening sockets could not be read), so any pages inside it ran on the real clock"
        }
        "embedded.qt_port_taken" => {
            "the port reserved for a Qt WebEngine debugging endpoint was held by something else by the time the engine would have bound it, so its pages could not be reached and ran on the real clock"
        }
        // Said only when a page READ another zone: every page is put on the session's zone, and the
        // shim in it checks the offset it reads (R4/16).
        "embedded.zone_is_host" => {
            "a page inside this application read this machine's time zone instead of the session's at some point, so a local time it showed could differ from the rest of the application, which reads the session zone"
        }
        "embedded.registry_arguments_hidden" => {
            "a WebView2 AdditionalBrowserArguments policy value for this application exists in the registry, and the session's environment variable hid it for the session - any flags it carried (a debugging port of your own, say) were not applied"
        }
        "embedded.webview2_not_reached" => {
            "the application loaded WebView2, but the session never reached its web engine, so its pages may have run on the real clock"
        }
        "embedded.elevated_host" => {
            "Chrono Mock is running as administrator, so this application is too, and WebView2 then ignores the setting the session reaches its pages through - run again with --elevated-embedded, or run Chrono Mock without administrator rights"
        }
        // The one WebView2 value a session may write to the machine registry for an application that
        // runs as administrator (docs/09 section 12.19), and what became of it.
        "embedded.policy_value_removed" => {
            "Chrono Mock set a WebView2 option for this application in the machine registry for the session, so the pages of an application running as administrator could be reached, and removed it when the session ended"
        }
        "embedded.policy_value_left" => POLICY_VALUE_LEFT,
        "embedded.policy_value_recovered" => {
            "a WebView2 option set by an earlier Chrono Mock session that ended abruptly was still in the machine registry, and was removed before this session started"
        }
        "embedded.policy_value_stale" => {
            "a WebView2 option set by an earlier Chrono Mock session that ended abruptly is still in the machine registry, and it keeps a debugging port open for the application it names - run Chrono Mock as administrator with --elevated-embedded once to remove it, or delete the value under HKLM\\SOFTWARE\\Policies\\Microsoft\\Edge\\WebView2\\AdditionalBrowserArguments that contains --chrono-mock-session"
        }
        "embedded.policy_value_foreign" => {
            "the machine registry already has a WebView2 option for this application that Chrono Mock did not set, so it was left as it is - the session could reach the application's pages only if that option opens a debugging port"
        }
        "embedded.policy_not_written" => {
            "Chrono Mock could not set the WebView2 option that lets the session reach the pages of an application running as administrator (the reason is on stderr), so those pages ran on the real clock"
        }
        "embedded.policy_name_mismatch" => {
            "Chrono Mock set the WebView2 option for the program it started, but WebView2 was loaded by a different program, which the option does not apply to - start that program directly to reach its pages"
        }
        "coverage.pid_registry_full" => {
            "this session ran more processes than the audit can follow (4096) - the ones past that limit were still put on the session clock, but the audit could not check them, so they are missing from the process count and the channel lists below"
        }
        // Says what the verdict above it does NOT say. The verdict answers "did the substitution take
        // effect", and it can be `works` over an application that read its time somewhere we cannot
        // reach - straight out of the shared page, through a direct syscall, or off a server. The
        // channels are covered, the counts are simply all zero, and without this line a reader takes
        // the verdict for proof that their application ran on the fake date.
        // The only key here that names a channel OUT OF REACH rather than one deliberately left
        // real. It also has to say what DID reach the target, because half a substitution is the
        // confusing part: the date is the real one, rendered in the session's time zone.
        "runtime.go_wall_clock_unreachable" => {
            "this target was built with the Go toolchain, whose runtime reads the wall clock straight out of shared kernel memory instead of calling a function - so the date it sees is the REAL one and no hook can change that, while the session time zone DOES reach it, which makes the times it shows the real moment expressed in the session's zone"
        }
        "coverage.session_clock_never_read" => {
            "not one process in this session ever read a clock the session substitutes, so nothing the application did was driven by the fake date - the channels listed above are installed and working, they were never asked"
        }
        "time.fake_clock_clamped" => {
            "the fake clock reached the last date this build can represent (year 30828) and stood there for the rest of the session, so late readings are not the moments the rate would have produced"
        }
        // A separate line from the one above, because it is a separate axis reaching a separate limit.
        // The wall stops at the end of the FILETIME range - the monotonic axes stop when their elapsed
        // term fills an i64, which at the maximum speed is about ten days in.
        "time.duration_axis_clamped" => {
            "the monotonic counters (tick count, interrupt time, and QPC when scaled) reached the end of their range and stood there, so elapsed time inside the target stopped advancing even though the session went on"
        }
        // The session clock stands still while the machine sleeps (ADR-5), and the log says so (docs/01
        // section 3.0a). `warning_line` puts the length of the sleep in when the verdict carried it.
        "time.host_slept" => {
            "the computer slept during the session - the session clock stood still while it slept, as during a pause (sleep is never multiplied by the speed factor)"
        }
        // R5-D2, R5-D35: said only when a deadline was converted BEFORE the clock moved, the one case in
        // which a converted deadline can fire at a moment the application did not mean.
        "timer.absolute_due_converted" => {
            "this application set a timer or a wait for a given date and time, and the session then jumped or changed speed - such a deadline is turned into a delay when it is set, at the speed in force then, so one still pending when the clock moved fires where the old clock would have reached it, while Windows moves its own such timers along with the clock"
        }
        k if k.starts_with("session.") => describe_session_warning(k),
        "embedded.pages_not_released" => {
            "a page inside the application did not confirm it was handed back to the real clock when the session ended, so it may keep the session date until it is reloaded or closed"
        }
        "embedded.zone_kept" => {
            "the application outlived the session, and some of its pages may still be on the session's time zone - asked after the session, they did not show this machine's time zone, so a local time they show can differ from the rest of the application by the zone offset until the application restarts"
        }
        "inheritance.child_not_injected" => {
            "a child process could not be covered and ran on the REAL clock - usually a child of the other bitness; the process count below is short by that many"
        }
        "source.network_at_start" => {
            "the target opened a network connection - it may read time from a server, which no local hook can cover"
        }
        // Says what it costs the READER, not what happened inside. The channels are covered and listed
        // like any other, so without this line their call counts read as totals when they are floors.
        // They are named in their own list above, because a runtime that loads winmm or ws2_32 brings
        // several at once and the old "a time channel" left the reader to guess which.
        "coverage.channel_installed_late" => {
            "the channels listed above as hooked only once their module loaded miss the calls made before that, so their counts are floors - and a scaled one made a single jump when it joined the session clock"
        }
        k if k.starts_with("chromium.") => describe_chromium_warning(k),
        "runtime.python_monotonic_qpc" => {
            "this Python app measures time with perf_counter and monotonic - both use QueryPerformanceCounter on Python 3.13+, which is left real, so a timer built on them does not scale (time.time and the wall clock do)"
        }
        "runtime.python_perfcounter_qpc" => {
            "this Python app's perf_counter uses QueryPerformanceCounter, which is left real, so a perf_counter timer does not scale - on Python 3.13+ monotonic uses QPC too (time.time and the wall clock do scale)"
        }
        "runtime.dotnet_stopwatch_qpc" => {
            "this .NET app's Stopwatch uses QueryPerformanceCounter, which is left real, so a Stopwatch timer does not scale (DateTime does, and Environment.TickCount does with --scale-duration)"
        }
        "runtime.java_nanotime_qpc" => {
            "this Java app's System.nanoTime uses QueryPerformanceCounter, which is left real, so a nanoTime timer does not scale (System.currentTimeMillis does)"
        }
        "qpc.scaled_render_may_distort" => {
            "QueryPerformanceCounter is being scaled (--scale-qpc), so a QPC-timed monotonic/elapsed clock accelerates - but a target that times its rendering or animation off QPC may look distorted"
        }
        // The one limit in this list that no coverage can lift, which is exactly why it has to be said.
        // Every other warning here is about a clock we did or did not reach - this one is about what the
        // target does with a clock it DID see: it refuses to take a step that large.
        "runtime.unity_delta_time_capped" => {
            "a Unity game: the engine caps how far it advances the simulation in a single frame (Time.maximumDeltaTime), so the gameplay hits a ceiling of its own and stops following the session speed well below it - the wall clock, dates and timestamps still move at the session rate, and no amount of clock coverage raises that ceiling"
        }
        _ => describe_thread_warning(key),
    };
    if text.is_empty() {
        key.to_string()
    } else {
        format!("{text} ({key})")
    }
}

/// The words for a key about calls the hook let by on a thread it could not keep its flags on. Split from
/// `describe_warning` for the reason `describe_chromium_warning` is, its length being the workspace's
/// ceiling, and reached through its last arm, so no line is added there.
fn describe_thread_warning(key: &str) -> &'static str {
    match key {
        "wait.thread_without_storage" => {
            "some waits or timers ran on a thread without thread-local storage - one Windows was ending, or one started without it - where this tool cannot tell an application's call from one Windows makes inside another, so it let them run at real speed and that part of the application did not accelerate"
        }
        _ => "",
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
        "cleanup.chromium_process_left" => {
            "Chrono Mock could not confirm that the browser it started had closed - if a process of it is still running, end it in Task Manager"
        }
        "embedded.policy_value_left" => POLICY_VALUE_LEFT,
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

/// The code a program ends with when Windows ends it on Ctrl+C or a closed console: an error status,
/// but the program was stopped, not broken.
const STATUS_CONTROL_C_EXIT: u32 = 0xC000_013A;

/// The code a program ends with when it hit a breakpoint no debugger took - a warning status, and still
/// a crash.
const STATUS_BREAKPOINT: u32 = 0x8000_0003;

/// Whether a process that ended with `code` crashed: Windows ends a program stopped by an exception it
/// did not handle with that exception's status as its exit code. An error status of Windows itself
/// (0xC0000005 an access violation, 0xC0000409 a fail-fast, 0xC00000FD a stack overflow, 0xC0000374 a
/// corrupted heap), an error status of a runtime (0xE06D7363 a C++ exception, 0xE0434352 a .NET one) and
/// a breakpoint nobody took are all a crash. A program may end with such a number on purpose, and that
/// is read as a crash too - outside the process nothing tells the two apart.
///
/// Not a crash: Ctrl+C or a closed console, and a negative number a program returns itself (-1, -2),
/// whose reserved bit no status has.
pub(crate) fn ended_by_crash(code: i32) -> bool {
    let status = code as u32;
    let windows_error = status & 0xF000_0000 == 0xC000_0000 && status != STATUS_CONTROL_C_EXIT;
    let runtime_error = status & 0xF000_0000 == 0xE000_0000;
    windows_error || runtime_error || status == STATUS_BREAKPOINT
}

/// Statically detect what the target is built on and warn about the limits that follow from it. Two
/// families today: a language runtime whose monotonic/elapsed clocks stand on QueryPerformanceCounter,
/// which is left real by default (ADR-2) and so does not scale, and a game engine that caps its own
/// simulation step, which no amount of clock coverage can lift. This reads file NAMES beside the target
/// (and its PyInstaller `_internal/` folder) plus bounded reads of the target's own executable (`pe`:
/// Go build info, the marks a .NET build leaves in the file): no QPC hook (ADR-2 holds), no process
/// inspection. Best-effort: a runtime unpacked at runtime (PyInstaller onefile), a `java -jar` launch,
/// a NativeAOT build published with `DebuggerSupport` off that also exports functions of its own, or
/// a launcher that starts the real binary from another folder is not caught here, and a false
/// positive only adds a "may not scale" note, never a false verdict (rules 4, 6).
pub(crate) fn detect_runtime_warnings(target_path: &std::path::Path, scale_qpc: bool) -> Vec<String> {
    let mut keys = fingerprint_target(target_path);

    if scale_qpc {
        // Under --scale-qpc the QPC axis IS scaled (A1), so a "monotonic/elapsed does not scale" warning
        // would be WRONG (B1 suppressed - it would contradict the feature). The render caution takes its
        // place: scaling QPC can distort a target that times its rendering off it, which is the risk ADR-2
        // guarded against, and it holds for any target rather than for a fingerprint.
        //
        // Dropping that FAMILY is not the same as dropping the fingerprint, and until now it was: this
        // function returned early, so everything else it had learned about the target went with it. The
        // target most likely to be run with --scale-qpc is a game, and a game is exactly the one the
        // engine warning below is for, so the warning reached the target that needs it least often.
        keys.retain(|k| !is_qpc_axis_warning(k));
        keys.insert(0, "qpc.scaled_render_may_distort".to_string());
    }

    keys
}

/// Whether a key belongs to the "this runtime reads elapsed time from QPC, which is left real" family -
/// the one `--scale-qpc` makes untrue. Spelled out rather than pattern-matched on the key text, because a
/// prefix rule would quietly swallow the next warning that happens to be named like these.
fn is_qpc_axis_warning(key: &str) -> bool {
    matches!(
        key,
        "runtime.python_monotonic_qpc"
            | "runtime.python_perfcounter_qpc"
            | "runtime.dotnet_stopwatch_qpc"
            | "runtime.java_nanotime_qpc"
    )
}

/// The folders whose files say something about the target: its own, and PyInstaller's `_internal/`
/// inside it. A bare name has an EMPTY parent rather than none - `Path::new("game.exe").parent()` is
/// `Some("")` - and `read_dir("")` fails, which read as "no runtime files" for the commonest way to
/// name a target: measured, `chrono run game.exe --dry-run` beside a `jvm.dll` warned of nothing, and
/// the same file by full path warned of Java (R4-S13). A bare name is started from the current
/// directory (`CreateProcessW` does not search for `lpApplicationName`), so that is the folder read.
fn dirs_beside(target_path: &std::path::Path) -> Vec<std::path::PathBuf> {
    let Some(parent) = target_path.parent() else {
        return Vec::new();
    };
    let parent = if parent.as_os_str().is_empty() { std::path::Path::new(".") } else { parent };
    vec![parent.to_path_buf(), parent.join("_internal")]
}

/// Everything the target's files say about it, independent of the session's flags. One walk of each
/// directory, never one per family: `read_dir` on Windows does not stat its entries (`FindNextFile`
/// hands back the name with the attributes), which is why this costs 0,2 ms on a real target folder and
/// is worth keeping to a single pass. The two looks inside the executable itself live in `pe`.
fn fingerprint_target(target_path: &std::path::Path) -> Vec<String> {
    fn add(keys: &mut Vec<String>, key: &str) {
        if !keys.iter().any(|k| k == key) {
            keys.push(key.to_string());
        }
    }

    let mut keys: Vec<String> = Vec::new();

    // First, because it is the only entry here that says a channel is OUT OF REACH rather than
    // merely left real. The others caution about an axis we chose not to scale - this one is a
    // clock we cannot touch at all.
    if crate::pe::is_go_binary(target_path) {
        add(&mut keys, "runtime.go_wall_clock_unreachable");
    }

    // A .NET executable that leaves no runtime file beside it for the loop below to find: NativeAOT,
    // any apphost including a framework-dependent single file, and .NET Framework, whose runtime lives
    // in the Windows directory. Its Stopwatch reads QPC all the same - in .NET the class lives in the
    // shared part of CoreLib, and in .NET Framework it calls QueryPerformanceCounter directly.
    if crate::pe::is_dotnet_executable(target_path) {
        add(&mut keys, "runtime.dotnet_stopwatch_qpc");
    }

    // The target executable's own name is a strong signal - a plain interpreter launcher, or the .NET
    // host that runs `dotnet app.dll` with the shared runtime, which sits nowhere near it on disk.
    if let Some(name) = target_path.file_name().and_then(|n| n.to_str()) {
        match name.to_ascii_lowercase().as_str() {
            "python.exe" | "pythonw.exe" => add(&mut keys, "runtime.python_perfcounter_qpc"),
            "java.exe" | "javaw.exe" => add(&mut keys, "runtime.java_nanotime_qpc"),
            "dotnet.exe" => add(&mut keys, "runtime.dotnet_stopwatch_qpc"),
            _ => {}
        }
    }

    // Runtime DLLs and manifests shipped beside the exe, and in PyInstaller's `_internal/` folder.
    for dir in dirs_beside(target_path) {
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
                // perf_counter is on QPC and monotonic (GetTickCount64) still scales under
                // --scale-duration, the flag every duration clock off QPC needs.
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
            } else if name == "unityplayer.dll" {
                // The engine ships as this one file beside the player executable, in both scripting
                // backends, and it is the marker our own import scan already leaned on. The ENGINE is named
                // here for the same reason .NET and Java are two branches up: the warning is about that
                // runtime's own clock behaviour, and a warning that will not say which runtime cannot be
                // acted on. No tested application is ever named (untouchable rules 5 and 26).
                add(&mut keys, "runtime.unity_delta_time_capped");
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
/// The caveat that has to stand above a cut-short run's numbers, or nothing at all. Its own function
/// because `render_report` is pinned at the shape ceiling and the next branch in it needs a decision
/// rather than a line - which is what the ceiling is for.
fn render_cut_short(stopped_early: Option<CutShort>) -> String {
    let Some(which) = stopped_early else {
        return String::new();
    };
    let why = match which {
        CutShort::Timeout => "the --timeout ceiling was reached",
        CutShort::Idle => "the core went quiet and was stopped",
        // The core died or was killed before it sent `ended` (R4-W3). The driver did not stop it, so
        // this is said apart from the two limits the driver enforces itself.
        CutShort::CoreLost => "the core stopped before it closed the session",
    };
    format!(
        "  stopped:  CUT SHORT - {why}\n\
         \x20           everything below is the last snapshot the core sent before that, so the\n\
         \x20           call counts are a FLOOR, not a total, and channels the target had not\n\
         \x20           reached yet are missing rather than absent\n"
    )
}

/// What a vanish is suspected to be, by its reason key. A target that started a program the hook
/// could not enter and closed is a hand-off, not a single-instance application (ADR-16), and the
/// line that used to say "single-instance" for every vanish said it of that one too.
///
/// An unknown key yields "" like every other gloss here. The fallback used to be the single-instance
/// suspicion itself, so a reason added to a newer core would have been explained as that by an older
/// driver - an explanation the core never sent.
fn vanish_cause(reason_key: &str) -> &'static str {
    match reason_key {
        "target.single_instance_suspected" => "suspected single-instance app",
        "target.handed_off_uncovered" => {
            "it started a program the session could not enter, usually one of the other bitness, which runs on the real clock"
        }
        "target.crashed" => {
            "it crashed - it ended with the code Windows gives a program stopped by an error it did not handle (the exit code is below) - start it once without Chrono Mock to see whether it crashes there too"
        }
        _ => "",
    }
}

/// The cause and the key for the vanish line, or the key alone when this driver cannot name the cause.
/// Its own function so the branch does not land in `render_report`, which is at the shape ceiling.
fn vanish_detail(reason_key: &str) -> String {
    match vanish_cause(reason_key) {
        "" => reason_key.to_string(),
        cause => format!("{cause}: {reason_key}"),
    }
}

/// The line for the target's own exit code. A crash is said as one: "closed itself" read as a program
/// that ended the way it meant to, under a verdict of works, for one stopped by an access violation.
fn render_target_exit(code: i32) -> String {
    if ended_by_crash(code) {
        format!(
            "  exited:   the target CRASHED - it ended with code {}, the code Windows gives a program stopped by an error it did not handle\n",
            exit_code_label(code)
        )
    } else {
        format!("  exited:   the target closed itself with code {}\n", exit_code_label(code))
    }
}

/// The processes the session went on for after the target closed (ADR-16), or nothing. Under the
/// line that says the target closed, because that line alone reads as the end of the session.
fn render_followed(followed: &[chrono_proto::FollowedProcess]) -> String {
    if followed.is_empty() {
        return String::new();
    }
    // "went on for", not "until they closed": a Stop or `--ticks` can end the session while one of them
    // still runs, and `session.left_running` says so on its own line.
    let mut out = String::from("  followed: the target had started these, and the session went on for them:\n");
    push_processes(&mut out, followed);
    out
}

/// The copies outside the session the target most likely handed its work to (ADR-4, 2026-11-11), or
/// nothing. They are why the verdict is not `works`, and the line under the verdict says so in its reason.
fn render_copies(copies: &[chrono_proto::FollowedProcess]) -> String {
    if copies.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "  copies:   the target most likely handed its work to these copies of it, running outside the session on the REAL clock:\n",
    );
    push_processes(&mut out, copies);
    out
}

/// One line per process under a heading, by name and pid, or by pid alone when the list had no name.
fn push_processes(out: &mut String, processes: &[chrono_proto::FollowedProcess]) {
    for p in processes {
        match &p.image {
            Some(image) => out.push_str(&format!("            - {image} (pid {})\n", p.pid)),
            None => out.push_str(&format!("            - pid {}\n", p.pid)),
        }
    }
}

/// What a refusal did to the application, or nothing when the session was not refused (R4-S5). The
/// verdict above it says the substitution did not take effect, and this is what followed from that:
/// the application was ended, except the processes named here, which could not be. The report used to
/// say nothing of it, so a reader could not tell a refused run from one that went on.
fn render_refusal(refused: Option<&[chrono_proto::FollowedProcess]>, incomplete: bool) -> String {
    let Some(left) = refused else {
        return String::new();
    };
    let mut out = String::from("  refused:  the substitution did not take effect, so the session ended the application\n");
    if incomplete {
        out.push_str("            not every process the application started could be looked for - some may still run on the REAL clock\n");
    }
    if !left.is_empty() {
        out.push_str("            except these, which it could not end - they run on the REAL clock:\n");
    }
    push_processes(&mut out, left);
    // The way past the refusal, said where the refusal is (R5, D4-1). The window has the same choice as
    // a box on its start screen, with its own explanation there.
    out.push_str("            to run it anyway, start it again with --force - the report then marks the session unreliable\n");
    out
}

/// One heading plus its pid-tagged channel names, or nothing when the list is empty. Shared by the
/// two name-only buckets so `render_report` stays under its pinned complexity ceiling - the ceiling
/// asked for this, and lifting a repeated shape out is the cheaper of its two answers.
/// What the headline adds after the covered count when some of the family was never reached:
/// ", plus N not covered". Empty when there is nothing to add, so the headline of a clean session
/// reads exactly as it always did.
fn uncovered_children_suffix(total: u32) -> String {
    if total == 0 {
        String::new()
    } else {
        format!(", plus {total} not covered")
    }
}

/// The processes the family spawned and the hook did not follow into, one line each with the
/// image name when the child was still alive to be asked, and the parent that spawned it - so a
/// reader can see WHICH part of the application ran on the real clock, not just that some did.
/// Empty when there were none. A total above the named list is said in words rather than left as
/// a mismatch for the reader to notice.
fn render_uncovered_children(children: &[chrono_proto::UncoveredChild], total: u32) -> String {
    if children.is_empty() && total == 0 {
        return String::new();
    }
    let mut out = String::new();
    out.push_str("  processes this app spawned that the hook could not follow (they ran on the REAL clock):\n");
    for c in children {
        let image = c.image.as_deref().unwrap_or("(exited before it could be named)");
        let role = c.role.as_deref().map(|r| format!("{r}, ")).unwrap_or_default();
        out.push_str(&format!("            - pid {} {image} ({role}spawned by pid {})\n", c.pid, c.parent_pid));
    }
    let named = children.len() as u32;
    if total > named {
        out.push_str(&format!("            - and {} more not named\n", total - named));
    }
    out
}

fn render_channel_names(heading: &str, rows: &[(UnitId, String)]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let mut out = format!("{heading}\n");
    for (unit, ch) in rows {
        out.push_str(&format!("            - {unit}: {ch}\n"));
    }
    out
}

/// The DevTools endpoints the session reached inside the application, one line each, or nothing.
/// The port is the actionable half: a debugging port stands open in the application for as long as
/// its engine runs, and a tester who wants their own DevTools on those pages attaches to this one.
///
/// The heading says what the session's page count says (R5, P15). It used to say that their pages ran
/// on the session clock whenever an engine was reached, also when no page was put on it - the session
/// attached to an engine whose pages it then could not shim, or found them all on the clock already
/// through the hook (their rows are under their process then). The count is one for every engine, so
/// the heading never claims every page.
fn render_engines(engines: &[chrono_proto::ReachedEngine], context_count: u32) -> String {
    if engines.is_empty() {
        return String::new();
    }
    let mut out = String::from(if context_count > 0 {
        "  web engines reached inside the app (pages put on the session clock through them are the context rows above):\n"
    } else {
        "  web engines reached inside the app (none of their pages was put on the session clock through the debugging port):\n"
    });
    for e in engines {
        let name = if e.browser.is_empty() { "engine" } else { e.browser.as_str() };
        out.push_str(&format!("            - {name} on port {} (pid {})\n", e.port, e.pid));
    }
    out
}

/// What the headline adds after the process count when the session covered JS contexts beside its
/// processes: ", contexts: N". Empty otherwise, so a report without an embedded engine reads exactly
/// as it always did, and a Chromium report - whose count IS contexts - does not say it twice.
fn contexts_suffix(cdp: bool, context_count: u32) -> String {
    if cdp || context_count == 0 {
        String::new()
    } else {
        format!(", contexts: {context_count}")
    }
}

/// The covered and the observed channels with their call counts. A count that stopped at the hook's
/// ceiling reads "at least" (R5-WY11), and so does the total above the covered rows when one of them
/// did - the audit never states more calls than there were, and never a ceiling as if it were a count.
fn render_counted_channels(r: &SessionReport) -> String {
    let mut out = String::new();
    let count = |unit: &UnitId, ch: &str, calls: u64| {
        if r.capped.iter().any(|(u, c)| u == unit && c == ch) {
            format!("at least {}", calls_label(calls))
        } else {
            calls_label(calls)
        }
    };
    if !r.covered.is_empty() {
        // The total belongs in the heading, not under the rows. There are dozens of these and the one
        // number a reader needs first is whether the application asked for the time at all - a wall of
        // "(0 calls)" says it too, but only to whoever reads every line. Saturating because a long
        // session at speed can count high and a wrapped total would read as silence.
        let total = r.covered.iter().fold(0u64, |sum, (_, _, calls)| sum.saturating_add(*calls));
        let floor = r.covered.iter().any(|(unit, ch, _)| r.capped.iter().any(|(u, c)| u == unit && c == ch));
        out.push_str(&format!(
            "  covered channels (substituted, with call counts, {}{} in all):\n",
            if floor { "at least " } else { "" },
            calls_label(total)
        ));
        for (unit, ch, calls) in &r.covered {
            out.push_str(&format!("            - {unit}: {ch} ({})\n", count(unit, ch, *calls)));
        }
    }
    if !r.observed.is_empty() {
        out.push_str("  observed channels (hooked but left real):\n");
        for (unit, ch, calls) in &r.observed {
            out.push_str(&format!("            - {unit}: {ch} ({})\n", count(unit, ch, *calls)));
        }
    }
    out
}

/// A warning as the report prints it: `describe_warning`, except the sleep, which also says how long
/// the machine slept when the session verdict carried the number (R5-P1, R5-D34).
fn warning_line(key: &str, host_slept_ms: u64) -> String {
    if key == "time.host_slept" && host_slept_ms > 0 {
        return format!(
            "the computer slept for {} during the session - the session clock stood still while it slept, \
             as during a pause (sleep is never multiplied by the speed factor) ({key})",
            sleep_label(host_slept_ms)
        );
    }
    describe_warning(key)
}

/// A length of sleep as a reader takes it in: hours and minutes from an hour up, minutes and seconds from
/// a minute up, seconds below that.
pub(crate) fn sleep_label(ms: u64) -> String {
    let s = ms / 1_000;
    match (s / 3_600, s % 3_600 / 60, s % 60) {
        (0, 0, sec) => format!("{sec} s"),
        (0, min, sec) => format!("{min} min {sec} s"),
        (h, min, _) => format!("{h} h {min} min"),
    }
}

/// The line under the target that names the browser a Chromium session drove (R5-D44), empty for a
/// session that drove none. The version is part of the evidence: a page behaves by its engine.
fn render_browser(browser: Option<&str>) -> String {
    browser.map(|b| format!("  browser:  {b}\n")).unwrap_or_default()
}

pub(crate) fn render_report(r: &SessionReport) -> String {
    let mut out = String::from("Chrono Mock - session report\n");
    out.push_str(&format!("  target:   {}\n", r.target));
    out.push_str(&render_browser(r.browser.as_deref()));
    // A Chromium session's coverage unit is a JS context, not an OS process (rule 4 - never sum
    // across units either way). Each row names its own unit - a native session can hold both kinds.
    let units = if r.cdp { "contexts" } else { "processes" };

    // Before the verdict, not after the channel lists: a caveat that arrives once the reader has
    // already read the numbers as totals has arrived too late.
    out.push_str(&render_cut_short(r.stopped_early));

    // Headline priority: a vanish is an honest non-effect, then the family verdict, then the
    // parent verdict as a fallback for an older core, then nothing.
    if let Some((reason_key, lived_ms)) = &r.vanished {
        out.push_str("  verdict:  DID NOT TAKE EFFECT - the target vanished right after injection\n");
        out.push_str(&format!("            ({}, lived {lived_ms} ms)\n", vanish_detail(reason_key)));
    } else if let Some((verdict, reason_key, count)) = &r.session_verdict {
        out.push_str(&format!(
            "  verdict:  {}  ({units}: {count}{}{})\n",
            verdict_headline(verdict),
            uncovered_children_suffix(r.uncovered_children_total),
            contexts_suffix(r.cdp, r.context_count)
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
    // Right under the verdict it follows from.
    out.push_str(&render_refusal(r.refused.as_deref(), r.refusal_incomplete));

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
        out.push_str(&render_target_exit(code));
    }
    out.push_str(&render_followed(&r.followed));
    out.push_str(&render_copies(&r.copies_outside));

    out.push_str(&render_counted_channels(r));

    out.push_str(&render_channel_names(
        "  uncovered channels (queried but not covered):",
        &r.uncovered,
    ));
    // Its own heading, and the wording is the point: this is not a gap in the substitution, it is a
    // watch that did not start. Reading it as an uncovered channel would suggest the session missed
    // time the target asked for, which it did not.
    out.push_str(&render_channel_names(
        "  channels we meant to watch and could not hook:",
        &r.unobserved,
    ));
    // Named, and above the warning that explains them: each is also listed with its count, and this is
    // the line that says which of those counts are floors.
    out.push_str(&render_channel_names(
        "  channels hooked only once their module loaded (their counts are floors):",
        &r.installed_late,
    ));
    out.push_str(&render_uncovered_children(&r.uncovered_children, r.uncovered_children_total));
    out.push_str(&render_engines(&r.engines, r.context_count));

    if !r.warnings.is_empty() {
        out.push_str("  warnings:\n");
        for w in &r.warnings {
            out.push_str(&format!("            - {}\n", warning_line(w, r.host_slept_ms)));
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
#[derive(Default)]
pub(crate) struct EvidenceParams {
    pub(crate) moment: String,
    pub(crate) zone: String,
    pub(crate) mode: String,
    /// The run itself, which the file used to leave out (R5, D4-5).
    pub(crate) run: RunFacts,
}

/// What a reader of an evidence file needs to know about the run besides the time it ran at: which
/// build ran what, when, where and with which switches (R5, D4-5). The file is the one a tester hands
/// on, and before this it named the moment and the mode but not the program it ran. Every field is
/// text the driver composed, rendered as it is.
#[derive(Default)]
pub(crate) struct RunFacts {
    /// The version and the core's bitness, "0.4.0, 64-bit core".
    pub(crate) build: String,
    /// When the run started, in UTC from the real clock - the driver is not hooked.
    pub(crate) started_utc: String,
    pub(crate) target: String,
    /// The folder the target started in, and whether it was asked for or inherited.
    pub(crate) folder: String,
    /// See [`arguments_line`].
    pub(crate) arguments: String,
    /// The switches the run was given beyond its moment, zone and mode, or "none".
    pub(crate) flags: String,
}

/// The arguments line of an evidence file (R5-D16): how many the target got, and the arguments
/// themselves only when the run asked for them with `--report-args`. An argument can carry a password
/// or a token, and the file is made to be handed to someone, so the count is the default. Recorded
/// arguments are quoted with escapes, so one with a space, a quote or a control character reads back
/// exactly and cannot pass for a line of the report.
pub(crate) fn arguments_line(args: &[String], record: bool) -> String {
    match (args.len(), record) {
        (0, _) => "none".to_string(),
        (n, true) => format!("{n} - {args:?}"),
        (n, false) => format!("{n}, left out (--report-args writes them word for word)"),
    }
}

/// The warnings that undo a WORKS verdict as evidence (R5-D3, P14). The verdict answers whether the
/// substitution took effect on the channels it covers, and it stands - but the file a tester cites
/// claims more: that what the application did came from the session clock. A Go program reads its
/// wall clock where no hook reaches, and a session in which no process ever read a clock the session
/// substitutes proves nothing about time at all. Mirrored by the window (`SessionViewModel.IsReliable`).
pub(crate) const WARNINGS_THAT_UNDO_EVIDENCE: [&str; 2] =
    ["runtime.go_wall_clock_unreachable", "coverage.session_clock_never_read"];

/// A clean WORKS session (no vanish, no partial/fails, no warning that undoes it as evidence).
/// Anything else must carry the unreliable banner in an evidence export - evidence that hides doubt is
/// worse than none (8.8).
pub(crate) fn session_is_reliable(r: &SessionReport) -> bool {
    // A run the driver cut short is not evidence of anything, whatever verdict happened to arrive
    // before the knife. The core emits the parent's verdict early, so a timed-out session CAN carry a
    // WORKS line - and without this the evidence file would have printed it with no banner at all.
    if r.stopped_early.is_some() {
        return false;
    }
    if r.vanished.is_some() {
        return false;
    }
    if r.warnings.iter().any(|w| WARNINGS_THAT_UNDO_EVIDENCE.contains(&w.as_str())) {
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
pub(crate) fn mode_label(mode: chrono_proto::TimeModeKind, multiplier: Option<i64>) -> String {
    match mode {
        chrono_proto::TimeModeKind::Multiplier => format!("x{}", multiplier.unwrap_or(1)),
        chrono_proto::TimeModeKind::Flow => "flow".to_string(),
        chrono_proto::TimeModeKind::Frozen => "frozen".to_string(),
        chrono_proto::TimeModeKind::Unknown => "unknown".to_string(),
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
    let f = &p.run;
    out.push_str(&format!(
        "  chrono:     {}\n  started:    {} (real clock)\n  target:     {}\n  folder:     {}\n  arguments:  {}\n  flags:      {}\n",
        f.build, f.started_utc, f.target, f.folder, f.arguments, f.flags
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::map_prepare_error;
    use crate::testutil::unique_temp_dir;

    /// A bare name is looked up in the current directory, where the session starts it - its empty
    /// parent used to make the look fail and say nothing about the runtime (R4-S13).
    #[test]
    fn a_bare_target_name_is_fingerprinted_in_the_current_directory() {
        use std::path::{Path, PathBuf};
        let here = Path::new(".");
        assert_eq!(dirs_beside(Path::new("game.exe")), vec![here.to_path_buf(), here.join("_internal")]);
        assert_eq!(
            dirs_beside(Path::new(r"C:\games\game.exe")),
            vec![PathBuf::from(r"C:\games"), PathBuf::from(r"C:\games\_internal")]
        );
        assert!(dirs_beside(Path::new(r"C:\")).is_empty(), "a root has no folder beside it to read");
    }

    fn pid(n: u32) -> UnitId {
        UnitId { unit: Unit::Process, id: n }
    }

    fn context(n: u32) -> UnitId {
        UnitId { unit: Unit::Context, id: n }
    }

    fn empty_report() -> SessionReport {
        SessionReport {
            target: "app.exe".into(),
            session_verdict: None,
            parent_verdict: None,
            vanished: None,
            errors: vec![],
            warnings: vec![],
            context_count: 0,
            engines: vec![],
            followed: vec![],
            copies_outside: vec![],
            refused: None,
            refusal_incomplete: false,
            uncovered: vec![],
            unobserved: vec![],
            installed_late: vec![],
            covered: vec![],
            observed: vec![],
            capped: vec![],
            host_slept_ms: 0,
            browser: None,
            timing: None,
            target_exit: None,
            residue: vec![],
            uncovered_children: vec![],
            uncovered_children_total: 0,
            cdp: false,
            stopped_early: None,
        }
    }

    /// R5-D44: the browser a Chromium session drove is named under the target, and a session that
    /// drove none has no such line.
    #[test]
    fn the_browser_a_chromium_session_drove_is_named_under_the_target() {
        let r = SessionReport { cdp: true, browser: Some("Chrome/152.0.7977.149".into()), ..empty_report() };
        let text = render_report(&r);
        assert!(text.contains("  target:   app.exe\n  browser:  Chrome/152.0.7977.149\n"), "{text}");
        assert!(!render_report(&empty_report()).contains("browser:"));
    }

    /// The processes the hook never got into have their own heading, each named with its role when
    /// the command line gave one, its parent, and a stated absence of a name when the child was gone
    /// before it could be asked. The headline carries the count beside the covered one, and a total
    /// above the named list is said in words.
    #[test]
    fn uncovered_children_are_listed_by_name_role_and_parent() {
        let r = SessionReport {
            session_verdict: Some(("partial".into(), "session.family_partial_children".into(), 3)),
            uncovered_children: vec![
                chrono_proto::UncoveredChild {
                    pid: 8072,
                    parent_pid: 28016,
                    image: Some("engine.exe".into()),
                    role: Some("renderer".into()),
                },
                chrono_proto::UncoveredChild { pid: 8073, parent_pid: 28016, image: None, role: None },
            ],
            uncovered_children_total: 5,
            ..empty_report()
        };
        let text = render_report(&r);
        assert!(text.contains("PARTIAL - time substitution took effect only in part  (processes: 3, plus 5 not covered)"), "{text}");
        assert!(text.contains("processes this app spawned that the hook could not follow (they ran on the REAL clock):"), "{text}");
        assert!(text.contains("- pid 8072 engine.exe (renderer, spawned by pid 28016)"), "{text}");
        assert!(text.contains("- pid 8073 (exited before it could be named) (spawned by pid 28016)"), "{text}");
        assert!(text.contains("- and 3 more not named"), "{text}");

        let clean = render_report(&empty_report());
        assert!(!clean.contains("could not follow"), "a clean session has no such heading");
    }

    #[test]
    fn cdp_report_labels_the_unit_as_context() {
        let r = SessionReport {
            session_verdict: Some(("works".into(), "chromium.contexts_covered".into(), 2)),
            covered: vec![(context(1), "page setInterval".into(), 5)],
            warnings: vec!["chromium.launched_with_debug_port".into()],
            cdp: true,
            context_count: 2,
            ..empty_report()
        };
        let out = render_report(&r);
        assert!(out.contains("contexts: 2"), "got:\n{out}");
        assert!(!out.contains("contexts: 2, contexts"), "a Chromium report does not say its count twice: {out}");
        assert!(out.contains("- context 1: page setInterval"), "got:\n{out}");
        assert!(out.contains("JS context ran on the session clock"), "got:\n{out}");
        assert!(out.contains("remote-debugging port"), "got:\n{out}");
    }

    /// A native session that reached an embedded web engine holds both kinds of unit, and the same
    /// number in the two namespaces is two rows: `pid 4` is a process the hook is inside, `context 4`
    /// is a page reached over the DevTools protocol (docs/09 section 12.4). The headline says how many
    /// contexts stood beside the processes, and the engines block names the port the pages were
    /// reached through - the actionable half for a tester who wants their own DevTools on them.
    #[test]
    fn a_native_report_names_each_row_by_its_own_unit() {
        let r = SessionReport {
            session_verdict: Some(("partial".into(), "session.family_partial_children".into(), 3)),
            covered: vec![
                (pid(4), "GetSystemTimeAsFileTime".into(), 9),
                (context(4), "page Date.now".into(), 12),
            ],
            context_count: 2,
            engines: vec![chrono_proto::ReachedEngine {
                pid: 8072,
                port: 51234,
                browser: "Engine/1.0".into(),
                context_count: 2,
            }],
            ..empty_report()
        };
        let out = render_report(&r);
        assert!(out.contains("processes: 3, contexts: 2"), "got:\n{out}");
        assert!(out.contains("- pid 4: GetSystemTimeAsFileTime (9 calls)"), "got:\n{out}");
        assert!(out.contains("- context 4: page Date.now (12 calls)"), "got:\n{out}");
        assert!(out.contains("- Engine/1.0 on port 51234 (pid 8072)"), "got:\n{out}");

        // No engine reached, no contexts: the report reads exactly as it always did.
        let plain = render_report(&SessionReport {
            session_verdict: Some(("works".into(), "session.family_covered".into(), 1)),
            ..empty_report()
        });
        assert!(plain.contains("(processes: 1)"), "got:\n{plain}");
        assert!(!plain.contains("web engines"), "got:\n{plain}");
    }

    /// The engines heading says only what the page count says (R5, P15): pages put on the session clock
    /// when there are some, and that none was when an engine was reached but no page was shimmed. It
    /// used to say their pages ran on the session clock in both cases.
    #[test]
    fn the_engines_heading_claims_pages_only_when_some_were_put_on_the_clock() {
        let reached = |contexts: u32| SessionReport {
            session_verdict: Some(("works".into(), "session.family_covered".into(), 1)),
            context_count: contexts,
            engines: vec![chrono_proto::ReachedEngine {
                pid: 8072,
                port: 51234,
                browser: "Engine/1.0".into(),
                context_count: contexts,
            }],
            ..empty_report()
        };
        let with_pages = render_report(&reached(2));
        assert!(with_pages.contains("(pages put on the session clock through them are the context rows above):"), "got:\n{with_pages}");
        let without = render_report(&reached(0));
        assert!(
            without.contains("(none of their pages was put on the session clock through the debugging port):"),
            "got:\n{without}"
        );
        for out in [&with_pages, &without] {
            assert!(!out.contains("their pages ran on the session clock"), "the old claim is back, got:\n{out}");
            assert!(out.contains("- Engine/1.0 on port 51234 (pid 8072)"), "got:\n{out}");
        }
    }

    /// A kind this build does not know - a newer core naming a unit this driver has never heard of -
    /// is neither a process nor a context. Folding it into `pid N` would put a claim in the report
    /// the core never made, and dropping it would lose evidence, so the row stays and says what it is.
    #[test]
    fn a_coverage_kind_this_build_does_not_know_is_not_called_a_pid() {
        assert_eq!(UnitId::from_wire("process", 8).unit, Unit::Process);
        assert_eq!(UnitId::from_wire("context", 8).unit, Unit::Context);
        assert_eq!(UnitId::from_wire("thread", 8).unit, Unit::Unknown);
        let text = UnitId::from_wire("thread", 8).to_string();
        assert!(!text.starts_with("pid"), "{text}");
        assert!(text.contains("does not know"), "{text}");

        let r = SessionReport {
            covered: vec![(UnitId::from_wire("thread", 8), "SomeChannel".into(), 3)],
            ..empty_report()
        };
        let out = render_report(&r);
        assert!(out.contains("- unit 8 (of a kind this build does not know): SomeChannel (3 calls)"), "got:\n{out}");
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
        assert!(out.contains("suspected single-instance app"), "got:\n{out}");
    }

    /// A target that started a program the hook could not enter and closed is a hand-off. The line
    /// said "suspected single-instance app" for every vanish, this one included (ADR-16).
    #[test]
    fn a_hand_off_to_an_uncovered_program_is_not_called_single_instance() {
        let r = SessionReport {
            vanished: Some(("target.handed_off_uncovered".into(), 15)),
            ..empty_report()
        };
        let out = render_report(&r);
        assert!(out.contains("DID NOT TAKE EFFECT"), "got:\n{out}");
        assert!(out.contains("could not enter"), "got:\n{out}");
        assert!(!out.contains("single-instance"), "got:\n{out}");
    }

    /// A reason this driver does not know is shown as the key alone. The fallback used to be the
    /// single-instance suspicion, which put an explanation the core never sent over a newer core's
    /// reason (the contract of every gloss here is "unknown key, no invented prose").
    #[test]
    fn an_unknown_vanish_reason_is_shown_without_an_invented_cause() {
        let r = SessionReport {
            vanished: Some(("target.some_future_reason".into(), 40)),
            ..empty_report()
        };
        let out = render_report(&r);
        assert!(out.contains("DID NOT TAKE EFFECT"), "got:\n{out}");
        assert!(out.contains("(target.some_future_reason, lived 40 ms)"), "got:\n{out}");
        assert!(!out.contains("single-instance"), "got:\n{out}");
    }

    /// Every error key a running session can send is explained rather than printed raw, and the two
    /// halves the reader needs are there: a dropped command left the session running, a broken
    /// command stream is why it stopped.
    #[test]
    fn a_refused_command_and_a_broken_command_stream_are_told_apart() {
        for key in ["protocol.bad_command_ignored", "protocol.unsupported_command"] {
            assert!(describe_error(key).contains("the session went on"), "{key}");
        }
        for key in ["protocol.line_too_long", "protocol.stream_unreadable"] {
            assert!(describe_error(key).contains("the session ended"), "{key}");
        }
        for key in ["moment.needs_calendar", "moment.unsupported_kind"] {
            assert!(describe_error(key).contains("not made"), "{key}");
        }
        // The line sent in place of an event that could not be written says what is missing.
        assert!(describe_error("proto.serialize_failed").contains("may be missing"));
    }

    /// A target the loader ended is named with what to try next (R4-S6), the step the GUI gives for the
    /// same key: Windows's error window is kept off while the target loads, so the report is all there is.
    #[test]
    fn a_target_the_loader_ended_is_told_what_to_try_next() {
        for key in ["target.loader_dll_not_found", "target.loader_entry_missing"] {
            assert!(describe_error(key).contains("run it once without chrono and Windows names"), "{key}");
        }
        for key in ["target.loader_bad_image", "target.loader_init_failed"] {
            assert!(describe_error(key).contains("check that it starts without chrono"), "{key}");
        }
        assert!(describe_error("target.loader_bad_image").contains("from the same working directory"));
    }

    /// The processes the session went on for stand under the line that says the target closed, named
    /// when the process list had a name and by pid when it did not (ADR-16).
    #[test]
    fn the_processes_a_session_went_on_for_are_listed_under_the_exit() {
        let r = SessionReport {
            target_exit: Some(0),
            followed: vec![
                chrono_proto::FollowedProcess { pid: 5150, image: Some("app.exe".into()) },
                chrono_proto::FollowedProcess { pid: 5151, image: None },
            ],
            ..empty_report()
        };
        let out = render_report(&r);
        let exited = out.find("exited:").expect("the exit line");
        let followed = out.find("followed:").expect("the followed line");
        assert!(exited < followed, "got:\n{out}");
        assert!(out.contains("- app.exe (pid 5150)"), "got:\n{out}");
        assert!(out.contains("- pid 5151\n"), "got:\n{out}");
        // A Stop can end the session while one of them still runs, so the heading never says they closed.
        assert!(!out.contains("closed:"), "got:\n{out}");
        // Nothing followed, no line.
        assert!(!render_report(&empty_report()).contains("followed:"));
    }

    /// ADR-4, 2026-11-11. The copies a single-instance application most likely handed its work to stand
    /// under the processes the session went on for, each named, and say they ran on the real clock. The
    /// reason and the warning are glossed, so the report never shows a bare key.
    #[test]
    fn the_copies_outside_the_session_are_listed_and_their_keys_explained() {
        let r = SessionReport {
            target_exit: Some(0),
            copies_outside: vec![
                chrono_proto::FollowedProcess { pid: 4600, image: Some("app.exe".into()) },
                chrono_proto::FollowedProcess { pid: 4601, image: None },
            ],
            ..empty_report()
        };
        let out = render_report(&r);
        let exited = out.find("exited:").expect("the exit line");
        let copies = out.find("  copies:   ").expect("the copies line");
        assert!(exited < copies, "got:\n{out}");
        assert!(out[copies..].contains("REAL clock"), "got:\n{out}");
        assert!(out[copies..].contains("- app.exe (pid 4600)\n            - pid 4601\n"), "got:\n{out}");
        assert!(!render_report(&empty_report()).contains("copies:"));
        assert!(!describe_reason("session.family_handed_to_copy").is_empty());
        assert!(describe_warning("session.handed_to_copy").contains("start the session again"));
        assert!(describe_warning("session.copy_search_failed").contains("could not be read"));
    }

    /// R4-S5: a refused session says the application was ended, right under the verdict, and names what
    /// could not be ended. A session that was not refused says nothing of the kind.
    #[test]
    fn a_refusal_says_the_application_was_ended_and_names_what_was_not() {
        let refused = |left: Vec<chrono_proto::FollowedProcess>| SessionReport {
            parent_verdict: Some(("fails".into(), "coverage.time_channels_uncovered".into())),
            refused: Some(left),
            ..empty_report()
        };
        let out = render_report(&refused(Vec::new()));
        let verdict = out.find("verdict:").expect("the verdict line");
        let line = out.find("refused:  the substitution did not take effect, so the session ended the application\n");
        assert!(line.is_some_and(|at| verdict < at), "got:\n{out}");
        assert!(!out.contains("except these"), "got:\n{out}");

        let out = render_report(&refused(vec![
            chrono_proto::FollowedProcess { pid: 5150, image: Some("helper.exe".into()) },
            chrono_proto::FollowedProcess { pid: 5151, image: None },
        ]));
        assert!(out.contains("except these, which it could not end - they run on the REAL clock:\n"), "got:\n{out}");
        assert!(out.contains("            - helper.exe (pid 5150)\n"), "got:\n{out}");
        assert!(out.contains("            - pid 5151\n"), "got:\n{out}");
        assert!(!out.contains("could be looked for"), "a complete search was reported as incomplete:\n{out}");

        // A search that could not look for everything says so right under the line that says the
        // application was ended, so that line never stands alone as a claim of a full cleanup.
        let incomplete = SessionReport { refusal_incomplete: true, ..refused(Vec::new()) };
        let out = render_report(&incomplete);
        let ended = out.find("so the session ended the application\n").expect("the refused line");
        let caveat = out
            .find("            not every process the application started could be looked for - some may still run on the REAL clock\n")
            .expect("the incomplete search is not in the report");
        assert!(ended < caveat, "got:\n{out}");

        assert!(!render_report(&empty_report()).contains("refused:"));
    }

    /// A refusal names the way past it, after everything it says about what was ended - with or without
    /// processes left behind - and a session that was not refused does not (R5, D4-1).
    #[test]
    fn a_refusal_names_force_as_the_way_to_run_anyway() {
        const FORCE: &str = "            to run it anyway, start it again with --force - the report then marks the session unreliable\n";
        let refused = |left: Vec<chrono_proto::FollowedProcess>| SessionReport {
            parent_verdict: Some(("fails".into(), "coverage.time_channels_uncovered".into())),
            refused: Some(left),
            ..empty_report()
        };
        for left in [Vec::new(), vec![chrono_proto::FollowedProcess { pid: 5150, image: Some("helper.exe".into()) }]] {
            let named = !left.is_empty();
            let out = render_report(&refused(left));
            let force = out.find(FORCE).unwrap_or_else(|| panic!("no --force line, got:\n{out}"));
            assert!(out.find("refused:").is_some_and(|at| at < force), "got:\n{out}");
            if named {
                assert!(out.find("helper.exe").is_some_and(|at| at < force), "the way past comes after the list, got:\n{out}");
            }
        }
        assert!(!render_report(&empty_report()).contains("--force"));
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
            uncovered: vec![(pid(1234), "KUSER_SHARED_DATA".into())],
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
            covered: vec![(pid(1234), "GetSystemTime".into(), 7)],
            observed: vec![(pid(1234), "WaitForSingleObject".into(), 1)],
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
        assert!(out.contains("the target CRASHED"), "an access violation read as a program that closed itself:\n{out}");
        assert!(!out.contains("closed itself"), "got:\n{out}");
    }

    /// The codes a crash ends a program with, and the ones that only look like one (measured: a program
    /// stopped by an access violation under a session ended with 0xC0000005 and a verdict of works).
    #[test]
    fn a_crash_is_told_from_an_ordinary_exit_by_its_code() {
        for crash in [0xC000_0005u32, 0xC000_0409, 0xC000_00FD, 0xC000_0374, 0xC000_0142, 0xE06D_7363, 0xE043_4352, 0x8000_0003] {
            assert!(ended_by_crash(crash as i32), "0x{crash:08X} is a crash");
        }
        // Ctrl+C or a closed console, a program's own -1 and -2, and the ordinary codes.
        for exit in [0xC000_013Au32, 0xFFFF_FFFF, 0xFFFF_FFFE, 0, 1, 2, 259, 0x8000_0004, 0x4001_0004] {
            assert!(!ended_by_crash(exit as i32), "0x{exit:08X} is not a crash");
        }
        let ordinary = render_target_exit(1);
        assert!(ordinary.contains("closed itself with code 1"), "{ordinary}");
        let said = describe_warning("session.target_crashed");
        assert!(said.starts_with("the application crashed") && said.contains("without Chrono Mock"), "{said}");
        assert!(vanish_detail("target.crashed").starts_with("it crashed"));
    }

    #[test]
    fn cleanup_residue_is_reported_unknown_key_verbatim() {
        let r = SessionReport {
            session_verdict: Some(("works".into(), "chromium.contexts_covered".into(), 1)),
            residue: vec![
                "cleanup.chromium_profile_left".into(),
                "cleanup.chromium_process_left".into(),
                "cleanup.something_new".into(),
            ],
            cdp: true,
            ..empty_report()
        };
        let out = render_report(&r);
        assert!(out.contains("not fully cleaned up"), "got:\n{out}");
        assert!(out.contains("temporary Chromium profile"), "got:\n{out}");
        assert!(out.contains("cleanup.chromium_profile_left"), "got:\n{out}");
        assert!(out.contains("could not confirm that the browser it started had closed"), "got:\n{out}");
        assert!(out.contains("end it in Task Manager (cleanup.chromium_process_left)"), "got:\n{out}");
        // Same rule as warnings: a key we cannot name is still shown, never swallowed.
        assert!(out.contains("cleanup.something_new"), "got:\n{out}");
    }

    /// A registry value a start that never ran could not take away comes as residue, and the report says
    /// what it is where it would otherwise print the bare key.
    #[test]
    fn a_registry_value_left_by_a_start_that_never_ran_is_explained_as_residue() {
        let text = describe_residue("embedded.policy_value_left");
        assert!(text.contains("could not remove the WebView2 option"), "got: {text}");
        assert!(text.ends_with("(embedded.policy_value_left)"), "got: {text}");
        assert_eq!(text, describe_warning("embedded.policy_value_left"), "one account of it, wherever it is said");
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
            ..Default::default()
        };
        let out = render_evidence(&r, &p);
        assert!(out.contains("the target closed itself with code 2"), "got:\n{out}");
    }

    #[test]
    fn a_watch_that_never_started_gets_its_own_line_and_leaves_the_verdict_alone() {
        // What this closes: an observed channel whose hook failed used to produce NO line at all -
        // not covered, not uncovered, absent - so the report read as complete while a watch it had
        // promised was simply not running. Proved by taking the install out and watching the channel
        // vanish from the report rather than appear as a gap.
        //
        // It must NOT land in `uncovered`, which is a verdict input: a watch that did not start says
        // nothing about whether the substitution took effect, and folding the two together would turn
        // a failed observer into a PARTIAL session. Hence the separate bucket and this fixture's WORKS
        // verdict, which has to survive.
        let r = SessionReport {
            session_verdict: Some(("works".into(), "session.family_covered".into(), 1)),
            covered: vec![(pid(7), "GetSystemTime".into(), 4)],
            unobserved: vec![(pid(7), "WaitOnAddress".into())],
            ..empty_report()
        };

        let text = render_report(&r);
        assert!(text.contains("WaitOnAddress"), "the channel must be named, got:\n{text}");
        assert!(text.contains("meant to watch"), "and named as a watch, not a gap, got:\n{text}");
        assert!(
            !text.contains("uncovered channels"),
            "it must not be reported as an uncovered channel, got:\n{text}"
        );
        assert!(text.contains("WORKS"), "the verdict is untouched by it, got:\n{text}");
        assert!(session_is_reliable(&r), "a failed watch is not an unreliable session");
    }

    #[test]
    fn channels_hooked_late_are_named_above_the_warning_that_explains_them() {
        // What this closes: the warning said "a time channel was only hooked once its module loaded" and
        // named none, while a runtime that loads winmm or ws2_32 brings several channels at once - so the
        // reader had every row of the report to guess from. The names come from the core, and the report
        // prints them under their own heading, before the warning that says what they cost.
        let r = SessionReport {
            session_verdict: Some(("works".into(), "session.family_covered".into(), 1)),
            covered: vec![(pid(7), "timeGetTime".into(), 12)],
            installed_late: vec![(pid(7), "timeGetTime".into())],
            warnings: vec!["coverage.channel_installed_late".into()],
            ..empty_report()
        };

        let text = render_report(&r);
        let heading = text.find("hooked only once their module loaded").expect("the heading must be printed");
        let named = text[heading..].find("timeGetTime").expect("and the channel named under it");
        let warning = text.find("(coverage.channel_installed_late)").expect("and the warning itself");
        assert!(heading + named < warning, "the names come before the warning that explains them, got:\n{text}");
        assert!(text.contains("floors"), "and say what they cost, got:\n{text}");
    }

    /// R5-WY11. A count that stopped at the hook's ceiling reads "at least", on its row and in the total of
    /// the heading, covered and observed alike - and an exact count beside it stays exact.
    #[test]
    fn a_count_at_the_ceiling_reads_at_least_on_its_row_and_in_the_total() {
        let r = SessionReport {
            session_verdict: Some(("works".into(), "session.family_covered".into(), 1)),
            covered: vec![(pid(1), "GetSystemTimeAsFileTime".into(), 1_048_576), (pid(1), "GetSystemTime".into(), 3)],
            observed: vec![(pid(1), "WaitForSingleObject".into(), 1_048_576)],
            capped: vec![(pid(1), "GetSystemTimeAsFileTime".into()), (pid(1), "WaitForSingleObject".into())],
            ..empty_report()
        };
        let text = render_report(&r);
        assert!(text.contains("GetSystemTimeAsFileTime (at least 1048576 calls)"), "got:\n{text}");
        assert!(text.contains("GetSystemTime (3 calls)"), "an exact count beside it stays exact, got:\n{text}");
        assert!(text.contains("WaitForSingleObject (at least 1048576 calls)"), "got:\n{text}");
        assert!(text.contains("with call counts, at least 1048579 calls in all"), "got:\n{text}");

        // No row at the ceiling, no "at least" anywhere.
        let exact = SessionReport { capped: vec![], ..r };
        let text = render_report(&exact);
        assert!(!text.contains("at least"), "got:\n{text}");
    }

    /// R5-P1, R5-D34. The sleep warning says how long the machine slept when the verdict carried it.
    #[test]
    fn the_sleep_warning_says_how_long_the_machine_slept() {
        let r = SessionReport {
            session_verdict: Some(("works".into(), "session.family_covered".into(), 1)),
            warnings: vec!["time.host_slept".into()],
            host_slept_ms: 28_800_000,
            ..empty_report()
        };
        let text = render_report(&r);
        assert!(text.contains("the computer slept for 8 h 0 min during the session"), "got:\n{text}");
        assert!(text.contains("(time.host_slept)"), "got:\n{text}");
        assert_eq!(sleep_label(45_000), "45 s");
        assert_eq!(sleep_label(125_000), "2 min 5 s");
        assert_eq!(sleep_label(3_660_000), "1 h 1 min");
        // Without the number the warning still explains itself.
        let bare = SessionReport { host_slept_ms: 0, ..r };
        assert!(render_report(&bare).contains("the computer slept during the session"));
    }

    /// R5-A9: each way a run is cut short has a line of its own - the idle limit too, which no test read
    /// while it was the catch-all of a match on strings - and a run that ended by itself has none.
    #[test]
    fn each_way_a_run_is_cut_short_says_its_own_why() {
        let lines = [CutShort::Timeout, CutShort::Idle, CutShort::CoreLost].map(|why| render_cut_short(Some(why)));
        assert!(lines[0].contains("the --timeout ceiling was reached"), "{}", lines[0]);
        assert!(lines[1].contains("the core went quiet and was stopped"), "{}", lines[1]);
        assert!(lines[2].contains("the core stopped before it closed the session"), "{}", lines[2]);
        assert_eq!(render_cut_short(None), "");
    }

    #[test]
    fn a_run_cut_short_says_so_above_its_numbers_and_is_never_cited_as_proof() {
        // The failure this closes, measured on a real target: a 30 s --timeout printed
        // `QueryPerformanceCounter (634)` for an application that went on to make more than a million
        // calls, and the report gave the reader nothing to distinguish that from a total. Worse, the
        // core emits the parent's verdict EARLY, so the cut run can carry a WORKS line - which is why
        // this fixture has one. Without the caveat the evidence file would have printed a clean WORKS
        // report over a sample from the session's first blink.
        let r = SessionReport {
            session_verdict: Some(("works".into(), "session.family_covered".into(), 1)),
            covered: vec![(pid(1), "QueryPerformanceCounter".into(), 634)],
            stopped_early: Some(CutShort::Timeout),
            ..empty_report()
        };

        let text = render_report(&r);
        assert!(text.contains("CUT SHORT"), "got:\n{text}");
        assert!(text.contains("FLOOR"), "the counts must be named as a floor, got:\n{text}");
        // Ahead of the numbers, not after them: a caveat read once the reader has taken the counts for
        // totals has arrived too late.
        let caveat = text.find("CUT SHORT").expect("caveat");
        let count = text.find("QueryPerformanceCounter").expect("count");
        assert!(caveat < count, "the caveat must come before the counts, got:\n{text}");

        let p = EvidenceParams {
            moment: "2038-01-19T03:14:07".into(),
            zone: "+00:00".into(),
            mode: "x60".into(),
            ..Default::default()
        };
        let out = render_evidence(&r, &p);
        assert!(
            out.starts_with("!! UNRELIABLE EVIDENCE"),
            "a cut run is not proof of anything, whatever verdict arrived first, got:\n{out}"
        );
        assert!(!session_is_reliable(&r));
    }

    /// R4-W3, the fixture shaped like the measured run: the core was killed two seconds in, so the only
    /// verdict is the parent's from the guard window and the counts are from that window too. The report
    /// said WORKS over them and the evidence file carried it with no banner.
    #[test]
    fn a_run_whose_core_stopped_before_it_closed_the_session_says_so_and_is_never_cited_as_proof() {
        let r = SessionReport {
            parent_verdict: Some(("works".into(), "coverage.time_channels_covered".into())),
            covered: vec![(pid(1), "GetSystemTimeAsFileTime".into(), 7)],
            stopped_early: Some(CutShort::CoreLost),
            ..empty_report()
        };

        let text = render_report(&r);
        assert!(text.contains("CUT SHORT - the core stopped before it closed the session"), "got:\n{text}");
        let caveat = text.find("CUT SHORT").expect("caveat");
        let verdict = text.find("WORKS").expect("verdict");
        assert!(caveat < verdict, "the caveat must come before the verdict it qualifies, got:\n{text}");

        let p = EvidenceParams {
            moment: "2030-06-15T12:00:00".into(),
            zone: "+00:00".into(),
            mode: "flow".into(),
            ..Default::default()
        };
        let out = render_evidence(&r, &p);
        assert!(out.starts_with("!! UNRELIABLE EVIDENCE"), "a session the core never closed is not proof, got:\n{out}");
    }

    #[test]
    fn evidence_from_works_has_no_unreliable_banner_and_echoes_params() {
        let r = SessionReport {
            session_verdict: Some(("works".into(), "session.family_covered".into(), 1)),
            covered: vec![(pid(1), "GetSystemTime".into(), 2)],
            ..empty_report()
        };
        let p = EvidenceParams {
            moment: "2038-01-19T03:14:07".into(),
            zone: "+00:00".into(),
            mode: "x60".into(),
            ..Default::default()
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
            ..Default::default()
        };
        let out = render_evidence(&r, &p);
        assert!(out.starts_with("!! UNRELIABLE EVIDENCE"), "non-works must lead with the banner, got:\n{out}");
        assert!(out.contains("DID NOT TAKE EFFECT"), "got:\n{out}");
    }

    /// A WORKS verdict is not evidence when the application read its time where no hook reaches, or
    /// read no substituted clock at all (R5-D3, P14). The verdict stays, the file gets the banner - for
    /// each key alone, and not for a warning that leaves the evidence standing.
    #[test]
    fn a_works_session_that_proves_nothing_about_time_is_flagged_as_unreliable_evidence() {
        let works_with = |warning: &str| SessionReport {
            session_verdict: Some(("works".into(), "session.family_covered".into(), 1)),
            covered: vec![(pid(1), "GetSystemTime".into(), 2)],
            warnings: vec![warning.to_string()],
            ..empty_report()
        };
        for key in WARNINGS_THAT_UNDO_EVIDENCE {
            let r = works_with(key);
            assert!(!session_is_reliable(&r), "{key} left a works session reliable");
            let out = render_evidence(&r, &EvidenceParams::default());
            assert!(out.starts_with("!! UNRELIABLE EVIDENCE"), "{key}: no banner, got:\n{out}");
            assert!(out.contains("WORKS"), "the verdict itself is not changed, got:\n{out}");
        }
        assert!(session_is_reliable(&works_with("wait.timeout_collapsed")), "a caution is not a reason to doubt the file");
    }

    /// The evidence file names the run itself, not only the moment it ran at (R5, D4-5), and the target's
    /// arguments only on request, because one can carry a password (R5-D16).
    #[test]
    fn the_evidence_file_names_the_run_and_keeps_the_arguments_out_unless_asked() {
        let r = SessionReport {
            session_verdict: Some(("works".into(), "session.family_covered".into(), 1)),
            ..empty_report()
        };
        let p = EvidenceParams {
            moment: "2038-01-19T03:14:07".into(),
            zone: "+00:00".into(),
            mode: "x60".into(),
            run: RunFacts {
                build: "0.4.0, 64-bit core".into(),
                started_utc: "2026-10-13T19:05:12Z".into(),
                target: r"C:\apps\test.exe".into(),
                folder: r"C:\work".into(),
                arguments: arguments_line(&["--token".into(), "s3cret".into()], false),
                flags: "--scale-duration --ticks 10".into(),
            },
        };
        let out = render_evidence(&r, &p);
        for line in [
            "  chrono:     0.4.0, 64-bit core\n",
            "  started:    2026-10-13T19:05:12Z (real clock)\n",
            "  target:     C:\\apps\\test.exe\n",
            "  folder:     C:\\work\n",
            "  arguments:  2, left out (--report-args writes them word for word)\n",
            "  flags:      --scale-duration --ticks 10\n",
        ] {
            assert!(out.contains(line), "missing {line:?}, got:\n{out}");
        }
        assert!(!out.contains("s3cret"), "an argument reached the file without --report-args, got:\n{out}");

        assert_eq!(arguments_line(&[], false), "none");
        assert_eq!(arguments_line(&[], true), "none");
        let odd = ["a b".to_string(), "say \"hi\"".to_string(), "line\nbreak".to_string()];
        assert_eq!(arguments_line(&odd, true), r#"3 - ["a b", "say \"hi\"", "line\nbreak"]"#, "recorded arguments read back exactly");
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
        // Python 3.12: only perf_counter is on QPC - monotonic (GetTickCount64) still scales under
        // --scale-duration.
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

    #[test]
    fn detect_flags_the_engine_ceiling_a_session_cannot_lift() {
        // A game is the one target where covering every clock still does not deliver the speed asked for:
        // the engine clamps the step it advances the simulation by (Time.maximumDeltaTime, Unity's own
        // scripting reference), so the gameplay tops out on its own. Saying nothing would be the silent
        // non-coverage the product rules out - the report would read Works over an application that
        // visibly ignored the rate.
        let dir = unique_temp_dir("chrono-rt-engine");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("UnityPlayer.dll"), b"").unwrap();
        let target = dir.join("Game.exe");
        std::fs::write(&target, b"").unwrap();

        assert!(
            detect_runtime_warnings(&target, false)
                .iter()
                .any(|k| k == "runtime.unity_delta_time_capped")
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scale_qpc_drops_the_qpc_family_and_keeps_everything_else() {
        // The failure this pairing exists for. Suppressing the "does not scale" family under --scale-qpc is
        // right, and it used to be done with an early return - which threw the whole fingerprint away. A
        // game is the target most likely to be run with --scale-qpc AND the one that needs the engine
        // ceiling warning, so that warning reached it least often of all.
        let dir = unique_temp_dir("chrono-rt-engine-qpc");
        std::fs::create_dir_all(dir.join("_internal")).unwrap();
        std::fs::write(dir.join("_internal").join("python314.dll"), b"").unwrap();
        std::fs::write(dir.join("UnityPlayer.dll"), b"").unwrap();
        let target = dir.join("Game.exe");
        std::fs::write(&target, b"").unwrap();

        let keys = detect_runtime_warnings(&target, true);

        // The QPC family goes, because under the flag it would be untrue.
        assert!(
            !keys.iter().any(|k| k == "runtime.python_monotonic_qpc"),
            "the QPC family must not survive --scale-qpc, got: {keys:?}"
        );
        // The render caution and the engine ceiling both stay - one is about the flag, the other about
        // the target, and neither answers for the other.
        assert!(keys.iter().any(|k| k == "qpc.scaled_render_may_distort"), "got: {keys:?}");
        assert!(keys.iter().any(|k| k == "runtime.unity_delta_time_capped"), "got: {keys:?}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
