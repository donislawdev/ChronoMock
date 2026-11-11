//! The Chromium/Electron session: the second substitution mechanism, beside the native one.
//!
//! Where the native path injects a DLL and hooks 36 time channels, this one drives a debug port and
//! evaluates a time shim in every JS context (ADR-8, ADR-9). It speaks the same protocol and returns
//! the same verdicts, so the interface cannot tell which mechanism ran - only the coverage report can.
//!
//! Same boundary as `cdp_probe.rs`: `cdp/` is the transport client, this is the product using it.


use std::io::BufReader;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use chrono_proto::{
    Command, Event, FollowedProcess, TargetSpec, TimeSpec,
    PROTOCOL_VERSION,
};

use crate::cdp;
use crate::output::diag;
use crate::zone::{epoch_ms_to_wall, now_epoch_ms};
use crate::events::{
    emit, ended_after_launch, ended_clean, error_event, reported_sleep_ms, sleep_to_announce, SLEPT_REPORTED_MS,
};
use crate::wire::spawn_command_reader;
use crate::cdp_attach::{Attacher, Pumped, END_WAIT};
use crate::cdp_clock::{cdp_jump_target, cdp_schedule_expr, cdp_set_expr, resync_due, CdpClock, HostAxis};
use crate::session_commands::{apply_command, JumpTo, SessionClock};
use crate::cdp_audit::{
    covered_channels, coverage_events, cdp_verdict, session_warnings,
    verdict_keys, SessionFacts,
};

/// A Chromium/Electron session driven over the Chrome DevTools Protocol, speaking the SAME machine
/// protocol as the native core (ADR-9). It is the inverse of the old human-report `driver_run_cdp`:
/// launch + shim + audit, but every outcome travels as `state`/`coverage`/`session_verdict`/`ended`,
/// so the GUI and CLI consume a CDP session exactly like a native one. Interactive commands (`query`
/// now, `set_multiplier`/`jump` in slice C7) arrive on the same stdin the native session reads.
///
/// `--ticks` is a DRIVER concern (the driver counts `state` heartbeats and sends `end`), so this loop
/// runs until `end`, stdin EOF, or the app closing - exactly like `run_session`. The app closing is
/// its debugging connection closing, not the launched process ending: a launcher that hands the
/// application over and exits leaves the browser, and the session, running (R4-S18).
///
/// `start_id` is the id of the `start` that opened the session, which an error before the session runs
/// answers (R5-P18).
pub(crate) fn cdp_session(start_id: u64, target: TargetSpec, time: TimeSpec, reader: BufReader<std::io::Stdin>) -> i32 {
    // Resolve the fake-clock origin ONCE, in Unix-epoch ms, and share it with both the shim and every
    // `state` event, so the panel's fake clock matches what the app's own `Date.now()` reads (no skew
    // from the launch+attach duration). The moment is absolute here (the driver resolved a relative
    // --at before spawning) - an out-of-range moment is an honest error, never a silent fall-back to
    // real time (untouchable rule 4). The clock runs on interrupt time, which a sleep does not move, and
    // the pages are kept to it (R5-F6, `WallWatch`).
    let mut watch = WallWatch::start();
    let real_start_ms = watch.now();
    let bias = time.moment.tz_bias_min.unwrap_or(0);
    // The live session clock, computed Rust-side and kept in step with the shim: set_multiplier and
    // jump re-anchor it here and push the new origin to every context. The shim is built PER ATTACH from
    // this clock's current origin (below), so a context that attaches after an in-flight change starts on
    // the same clock as the others. A flag records whether a rate change happened in flight, so the end
    // report can carry the honest running-timer caveat (rule 4).
    let mut clock = match CdpClock::from_time_spec(&time, real_start_ms) {
        Ok(c) => c,
        Err(key) => {
            emit(&error_event(Some(start_id), 1, key, "core"));
            emit(&ended_clean());
            return 1;
        }
    };
    let mut rate_changed_in_flight = false;

    // Launch under our own isolated profile + debug port, then attach. Any failure is an honest error
    // event plus exit 2 (could not launch/attach), never a faked verdict.
    // The heartbeat starts here, not after the attach. Everything between accepting `start` and the
    // first `state` used to be silence, bounded by the port wait (15 s) - and the client's idle
    // watchdog is 15 s, so a slow Electron and a dead core looked the same to it. Measured on
    // the measured Electron app the gap is about 1.6 s, so this is about the tail, not the common case (R3-3).
    let mut last_beat = std::time::Instant::now();
    // The browser's own slowdown of timers in hidden windows is switched off when the session asked for
    // it (R4-N28) - otherwise a page hidden while time ran faster is reported below.
    let browser_args = cdp::with_background_timers(&target.args, target.keep_background_timers);
    let mut launched = match cdp::launch_chromium(&target.path, &browser_args, target.cwd.as_deref(), || {
        if last_beat.elapsed() >= std::time::Duration::from_secs(1) {
            last_beat = std::time::Instant::now();
            emit(&clock.state_event_at(watch.now()));
        }
    }) {
        Ok(l) => l,
        Err(e) => {
            emit(&error_event(Some(start_id), 2, "target.launch_failed", "mechanism"));
            diag!("chrono core: {e}");
            // A browser that never opened its port was ended and its profile removed before this - what
            // could not be removed is said, not covered by a `clean: true` (R5-Z1).
            emit(&ended_after_launch(e.residue));
            return 2;
        }
    };
    // The contexts, their counts and their indexes live in the attacher (cdp_attach) - the index
    // counter stays here, because it is the session's: several attachers in one session must hand
    // out disjoint indexes, and that is the shape the embedded-engine channel needs (docs/09).
    let mut next_index = 0u32;
    // The attacher connects to the browser endpoint, arms auto-attach and attaches by name to the
    // pages the browser already has - measured, auto-attach delivers those too, and the by-name pass
    // is the belt for an engine where it does not. Any failure is one honest error here, as before.
    // One deadline for all of it: no heartbeat beats in here (R4-N26).
    let start_by = Instant::now() + cdp::CONNECT_DEADLINE;
    // Every context goes on the session's zone, set before the first attach. The bias was checked when
    // the clock was built, so it is the zone the panel's fake wall is in.
    let mut attacher = match Attacher::connect_by_path("127.0.0.1", launched.port, launched.ws_path.as_deref(), start_by).and_then(|mut a| {
        a.set_zone(bias);
        a.attach_existing(clock.shim_origin(), &mut next_index, start_by).map(|_| a)
    }) {
        Ok(a) => a,
        Err(e) => {
            let residue = launched.shutdown_with_residue();
            emit(&error_event(Some(start_id), 2, "target.attach_failed", "mechanism"));
            diag!("chrono core: cannot attach over CDP: {e}");
            emit(&ended_after_launch(residue));
            return 2;
        }
    };
    // Which browser the evidence is about (R5-D44), asked now that the pages that existed are in.
    let browser = attacher.browser_version(Instant::now() + BROWSER_VERSION_WAIT);
    // No start verdict for CDP: unlike the native guard window, at start there is nothing to judge yet
    // (contexts attach asynchronously and have made no time calls). The authoritative verdict is the
    // family `session_verdict` at end - emitting "undetermined" now would read as "not working" when it
    // only means "not audited yet".

    // A stdin reader thread turns command lines into `Command`s (end/query/set_multiplier/jump) so the
    // main loop can interleave them with CDP polling and the heartbeat without blocking on read_line.
    let rx = spawn_command_reader(reader);

    // Install the shim into every context as it attaches (page and its Web Workers), beat a ~1 s
    // `state` heartbeat, and sample per-context call counts, until `end`, stdin EOF, or the app closes.
    let mut app_closed = false;
    let heartbeat = Duration::from_secs(1);
    let mut deadline = Instant::now() + heartbeat;
    let mut last_audit = Instant::now();
    let (port, launched_pid) = (launched.port, launched.pid());
    let mut handoff = Handoff::default();

    'session: loop {
        // The launched process ending does not end the session (R4-S18) - it is noted, with the
        // process that holds the debugging port from then on.
        let alive = launched.is_running();
        handoff.observe(alive, Instant::now(), || port_holder(port, launched_pid));
        // Where the system clock stands against the session's, the pages kept to it, a sleep said.
        watch.turn(&mut clock, &mut attacher, &mut next_index);
        // Drain protocol commands without blocking. A clock move waits for the pages up to
        // `MOVE_WAIT_MS`, so the heartbeat is looked at after every command rather than once the
        // queue is empty - commands back to back cannot add their waits into one silence (R4-S10).
        loop {
            match rx.try_recv() {
                Ok(Command::End { .. }) => break 'session,
                Ok(command) => {
                    let moved = apply_command(
                        &mut CdpCommandClock { clock: &mut clock, attacher: &mut attacher, next_index: &mut next_index, watch: &mut watch },
                        command,
                    );
                    rate_changed_in_flight |= moved;
                    beat(&clock, &mut deadline, heartbeat, watch.now());
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break 'session, // stdin closed (EOF)
            }
        }
        // Poll CDP (bounded by the poll interval) for a newly attached context and shim it, drop one
        // that went away, and take the answers that came. The shim is built from the clock's CURRENT
        // origin, so a context attaching after an in-flight rate change or jump starts on the same
        // clock as every other context (one absolute origin - rule 3).
        if attacher.pump(clock.shim_origin(), &mut next_index) == Pumped::Closed {
            app_closed = true; // the connection dropped, i.e. the app exited
            break;
        }
        // ~1 s heartbeat (also when frozen) and ~1 s coverage sampling, asked for without waiting.
        beat(&clock, &mut deadline, heartbeat, watch.now());
        if last_audit.elapsed() >= Duration::from_secs(1) {
            attacher.request_counts();
            last_audit = Instant::now();
        }
    }
    // Taken as the loop ends, before the settle below spends its wait: a launched process that ended
    // a few milliseconds before the connection closed is a browser that closed, not a handover.
    let followed = handoff.followed(Instant::now());
    say_why_closed(app_closed && launched.is_running(), &attacher);
    // The last counts, and every clock move a page has not answered taken as missed.
    attacher.settle(clock.shim_origin(), &mut next_index, Instant::now() + END_WAIT);
    // What the session covered: the audit's rows and every context it shimmed - past the ceiling of own
    // rows they are counted together in the last row, shimmed all the same (R5-P20) - the pages that took
    // a clock move late or not at all (`chromium.clock_move_missed`, rule 6), and the rest of its facts.
    let outcome = attacher.into_outcome();
    let audited = !outcome.counts.is_empty();
    // The sleep the log still owes, and the whole of it for the verdict - as the native session says it.
    let slept_ms = watch.finish();

    let covered = covered_channels(outcome.counts);

    let verdict = cdp_verdict(outcome.contexts, !covered.is_empty(), outcome.failed);
    let (token, reason) = verdict_keys(&verdict);

    let facts = SessionFacts {
        app_closed,
        audited,
        rate_changed_in_flight,
        clock_moves_missed: outcome.moves_missed > 0,
        contexts_pooled: outcome.pooled > 0,
        context_reached_running: outcome.reached_running > 0,
        timers_capped: outcome.timers_capped > 0,
        frames_unreached: outcome.frames_unreached > 0,
        engine_stamps_real: outcome.engine_stamps > 0,
        animations_real: outcome.animated > 0,
        contexts_unread: outcome.unread > 0,
        clock_clamped: clock.reached_range_end(watch.now()),
        host_slept: slept_ms >= SLEPT_REPORTED_MS,
        followed_browser: followed.is_some(),
        zone_missed: outcome.zone_missed > 0,
        // Not said when the browser was started with the slowdown switched off - its timers kept the
        // session speed while hidden (measured, R4-N28).
        background_timers_slowed: outcome.hidden_fast > 0 && !target.keep_background_timers,
        main_process_uncovered: cdp::is_electron_target(&target.path),
    };
    for event in coverage_events(&outcome.seen, &covered, session_warnings(&facts)) {
        emit(&event);
    }

    let verdict_facts = VerdictFacts { followed: followed.unwrap_or_default(), browser, host_slept_ms: reported_sleep_ms(slept_ms) };
    emit_cdp_session_verdict(token, reason, u32::try_from(outcome.contexts).unwrap_or(u32::MAX), verdict_facts);

    // Session timing from the live clock (correct across any in-flight rate changes and jumps): real is
    // the whole session, fake is the elapsed duration, and the end wall is where the fake clock landed.
    // Then tear down our instance (kill + remove the temp profile), reporting any cleanup residue
    // honestly via `ended.residue_keys` (rules 4, 6).
    let end_now = watch.now();
    let real_ms = clock.elapsed_real_ms(end_now);
    let fake_ms = clock.elapsed_fake_ms(end_now);
    let fake_end_wall = epoch_ms_to_wall(clock.fake_wall_ms(end_now), bias);
    let residue = launched.shutdown_with_residue();
    emit(&Event::Ended {
        v: PROTOCOL_VERSION,
        clean: residue.is_empty(),
        residue_keys: residue,
        target_exit_code: None,
        elapsed_real_ms: real_ms,
        elapsed_fake_ms: fake_ms,
        fake_end_wall: Some(fake_end_wall),
    });
    verdict.exit_code()
}




/// Say why the debugging connection closed when the browser is still running - not the application
/// closing, which is the ordinary end and says nothing, but a connection that dropped under a live
/// browser (R5-D6). The session ended there all the same, and the reader of the diagnostic stream
/// learns why instead of guessing.
fn say_why_closed(closed_under_a_live_browser: bool, attacher: &Attacher) {
    if closed_under_a_live_browser && let Some(why) = attacher.closed_because() {
        diag!("chrono core: the debugging connection closed while the browser still runs: {why}");
    }
}

/// Emit `state` when the heartbeat is due - between commands as well as at the end of a turn. `now`
/// is on the session clock's axis ([`WallWatch::now`]).
fn beat(clock: &CdpClock, deadline: &mut Instant, every: Duration, now: i64) {
    if Instant::now() >= *deadline {
        emit(&clock.state_event_at(now));
        *deadline = Instant::now() + every;
    }
}

/// The pages' clock kept to the session's across a sleep and a correction of the system clock (R5-F6,
/// R5-D42). The session clock runs on [`HostAxis`], interrupt time, and the pages on their own
/// `Date.now`, the system clock: a sleep or a correction moves the second and not the first, and the
/// pages would read the session clock moved by it times the speed. When they stand more than
/// [`DRIFT_MS`] from it, they are given the clock again - the wall alone moves, as in a jump - the way
/// the pages of an engine inside an application are kept to the host (docs/09 section 12.5). A page
/// that runs a script between the wake-up and that move reads the moved clock for that moment. The
/// sleep itself is announced as the native session announces it.
struct WallWatch {
    axis: HostAxis,
    /// Where the system clock stood against the axis when the pages were last given the clock.
    pushed_offset: i64,
    /// The sleep, in ms, the log has been told about so far.
    slept_announced_ms: i64,
}

impl WallWatch {
    fn start() -> WallWatch {
        WallWatch { axis: HostAxis::start(), pushed_offset: 0, slept_announced_ms: 0 }
    }

    /// Now, on the session clock's axis.
    fn now(&self) -> i64 {
        self.axis.now()
    }

    /// One turn: put the clock on where the system clock stands, give the pages the clock again when
    /// they stand too far from it, and announce the sleep the log owes.
    fn turn(&mut self, clock: &mut CdpClock, attacher: &mut Attacher, next_index: &mut u32) {
        let now = self.axis.now();
        clock.set_wall_offset(now_epoch_ms().saturating_sub(now));
        if resync_due(clock.pages_drift_ms(now, self.pushed_offset), clock.mult_at(now)) {
            attacher.move_clock(&cdp_set_expr(clock.shim_origin()), clock.shim_origin(), next_index);
            self.pushed(clock);
        }
        self.announce_sleep();
    }

    /// The pages were just given the clock - a jump, a rate change, a resync.
    fn pushed(&mut self, clock: &CdpClock) {
        self.pushed_offset = clock.wall_offset();
    }

    fn announce_sleep(&mut self) {
        let total = self.axis.slept_ms();
        if let Some(slept) = sleep_to_announce(self.slept_announced_ms, total) {
            self.slept_announced_ms = total;
            emit(&Event::HostSlept { v: PROTOCOL_VERSION, slept_ms: slept as u64, total_slept_ms: total as u64 });
        }
    }

    /// The end: the sleep the log still owes, and the whole sleep of the session, in ms.
    fn finish(&mut self) -> i64 {
        self.announce_sleep();
        self.axis.slept_ms()
    }
}

/// The Chromium session's clock, as the commands see it (R5-A3): the panel's clock, with every context
/// given the move. What its loop notes is a rate changed under a live context, for the report's line
/// about timers already running at the old rate - a jump moves the wall alone and leaves no such timer.
struct CdpCommandClock<'a> {
    clock: &'a mut CdpClock,
    attacher: &'a mut Attacher,
    next_index: &'a mut u32,
    watch: &'a mut WallWatch,
}

impl SessionClock for CdpCommandClock<'_> {
    fn state_event(&self) -> Event {
        self.clock.state_event_at(self.watch.now())
    }

    /// The new rate is scheduled for one instant shortly ahead, on the panel and in every context
    /// alike, so a page takes it over from its own segment at the panel's moment and its wall never
    /// steps at the change (R4-S17). From that instant Date.now, new Date, performance.now and new
    /// timers run at the new rate - only an already-queued setInterval keeps its old cadence, which the
    /// end report warns about (rule 4).
    fn set_multiplier(&mut self, multiplier: i64) -> bool {
        // A change with no context to feel it leaves no timer at the old rate, so it is no caveat for
        // the report (R5-P19) - one attached later takes the new rate from its start.
        let felt = !self.attacher.contexts().is_empty();
        let next = self.clock.set_multiplier_at(multiplier, self.watch.now());
        // The instant on the system clock the pages read (R5-F6).
        self.attacher.move_clock(&cdp_schedule_expr(self.clock.for_pages(next)), self.clock.shim_origin(), self.next_index);
        self.watch.pushed(self.clock);
        felt
    }

    fn jump(&mut self, to: &JumpTo<'_>) -> Result<bool, &'static str> {
        let now = self.watch.now();
        let new_fake = cdp_jump_target(self.clock, to, now)?;
        self.clock.jump_to_at(new_fake, now);
        self.attacher.move_clock(&cdp_set_expr(self.clock.shim_origin()), self.clock.shim_origin(), self.next_index);
        self.watch.pushed(self.clock);
        Ok(false)
    }
}

/// The family verdict of a CDP session. No PID registry on this path: a CDP session tracks JS
/// contexts, not injected processes, so its per-context warnings already travel on the coverage
/// events, and it spawns nothing the hook could fail to follow - the two child fields stay empty.
/// `process_count` has carried the context count since this session existed and keeps doing so for
/// the clients that read it there. `context_count` says the same number under its own name.
/// `followed` names the browser a launcher handed the application to, when the session went on with
/// it after the launcher ended (R4-S18) - the same field the native session fills for the processes it
/// went on for (ADR-16), so the report and the window show it without knowing which path ran.
fn emit_cdp_session_verdict(token: &str, reason: &str, contexts: u32, facts: VerdictFacts) {
    emit(&Event::SessionVerdict {
        v: PROTOCOL_VERSION,
        verdict: token.to_string(),
        reason_key: reason.to_string(),
        process_count: contexts,
        warning_keys: Vec::new(),
        uncovered_children: Vec::new(),
        uncovered_children_total: 0,
        context_count: contexts,
        engines: Vec::new(),
        followed: facts.followed,
        // A hand-off to a copy is a native session's finding: this session owns its browser.
        copies_outside: Vec::new(),
        host_slept_ms: facts.host_slept_ms,
        browser: facts.browser,
    });
}

/// What the verdict of a Chromium session carries beside its judgement: the browser a launcher handed
/// the application to (R4-S18), what the browser calls itself (R5-D44), and how long the machine
/// slept while the session clock stood still (R5-F6, 0 below the threshold).
struct VerdictFacts {
    followed: Vec<FollowedProcess>,
    browser: Option<String>,
    host_slept_ms: u64,
}

/// How long the session waits for the browser to name itself before it goes on without the name.
const BROWSER_VERSION_WAIT: Duration = Duration::from_secs(1);

/// How long the debugging connection has to outlive the launched process before the session counts
/// the launch as handed over (R4-S18). A browser launched directly closes its socket while its process
/// is torn down, so its connection drops within milliseconds of its exit - without a margin, a loop
/// that happens to see the exit before the closed connection would read an ordinary close as a
/// handover.
const HANDOFF_MARGIN: Duration = Duration::from_secs(1);

/// What the session learns about the launched process ending while the browser goes on (R4-S18).
///
/// Measured before the change: a `.bat` that starts the browser and exits ended the session in about
/// 0.4 s with no verdict, and the browser with it, and so did a Chromium browser started straight
/// from its runtime folder, which hands over to a child of its own after about 0.1 s - the port then
/// belongs to that child.
#[derive(Debug, Default)]
struct Handoff {
    /// When the launched process was first SEEN ended, not when it ended: a launcher that exits
    /// before the browser opens its port is first seen on the loop's first turn.
    seen_ended: Option<Instant>,
    /// The process that held the debugging port once the launched one had ended.
    holder: Option<FollowedProcess>,
    /// When the socket table was last read for the holder, so it is read at most once a second.
    asked: Option<Instant>,
}

impl Handoff {
    /// One look at the launched process, every turn of the loop. Nothing happens while it runs. Once
    /// it has ended, `find_holder` reads the socket table, at most once per [`HANDOFF_MARGIN`], until
    /// it names a holder - a few milliseconds each time, and none at all once one is named.
    fn observe(&mut self, launched_alive: bool, now: Instant, find_holder: impl FnOnce() -> Option<FollowedProcess>) {
        if launched_alive || self.holder.is_some() {
            return;
        }
        self.seen_ended.get_or_insert(now);
        if self.asked.is_some_and(|at| now.duration_since(at) < HANDOFF_MARGIN) {
            return;
        }
        self.asked = Some(now);
        self.holder = find_holder();
    }

    /// `None` unless the session went on: the launched process ended and the connection outlived it by
    /// [`HANDOFF_MARGIN`] when the loop ended at `ended`. Otherwise the process the session went on
    /// with, or an empty list when the socket table never named one - the session went on all the
    /// same, and the key says that much.
    fn followed(&self, ended: Instant) -> Option<Vec<FollowedProcess>> {
        let seen = self.seen_ended?;
        (ended.saturating_duration_since(seen) >= HANDOFF_MARGIN).then(|| self.holder.iter().cloned().collect())
    }
}

/// The process holding the session's debugging port on a loopback address, other than the launched
/// one: the browser a launcher handed the application to. `None` when the table cannot be read or
/// nothing else holds the port. Its name goes through the same sieve as every other name a process
/// gives the report.
fn port_holder(port: u16, launched: u32) -> Option<FollowedProcess> {
    let table = chrono_mech::listening_sockets().ok()?;
    let pid = holder_pid(&table.sockets, port, launched)?;
    let image = chrono_mech::process_image_name(pid);
    Some(FollowedProcess { pid, image: image.as_deref().map(cdp::sanitise_target_text) })
}

/// The choice behind [`port_holder`], pure over the table so it is tested without a socket.
fn holder_pid(table: &[chrono_mech::Listener], port: u16, launched: u32) -> Option<u32> {
    table.iter().find(|l| l.port == port && l.loopback && l.pid != launched && l.pid != 0).map(|l| l.pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listener(pid: u32, port: u16, loopback: bool) -> chrono_mech::Listener {
        chrono_mech::Listener { pid, port, loopback, v6: false, addr_v4: 0 }
    }

    fn browser() -> FollowedProcess {
        FollowedProcess { pid: 7, image: Some("browser.exe".into()) }
    }

    /// R5-P19: a rate change is a caveat about running timers only when a context was there to run
    /// them - one with no context left the report warning about timers nobody had.
    #[test]
    fn a_rate_change_is_a_caveat_only_with_a_context_to_feel_it() {
        let (port, server) = crate::cdp::fake_browser_holding(move |request| {
            let id = request["id"].clone();
            match request["method"].as_str().unwrap_or("") {
                "Target.getTargets" => vec![(id, serde_json::json!({ "targetInfos": [{ "targetId": "T", "type": "worker" }] }))],
                "Target.attachToTarget" => vec![(id, serde_json::json!({ "sessionId": "S" }))],
                "Runtime.evaluate" => vec![(id, serde_json::json!({ "result": { "value": "ok" } }))],
                _ => vec![(id, serde_json::json!({}))],
            }
        });
        let ws = crate::cdp::WsClient::connect("127.0.0.1", port, "/", Instant::now() + Duration::from_secs(5)).unwrap();
        let mut attacher = Attacher::over(cdp::CdpClient::from_ws(ws), port);
        attacher.set_budgets(Duration::from_millis(20), Duration::from_secs(2));
        let mut watch = WallWatch::start();
        let mut clock = CdpClock::from_time_spec(
            &TimeSpec {
                moment: chrono_proto::MomentSpec { kind: chrono_proto::MomentKind::Absolute, local: None, tz_bias_min: Some(0), delta: None },
                mode: chrono_proto::TimeModeKind::Multiplier,
                multiplier: Some(60),
                scale_duration: false,
                scale_qpc: false,
            },
            watch.now(),
        )
        .unwrap();
        let mut next = 0;
        let speed = |id: u64| Command::SetMultiplier { v: PROTOCOL_VERSION, id, multiplier: 120 };
        let mut apply = |command, clock: &mut CdpClock, attacher: &mut Attacher, next: &mut u32| {
            apply_command(&mut CdpCommandClock { clock, attacher, next_index: next, watch: &mut watch }, command)
        };
        assert!(!apply(speed(1), &mut clock, &mut attacher, &mut next), "no context, no caveat");
        attacher.attach_existing(clock.shim_origin(), &mut next, Instant::now() + Duration::from_secs(2)).unwrap();
        assert!(apply(speed(2), &mut clock, &mut attacher, &mut next), "a worker ran timers at the old rate");
        drop(attacher);
        drop(server);
    }

    /// A browser launched directly that closes: its connection drops within milliseconds of the
    /// exit, so the session ended with it and did not go on for anything (R4-S18).
    #[test]
    fn a_browser_that_closes_did_not_hand_anything_over() {
        let t0 = Instant::now();
        let mut h = Handoff::default();
        h.observe(true, t0, || panic!("the table is not read while the launched process runs"));
        h.observe(false, t0 + Duration::from_millis(10), || None);
        assert_eq!(h.followed(t0 + Duration::from_millis(40)), None);
    }

    /// A launcher that ends while the browser it started keeps the connection open: the session went
    /// on with that browser, and names it.
    #[test]
    fn a_launcher_that_ends_hands_the_session_to_the_port_holder() {
        let t0 = Instant::now();
        let mut h = Handoff::default();
        h.observe(false, t0, || Some(browser()));
        h.observe(false, t0 + Duration::from_millis(200), || panic!("a named holder is not asked for again"));
        assert_eq!(h.followed(t0 + Duration::from_secs(3)), Some(vec![browser()]));
        assert_eq!(h.followed(t0 + Duration::from_millis(999)), None, "the margin is a full second");
    }

    /// A table that cannot name the holder is read again, but not on every turn of the loop - and a
    /// session that went on still says so, with nobody named.
    #[test]
    fn the_table_is_read_at_most_once_a_second_until_it_names_the_holder() {
        let t0 = Instant::now();
        let mut h = Handoff::default();
        let mut reads = 0;
        for ms in [0, 50, 500, 999, 1_000, 1_400, 2_100] {
            h.observe(false, t0 + Duration::from_millis(ms), || {
                reads += 1;
                None
            });
        }
        assert_eq!(reads, 3, "read at 0, 1000 and 2100 ms");
        assert_eq!(h.followed(t0 + Duration::from_secs(5)), Some(Vec::new()));
    }

    /// A launched process that never ended hands nothing over, however long the session ran.
    #[test]
    fn a_launched_process_that_runs_to_the_end_hands_nothing_over() {
        let t0 = Instant::now();
        let mut h = Handoff::default();
        h.observe(true, t0, || panic!("not read"));
        assert_eq!(h.followed(t0 + Duration::from_secs(60)), None);
    }

    /// The holder is whoever else listens on the session's port on a loopback address - never the
    /// launched process, never a socket on another port or another address, never pid 0.
    #[test]
    fn the_holder_is_another_process_on_the_sessions_loopback_port() {
        let table = [
            listener(10, 9222, true),
            listener(0, 9222, true),
            listener(11, 9222, false),
            listener(12, 9333, true),
            listener(13, 9222, true),
        ];
        assert_eq!(holder_pid(&table, 9222, 10), Some(13));
        assert_eq!(holder_pid(&table, 9222, 13), Some(10), "the launched pid is the only one skipped");
        assert_eq!(holder_pid(&table[..4], 9222, 10), None);
        assert_eq!(holder_pid(&[], 9222, 10), None);
    }
}


