//! The attacher: one DevTools endpoint and every JS context reached through it, shimmed.
//!
//! This is the middle of `cdp_session` lifted out and given a name. That loop kept six variables
//! for one job - which contexts it still talks to, which it ever covered, their call counts, the
//! index each target keeps across re-attaches, how many shims failed, and the next index - and the
//! embedded-engine channel (docs/09) needs the same job done on a port it discovered rather than
//! one it opened. Two copies of that state would drift the first time one of them learned
//! something, so there is one, here, and both the Chromium session and the embedded probe drive it.
//!
//! The attacher owns the connection and the per-context bookkeeping. It does NOT own the clock:
//! every attach asks the caller for the clock's current origin, so a context that arrives after an
//! in-flight rate change starts on the same clock as the rest (rule 3). And it does not own the
//! context index counter: several attachers in one session (slice C) must hand out disjoint
//! indexes, because the index is the unit's identity on the wire.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::time::{Duration, Instant};

use serde_json::json;

use crate::cdp;
use crate::cdp_audit::context_index_for;
use crate::cdp_requests::{CdpContext, Requests, ZoneTally};
use crate::output::diag;
use crate::zone::{now_epoch_ms, WALL_MAX_MS};

/// How long a clock move waits for the pages' new hooks before the command is acknowledged (R4-S10,
/// ADR-20). A page that has not answered by then still gets its move once its hook comes back - this
/// bounds the wait, not the move.
pub(crate) const MOVE_WAIT_MS: u64 = 2_000;

/// How long the end of a session waits for the release and the last counts together. A GUI gives the
/// core two seconds after `end` (`CoreClient`), and the session has a verdict to write after this.
pub(crate) const END_WAIT: Duration = Duration::from_millis(750);

/// How long one pump turn goes on taking messages that are already here, so a burst of attaches
/// cannot hold the loop from its heartbeat (R4-S10). The rest is taken on the next turn.
const TURN_BUDGET: Duration = Duration::from_millis(100);

/// The part of the end's deadline the release keeps for the zone it gives back and the read after it
/// (ADR-21), when the session set a zone: two round trips over loopback, which take milliseconds in a
/// page that is not busy - a page busy for this long has not answered the release either.
const ZONE_STAGE: Duration = Duration::from_millis(250);

/// The part of [`ZONE_STAGE`] kept for the zone read itself, sent at the latest this far before the
/// deadline whether every zone was answered or not.
const READ_STAGE: Duration = Duration::from_millis(100);

/// This machine's zone as the release gives it back and checks it: its name (`None` when the system
/// names none, and the override is then taken away instead), the two instants of the year it is read
/// at, and this machine's offsets at them (`None` when the system would not convert them).
pub(crate) struct MachineZone<'a> {
    pub(crate) name: Option<&'a str>,
    pub(crate) at_ms: [i64; 2],
    pub(crate) offsets: Option<[i32; 2]>,
}

/// What the release of an attacher came to: how many contexts did not confirm they were let go, and
/// where the contexts stand with their zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Released {
    pub(crate) unconfirmed: u32,
    pub(crate) zone: ZoneTally,
}

/// The clock origin a shim is built from: fake start and real start (both Unix-epoch ms), the wall
/// rate and the duration rate. A Chromium session runs both rates at the multiplier. A page inside a
/// natively hooked application follows that application's duration axis, which scales only under
/// `scale_duration` (docs/09 section 12.6).
///
/// `scheduled` is a rate change of a Chromium session still waiting for its instant (R4-S17). The
/// pages of an embedded engine follow the host's clock as it is, so theirs is always `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ShimOrigin {
    pub(crate) fake0: i64,
    pub(crate) real0: i64,
    pub(crate) mult: i64,
    pub(crate) dur: i64,
    pub(crate) scheduled: Option<cdp::ScheduledRate>,
}

impl ShimOrigin {
    /// The shim source that puts a context on this clock, checking that it reads the zone `zone`
    /// (the session's bias, `None` for no check).
    fn shim(&self, zone: Option<i32>) -> String {
        cdp::build_shim(self.fake0, self.real0, self.mult, self.dur, self.scheduled, WALL_MAX_MS, zone)
    }
}

/// What a context answered when asked which clock it already reads (docs/09 section 12.17 point 3).
/// Asked before the shim goes in, by an attacher inside a natively hooked session: a context that
/// already reads the session clock through the native mechanism must not get the shim on top, or it
/// would scale the fake wall a second time and the audit would still call it covered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClockRead {
    /// Our own shim marker is there - a page that reloaded and re-ran the add-script hook.
    Shim,
    /// The reading stands nearer the session clock than the real one: covered natively already.
    Native,
    /// The reading is real, or there was no reading to judge by - the shim is what covers it.
    Real,
}

/// The probe: our marker if it is there, otherwise the context's own `Date.now()` as text.
const CLOCK_PROBE_EXPR: &str = "globalThis.__chronomock ? 'shim' : String(Date.now())";

/// Judge a probe reply against where the session clock and the real clock stand now. No reply, or one
/// that is not a number, is `Real`: the absence of evidence that a context is covered is not evidence
/// that it is, and the shim is the safe side. A reading equally far from both is `Real` as well - that
/// only happens with the session clock within milliseconds of the real one, where a second
/// substitution is the identity. The distances are unsigned, because the reading is whatever the
/// page's own `Date.now` returned - a subtraction near `i64::MIN` panicked the core (R4-N23).
pub(crate) fn classify_clock_read(reply: Option<&str>, fake_now_ms: i64, real_now_ms: i64) -> ClockRead {
    match reply {
        Some("shim") => ClockRead::Shim,
        Some(text) => match text.parse::<i64>() {
            Ok(read) if read.abs_diff(fake_now_ms) < read.abs_diff(real_now_ms) => ClockRead::Native,
            _ => ClockRead::Real,
        },
        None => ClockRead::Real,
    }
}

/// How many contexts of one attacher get an audit row of their own. A family that fans out without
/// bound must not grow the audit without bound, but the ceiling is the audit's, not the coverage's
/// (R5-P20, R5-D8): a context past it is shimmed like any other and counted in one row together with
/// the rest past it - the pool, which the caller names (`chromium.contexts_pooled`). It used to be
/// refused the shim and ran on the real clock, and an application that starts a worker per task
/// passed the ceiling in minutes (measured on Electron 44: every `new Worker()` is a target of its own).
pub(crate) const MAX_CONTEXTS: usize = 256;

/// How many target ids of contexts that went away are remembered, so one that comes back keeps its
/// index (R3-7) while the memory stays bounded however many workers come and go (R5-Z2). Past this
/// the oldest is forgotten, and a context that comes back after it gets a new index.
const DETACHED_KEPT: usize = 4096;

/// How many contexts the session could not cover are named on the diagnostic stream, one line each,
/// before the rest are only counted - a page that fails over and over must not flood it.
const SAID_MAX: u32 = 3;

/// What one turn of the pump found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Pumped {
    /// Nothing within the poll interval, or an event that changed no context.
    Idle,
    /// A context attached and was shimmed (or refused past the ceiling, or failed - see the counters).
    Attached,
    /// A context went away and was dropped from the live list.
    Detached,
    /// The connection is gone: the application closed, or the engine did.
    Closed,
}

impl Pumped {
    /// The outcome a turn of several messages reports: a closed connection over anything, an attach
    /// over a detach, anything over nothing.
    fn stronger(self, other: Pumped) -> Pumped {
        let rank = |p: &Pumped| match p {
            Pumped::Idle => 0,
            Pumped::Detached => 1,
            Pumped::Attached => 2,
            Pumped::Closed => 3,
        };
        if rank(&other) > rank(&self) { other } else { self }
    }
}

/// How many messages one turn of the pump handles after the first - all of them already here, so
/// none is waited for (R4-S14). One message a turn handled about ten a second on an engine inside a
/// natively hooked session, so a page load's seventy-odd events kept a worker auto-attach paused on
/// start waiting about seven seconds for its shim and its release. Bounded so a target that never
/// stops talking cannot keep the turn from ending.
const PUMP_DRAIN_MAX: usize = 256;

/// The rest of a pump turn after its first message: `next_ready` handles the next message that is
/// already here and says what it found, or `None` when nothing is. Stops at a closed connection, at
/// the end of what is here, at [`PUMP_DRAIN_MAX`], or once `in_budget` says the turn has run long
/// enough, and reports the strongest outcome of the turn.
fn drain_turn(first: Pumped, mut in_budget: impl FnMut() -> bool, mut next_ready: impl FnMut() -> Option<Pumped>) -> Pumped {
    let mut turn = first;
    for _ in 0..PUMP_DRAIN_MAX {
        if turn == Pumped::Closed || !in_budget() {
            break;
        }
        match next_ready() {
            Some(found) => turn = turn.stronger(found),
            None => break,
        }
    }
    turn
}

/// What one attacher covered, handed over when it is done - because it was closed by the engine, or
/// because the session ended. The counts and the seen list are the audit's evidence, and an attacher
/// dropped without this hand-over takes them with it (docs/09 section 12.17).
pub(crate) struct AttacherOutcome {
    /// The audit's rows: every context with a row of its own, then the pool's row when there is one.
    pub(crate) seen: Vec<u32>,
    pub(crate) counts: BTreeMap<(u32, String), u64>,
    pub(crate) failed: usize,
    /// How many contexts were counted in the pool's row (R5-P20).
    pub(crate) pooled: usize,
    /// How many contexts were shimmed: the own rows and the pooled ones.
    pub(crate) contexts: usize,
    /// How many of them were reached while already running (R5-P7).
    pub(crate) reached_running: usize,
    /// How many pages missed a clock move (`chromium.clock_move_missed`).
    pub(crate) moves_missed: usize,
    /// How many contexts read a zone other than the session's at some point.
    pub(crate) zone_missed: usize,
    /// How many pages were hidden at some point while their timers ran faster (R4-N28).
    pub(crate) hidden_fast: usize,
    /// How many contexts had a timer the engine's 4 ms floor held back at the session's speed (R5-P6).
    pub(crate) timers_capped: usize,
    /// How many pages had a frame in their process the shim did not reach (R5-P5).
    pub(crate) frames_unreached: usize,
    /// How many contexts read a time the engine stamps while their timers ran faster (R5-F9).
    pub(crate) engine_stamps: usize,
    /// How many pages played an animation while their timers ran faster (R5-F9).
    pub(crate) animated: usize,
    /// How many contexts went away before their shim answered an audit (R5-F10).
    pub(crate) unread: usize,
}

pub(crate) struct Attacher {
    client: cdp::CdpClient,
    port: u16,
    /// Whether to ask a context which clock it reads before shimming it. Off for the Chromium
    /// session, whose contexts nothing else covers. On for an attacher inside a native session.
    probe_clock: bool,
    /// Contexts found already on the session clock and left alone, for the diagnostic line.
    native: usize,
    /// Who we still TALK to, and every request in flight to them (ADR-20).
    requests: Requests,
    /// Who this attacher ever COVERED with a row of its own, in attach order, append-only: the audit
    /// is a record of what happened, not of what is still open, so a context that reloaded or closed
    /// keeps its evidence (R2-W1). Once per CONTEXT rather than once per attach, at most
    /// [`MAX_CONTEXTS`].
    seen: Vec<u32>,
    failed: usize,
    /// The pages whose shim is their new-document hook's to give, by index, with whether the index was
    /// known before (R5-F11): covered once their audit shows the shim, counted as not covered while
    /// they wait - at the end too (R5-D46).
    awaiting: HashMap<u32, bool>,
    /// The row the contexts past [`MAX_CONTEXTS`] are counted in: the index of the first of them.
    pool: Option<u32>,
    /// How many contexts were counted in the pool's row, each once.
    pooled: usize,
    /// How many contexts were reached while already running, each once (R5-P7).
    reached_running: usize,
    /// How many contexts the session could not cover were said on the diagnostic stream (R5-D6).
    said: u32,
    /// What closed the connection, when it closed (R5-D6).
    closed_because: Option<String>,
    /// targetId -> context index, so a re-attached context keeps the identity it already had. Live
    /// contexts always, the ones with an audit row of their own always, the others that went away while
    /// they are among the last [`DETACHED_KEPT`].
    index_by_target: HashMap<String, u32>,
    /// The target ids of the contexts that went away, the last [`DETACHED_KEPT`] of them (R5-Z2).
    detached: DetachedRing,
    /// How many of them are kept - [`DETACHED_KEPT`], lowered only by a test that needs the ring full.
    detached_kept: usize,
    /// The session is ending: a context that attaches now is let go without a shim. It would get a
    /// clock nobody moves or releases any more, and shimming it would hold the end for an attach.
    ending: bool,
    /// The session's zone bias (minutes west of UTC), which every context is put on through the
    /// engine's zone override. `None` sets no zone and checks none.
    zone: Option<i32>,
    /// A context went away since the zone was last given to the rest. The override is held by one
    /// context per renderer process, and the others in that process lose it with the holder, so the
    /// rest are given it again - once per turn, however many went away in it.
    zone_dirty: bool,
}

impl Attacher {
    /// Connect to the browser endpoint on a loopback host and port and arm auto-attach, so every
    /// page and worker the browser creates from now on arrives as an `attachedToTarget` event,
    /// paused until the shim is in, all of it by `deadline`. The pages that ALREADY exist are the
    /// caller's next call. The host is `127.0.0.1` or `::1` - whichever family the listener was
    /// found on.
    pub(crate) fn connect(host: &str, port: u16, deadline: Instant) -> io::Result<Attacher> {
        Attacher::connect_by_path(host, port, None, deadline)
    }

    /// [`Attacher::connect`] by the browser endpoint's path when the caller has it - the one a browser
    /// this session launched wrote beside its port (R5-WY9) - and over `/json/version` when not.
    pub(crate) fn connect_by_path(host: &str, port: u16, path: Option<&str>, deadline: Instant) -> io::Result<Attacher> {
        let mut client = cdp::CdpClient::connect_to_browser(host, port, path, deadline)?;
        client.call_until(
            "Target.setAutoAttach",
            json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true }),
            None,
            deadline,
        )?;
        Ok(Attacher::over(client, port))
    }

    /// An attacher over a client already connected and armed - what [`Attacher::connect`] builds once
    /// the endpoint answered, and what a test builds over a socket of its own, here and in the bridge.
    pub(crate) fn over(client: cdp::CdpClient, port: u16) -> Attacher {
        Attacher {
            client,
            port,
            probe_clock: false,
            native: 0,
            requests: Requests::default(),
            seen: Vec::new(),
            failed: 0,
            awaiting: HashMap::new(),
            pool: None,
            pooled: 0,
            reached_running: 0,
            said: 0,
            closed_because: None,
            index_by_target: HashMap::new(),
            detached: DetachedRing::default(),
            detached_kept: DETACHED_KEPT,
            ending: false,
            zone: None,
            zone_dirty: false,
        }
    }

    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    /// What the browser calls itself (`Browser.getVersion`'s `product`), cleaned for the report, or
    /// `None` when it did not say by `deadline` (R5-D6). Asked once the pages that already exist are
    /// attached, so it costs the first page nothing - a context that attaches meanwhile waits in the
    /// client's queue for the next turn.
    pub(crate) fn browser_version(&mut self, deadline: Instant) -> Option<String> {
        let reply = self.client.call_until("Browser.getVersion", json!({}), None, deadline).ok()?;
        reply["product"].as_str().map(cdp::sanitise_target_text).filter(|p| !p.is_empty())
    }

    /// Put every context this attacher shims from now on in the session's zone, `bias_min` minutes
    /// west of UTC as Windows counts it, and check that each one reads it. Called before the first
    /// attach, so no context is shimmed without it.
    pub(crate) fn set_zone(&mut self, bias_min: i32) {
        self.zone = Some(bias_min);
    }

    /// Shorten the client's poll interval and call deadline - see `CdpClient::set_budgets`. For an
    /// attacher driven from a loop that has its own cadence to keep.
    pub(crate) fn set_budgets(&mut self, poll: Duration, call: Duration) {
        self.client.set_budgets(poll, call);
    }

    /// Ask each context which clock it reads before shimming it (see [`ClockRead`]).
    pub(crate) fn probe_clock_before_shim(&mut self) {
        self.probe_clock = true;
    }

    /// How many contexts were found already on the session clock and left unshimmed.
    pub(crate) fn native(&self) -> usize {
        self.native
    }

    /// Hand over what this attacher covered. The connection closes with it.
    pub(crate) fn into_outcome(self) -> AttacherOutcome {
        let moves_missed = self.requests.moves_missed();
        let zone_missed = self.requests.zone_missed();
        let hidden_fast = self.requests.hidden_fast();
        let timers_capped = self.requests.timers_capped();
        let frames_unreached = self.requests.frames_unreached();
        let engine_stamps = self.requests.engine_stamps();
        let animated = self.requests.animated();
        let unread = self.requests.unread();
        let seen = self.seen();
        let contexts = self.contexts_covered();
        let failed = self.failed();
        AttacherOutcome {
            seen,
            counts: self.requests.into_counts(self.pool),
            failed,
            pooled: self.pooled,
            contexts,
            reached_running: self.reached_running,
            moves_missed,
            zone_missed,
            hidden_fast,
            timers_capped,
            frames_unreached,
            engine_stamps,
            animated,
            unread,
        }
    }

    /// Attach to every shimmable target the browser already has - the pages an embedded engine
    /// opened before the session found its port. Auto-attach delivers them too (measured in slice
    /// B), so this is the belt, asked for by name and idempotent: a target the index map already
    /// knows is skipped, whichever road it came by. Stops starting new attaches at `deadline` and
    /// leaves the rest to auto-attach. Returns how many were newly shimmed.
    pub(crate) fn attach_existing(&mut self, origin: ShimOrigin, next_index: &mut u32, deadline: Instant) -> io::Result<usize> {
        let reply = self.client.call_until("Target.getTargets", json!({}), None, deadline)?;
        let mut attached = 0;
        for (tid, ty) in new_targets(&reply, &self.index_by_target) {
            if Instant::now() >= deadline {
                break;
            }
            let params = json!({ "targetId": tid, "flatten": true });
            // A target the session could not attach to ran unreached, like one whose shim failed, and
            // is counted the same way (R5-D6) - it used to be skipped without a word.
            let reply = match self.client.call_until("Target.attachToTarget", params, None, deadline) {
                Ok(reply) => reply,
                Err(e) => {
                    self.failed += 1;
                    self.say("a page or worker could not be attached to", &e);
                    continue;
                }
            };
            let sid = reply["sessionId"].as_str().unwrap_or("").to_string();
            if sid.is_empty() {
                self.failed += 1;
                self.say("a page or worker could not be attached to", &"the browser named no session");
                continue;
            }
            // Attached by name, so not paused on start: a target that was there before the session.
            self.shim(Arrival { sid, ty, tid, waiting: false, parent: None }, origin, next_index, Some(deadline));
            attached += 1;
        }
        Ok(attached)
    }

    /// One turn: poll the connection (bounded by the client's poll interval), act on what came, then
    /// on everything else that is already here, without waiting for more (R4-S14) and for no longer
    /// than [`TURN_BUDGET`].
    pub(crate) fn pump(&mut self, origin: ShimOrigin, next_index: &mut u32) -> Pumped {
        let polled = self.client.poll();
        let started = Instant::now();
        let first = self.handle(polled, origin, next_index);
        let turn = drain_turn(first, || started.elapsed() < TURN_BUDGET, || match self.client.poll_ready() {
            Ok(None) => None,
            ready => Some(self.handle(ready, origin, next_index)),
        });
        self.give_zone_again();
        turn
    }

    /// Give the session's zone again to every live context once one went away (see `zone_dirty`).
    /// Measured: a second page in a renderer process reads the holder's override, and goes back to the
    /// host's zone when the holder closes - asked again, it takes the override itself. Not while the
    /// session is ending, when the release takes the zone away.
    fn give_zone_again(&mut self) {
        if !self.zone_dirty || self.ending {
            return;
        }
        self.zone_dirty = false;
        if let Some(bias) = self.zone {
            self.requests.send_zone(&cdp::zone_id(bias), &mut self.client);
        }
    }

    /// Act on one polled message. Every answer goes to the request table, whatever it answers and
    /// whenever it comes (ADR-20).
    fn handle(&mut self, polled: io::Result<Option<cdp::Msg>>, origin: ShimOrigin, next_index: &mut u32) -> Pumped {
        match polled {
            Ok(Some(cdp::Msg::Response { id, result, error })) => {
                let reply = if error.is_none() { Ok(result) } else { Err(()) };
                self.requests.on_reply(id, reply, &mut self.client);
                // A page that waited for its shim and showed it in this answer is covered from now on.
                for index in self.requests.take_confirmed() {
                    if let Some(known) = self.awaiting.remove(&index) {
                        self.cover(index, known, false);
                    }
                }
                Pumped::Idle
            }
            Ok(Some(cdp::Msg::Event { method, params, session_id: parent })) if method == "Target.attachedToTarget" => {
                let sid = params["sessionId"].as_str().unwrap_or("").to_string();
                let ty = params["targetInfo"]["type"].as_str().unwrap_or("").to_string();
                let tid = params["targetInfo"]["targetId"].as_str().unwrap_or("").to_string();
                if sid.is_empty() {
                    return Pumped::Idle;
                }
                if self.ending || !cdp::is_shimmable(&ty) {
                    // Auto-attach paused it on start, like everything it delivers. A target with no
                    // timer of ours to cover - or one that arrives as the session ends - is let go at
                    // once: left as it arrived, it would stay paused.
                    self.let_go(&sid, parent.as_deref());
                    return Pumped::Idle;
                }
                let waiting = params["waitingForDebugger"].as_bool().unwrap_or(false);
                self.shim(Arrival { sid, ty, tid, waiting, parent }, origin, next_index, None);
                Pumped::Attached
            }
            // A context that went away - a closed window, a recycled worker. Dropped from the live
            // list with its requests in flight: its counts stay in the table and its index in `seen`.
            Ok(Some(cdp::Msg::Event { method, params, .. }))
                if method == "Target.detachedFromTarget" || method == "Target.targetDestroyed" =>
            {
                let sid = params["sessionId"].as_str().unwrap_or("");
                // A session that went away is that session alone: its target can have another session,
                // the one this attacher drives - a second session on a target is let go (R5-Z3), and its
                // going names the target too. Only a destroyed target, which names no session, takes
                // every context of the target with it.
                let tid = if method == "Target.detachedFromTarget" && !sid.is_empty() { "" } else { params["targetId"].as_str().unwrap_or("") };
                let gone = self.requests.forget(sid, tid);
                if gone.is_empty() {
                    return Pumped::Idle;
                }
                for target in gone {
                    self.remember_detached(target);
                }
                // It may have held its process's zone override for the others (see `zone_dirty`).
                self.zone_dirty = self.zone.is_some();
                Pumped::Detached
            }
            Ok(_) => Pumped::Idle,
            Err(e) => {
                // Kept for the caller, who knows whether the application closed with it (R5-D6).
                self.closed_because.get_or_insert_with(|| e.to_string());
                // The contexts still listed went with the connection, and the ones never read with
                // their calls (R5-F10).
                self.requests.connection_closed();
                Pumped::Closed
            }
        }
    }

    /// One line on the diagnostic stream for a context the session could not cover, at most
    /// [`SAID_MAX`] a session, then one more that says the rest are only counted (R5-D6). The count is
    /// the verdict's and the report's - this is the why, for the one reading the stream.
    fn say(&mut self, what: &str, why: &dyn std::fmt::Display) {
        self.said += 1;
        if self.said <= SAID_MAX {
            diag!("chrono core: {what}: {why}");
        } else if self.said == SAID_MAX + 1 {
            diag!("chrono core: more pages or workers could not be covered - they are counted, not listed");
        }
    }

    /// Why the connection closed, when it did.
    pub(crate) fn closed_because(&self) -> Option<&str> {
        self.closed_because.as_deref()
    }

    /// Handle what comes until `done` says so or `deadline` passes - the one place an attacher waits
    /// for answers. Messages are handled exactly as a turn handles them, so an attach or a detach in
    /// the meantime is not lost, and the deadline bounds the wait, never what is learned (ADR-20).
    fn wait_until(&mut self, deadline: Instant, origin: ShimOrigin, next_index: &mut u32, done: impl Fn(&Requests) -> bool) {
        while !done(&self.requests) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            let polled = self.client.poll_for(left);
            if self.handle(polled, origin, next_index) == Pumped::Closed {
                return;
            }
        }
    }

    /// Give a newly attached context its index and its shim. The index is keyed by the CDP targetId,
    /// which Chromium keeps across re-attaches (R3-7). The shim is built from the clock's CURRENT
    /// origin, never the session's initial one.
    fn shim(&mut self, arrival: Arrival, origin: ShimOrigin, next_index: &mut u32, cap: Option<Instant>) {
        let Arrival { sid, ty, tid, waiting, parent } = arrival;
        // The session this attacher drives already, announced again: a by-name attach is also announced
        // as an attach event (measured on Electron 44 in flatten mode), and it is the same session, not
        // a second one to let go - letting it go would detach the context the session covers.
        if self.requests.contexts().iter().any(|c| c.session_id == sid) {
            return;
        }
        // A target reached twice while it is live - auto-attach and the by-name attach can both
        // deliver the same page - stays one context: the shim itself is idempotent, but a second
        // session on the list would be polled and broadcast to twice. The second session is let go
        // so it does not sit paused.
        if !tid.is_empty() && self.requests.contexts().iter().any(|c| c.target_id == tid) {
            self.let_go(&sid, parent.as_deref());
            return;
        }
        // One deadline for everything this attach asks the context (R4-S10): each call used to have
        // its own, so one context in a busy renderer could hold the session for several of them.
        let deadline = attach_deadline(Instant::now(), self.client.call_budget(), cap);
        let known = !tid.is_empty() && self.index_by_target.contains_key(&tid);
        let index = context_index_for(&tid, &mut self.index_by_target, next_index);
        // A second session of this target announced meanwhile is let go as it comes: the target runs
        // nothing, the shim included, until every session that holds it paused releases it.
        self.client.attaching(Some((&tid, &sid)));
        let put = self.put_shim_in(&sid, &ty, waiting, origin, deadline);
        self.client.attaching(None);
        let Some(injected) = put else {
            // Covered by the native mechanism already: it has a row of its own under its pid, and a
            // shim here would scale the fake wall twice. Released, and not counted as a context.
            self.native += 1;
            self.let_go(&sid, parent.as_deref());
            return;
        };
        match injected {
            Ok(cdp::Injected { script, children, running, awaiting }) => {
                if !children && cdp::starts_workers(&ty) {
                    // The context is on the session clock, but it refused auto-attach, so a worker it
                    // starts runs on the real clock unseen. Counted as one that could not be reached,
                    // so the verdict says some were not rather than that all were (rule 4).
                    self.failed += 1;
                }
                // The target named its own context type, and that name becomes a coverage key in the
                // report and on the wire. Cleaned here, at the one place a context is built.
                self.requests.push(CdpContext::new(index, sid, cdp::sanitise_target_text(&ty), tid, script).awaiting(awaiting));
                if awaiting {
                    // No document yet: the hook gives the first one its shim, and the first audit that
                    // answers from it makes the page covered (R5-F11, R5-D46).
                    self.awaiting.insert(index, known);
                    return;
                }
                // Reached while it was already running (R5-P7): a page whose own scripts the shim found
                // before it, a worker that was not paused on start. Its scripts may have read the real
                // clock first - said, once per context. A page paused on start finds no scripts.
                let late = if cdp::is_worker(&ty) { !waiting } else { running };
                self.cover(index, known, late);
            }
            Err(e) => {
                // The shim did not take, and the injection may have stopped before its own resume
                // call: let the context run unshimmed rather than paused, and count it as uncovered - and say
                // why, the text the target's engine gave being cleaned at its source (R5-D6).
                self.failed += 1;
                self.say("a page or worker could not take the time shim", &e);
                self.resume(&sid);
            }
        }
    }

    /// Put the shim into the context of session `sid`, by `deadline`: what the injection left, or `None`
    /// when the context was asked first which clock it reads and reads the session's already. `waiting`:
    /// auto-attach delivered it paused on start, and a page so paused may answer nothing until it is
    /// released (see [`cdp::inject_page`]).
    fn put_shim_in(&mut self, sid: &str, ty: &str, waiting: bool, origin: ShimOrigin, deadline: Instant) -> Option<io::Result<cdp::Injected>> {
        if self.probe_clock && self.reads_session_clock_natively(sid, origin, deadline) {
            return None;
        }
        let shim = origin.shim(self.zone);
        let zone = self.zone.map(cdp::zone_id);
        Some(if cdp::is_worker(ty) {
            cdp::inject_worker(&mut self.client, sid, &shim, zone.as_deref(), deadline)
        } else {
            cdp::inject_page(&mut self.client, sid, &shim, zone.as_deref(), waiting, deadline)
        })
    }

    /// Count a context the shim is in as covered: a row of its own, or past the ceiling of own rows the
    /// pool's row (R5-P20 - a ceiling on the audit must not take the coverage away, rule 27), and
    /// whether it was reached `late`, already running (R5-P7). `known`: it came back under the index it
    /// had, and is not counted again.
    fn cover(&mut self, index: u32, known: bool, late: bool) {
        if late && !known {
            self.reached_running += 1;
        }
        if past_ceiling(&self.seen, index) {
            if !known {
                self.pooled += 1;
            }
            self.pool.get_or_insert(index);
            self.requests.pool_context(index);
        } else if !self.seen.contains(&index) {
            self.seen.push(index);
        }
    }

    /// Ask a context which clock it reads and judge the answer against the session clock projected
    /// to now. One `Runtime.evaluate`, by the attach's deadline - a context that does not answer
    /// counts as real, and gets the shim.
    fn reads_session_clock_natively(&mut self, sid: &str, origin: ShimOrigin, deadline: Instant) -> bool {
        let real_now = now_epoch_ms();
        let fake_now = origin.fake0.saturating_add((real_now - origin.real0).saturating_mul(origin.mult));
        let params = json!({ "expression": CLOCK_PROBE_EXPR, "returnByValue": true });
        let reply = self.client.call_until("Runtime.evaluate", params, Some(sid), deadline).ok();
        let text = reply.as_ref().and_then(|r| r["result"]["value"].as_str());
        classify_clock_read(text, fake_now, real_now) == ClockRead::Native
    }

    /// Release a target that auto-attach paused on start. Not waited for: its answer changes nothing,
    /// a target that is not paused answers the same, and one that is already gone errors harmlessly.
    fn resume(&mut self, sid: &str) {
        let _ = self.client.send("Runtime.runIfWaitingForDebugger", json!({}), Some(sid));
    }

    /// Release a target this session will not drive and detach from it (R5-Z3): a second session on a
    /// target that has one, a context the native mechanism covers, a target with no JS of ours to cover,
    /// one that arrives as the session ends. Left attached, the session stays open in the browser for
    /// as long as the connection lives. Measured on Electron 44: a worker detached right after its
    /// release runs on - a chain of a hundred workers, each started by the one before, ran through. NOT
    /// for a context whose shim failed: its release is queued in its own session behind the shim
    /// (`take_back`), and a detach could drop it. The detach goes through `parent`, the session that
    /// announced it ([`cdp::CdpClient::let_go`]).
    fn let_go(&mut self, sid: &str, parent: Option<&str>) {
        self.client.let_go(sid, parent);
    }

    /// Remember a target that went away for a while, so it keeps its index if it comes back, and forget
    /// the oldest past [`DETACHED_KEPT`] unless it came back and is live, or holds an audit row of its
    /// own (R5-Z2). Those are never forgotten: there are at most [`MAX_CONTEXTS`] of them, and one that
    /// came back under a new index was counted twice (CodeRabbit on #110).
    fn remember_detached(&mut self, target: String) {
        let contexts = self.requests.contexts();
        let seen = &self.seen;
        let index_by_target = &self.index_by_target;
        let keep = |t: &str| {
            contexts.iter().any(|c| c.target_id == t) || index_by_target.get(t).is_some_and(|i| seen.contains(i))
        };
        let forgotten = remember_detached_in(&mut self.detached, target, self.detached_kept, keep);
        if let Some(old) = forgotten {
            self.index_by_target.remove(&old);
        }
    }

    /// The clock moved - a rate change, a jump, a resync: give every page a new-document hook built on
    /// `origin`, and `expr` to every live document, a page's once its new hook is back - so a page
    /// that loads a new document meanwhile already gets the new clock (R4-W5), a scheduled change
    /// included, which is why a Chromium session schedules its rate changes far enough ahead to cover
    /// both (R4-S17).
    ///
    /// Waits for the new hooks at most [`MOVE_WAIT_MS`] and then returns, all pages or not (R4-S10).
    /// A page that answers later still gets its move. Whether a page missed the move is read from its
    /// answers whenever they come, and handed over at the end ([`Attacher::moves_missed`]).
    pub(crate) fn move_clock(&mut self, expr: &str, origin: ShimOrigin, next_index: &mut u32) {
        let shim = origin.shim(self.zone);
        let hooks = self.requests.start_move(expr, &shim, &mut self.client);
        let deadline = Instant::now() + Duration::from_millis(MOVE_WAIT_MS);
        self.wait_until(deadline, origin, next_index, |r| r.answered(&hooks));
    }

    /// Ask every context for its call counts, without waiting - the answers are merged as they come.
    /// A context still answering the last request is not asked again (R4-S10).
    pub(crate) fn request_counts(&mut self) {
        self.requests.request_counts(&mut self.client);
    }

    /// Let every live context go and ask for its last counts, then wait for both until `deadline`, and
    /// count the contexts that did not confirm they were let go, each once: a page that answered
    /// anything but `ok` or `no-shim`, one that did not answer by the deadline, and one that may still
    /// have a hook - those may still be on the session clock, which the caller has to say (rule 6).
    ///
    /// The hooks go with the release. They die with the connection (measured: a page reloaded after
    /// the session comes up with no shim), but the connection outlives the release by the last look at
    /// the host's tree, and a page that navigated then loaded its next document on the session clock
    /// again and kept it after the session with no warning (R4-W5).
    ///
    /// The zone override goes last, once every answer is in or the deadline has passed. One context
    /// holds it for its whole renderer process, and the protocol does not order commands across
    /// sessions, so a reset sent with the last counts could reach the renderer before the count of
    /// another context, which then read the host's zone and was reported as having missed the
    /// session's (CodeRabbit on #89). A context let go no longer checks its zone ([`cdp::release_expr`]),
    /// and the last counts after this wait for the same deadline (`EmbeddedBridge::finish`), so a count
    /// still in flight when it passed is never read.
    ///
    /// The pages go back on `machine`, this machine's zone as the system names it (ADR-21). Taking the
    /// override away gives the page of an embedded engine nothing back: its renderer keeps the last zone
    /// it was given, also once the connection closes (measured on WebView2 154 in a hooked application,
    /// 2026-10-05 - the browser of a Chromium session, which is not hooked, did go back to the machine's
    /// zone). A named zone given last stays, with the machine's offsets on both sides of daylight saving.
    /// With no name the override is taken away all the same.
    ///
    /// Neither is taken on trust: an engine that does not know the name keeps the page where it was, and
    /// the answer to the override proves nothing. Once every context answered the zone, each is asked
    /// what it shows at two instants of the year, and judged against the session's offset and this
    /// machine's ([`crate::cdp_requests::zone_back`]) - the caller says the ones it cannot vouch for
    /// (rule 6). The release keeps [`ZONE_STAGE`] of the deadline for this.
    pub(crate) fn release(
        &mut self,
        expr: &str,
        machine: &MachineZone<'_>,
        origin: ShimOrigin,
        next_index: &mut u32,
        deadline: Instant,
    ) -> Released {
        self.ending = true;
        self.requests.request_counts(&mut self.client);
        self.requests.start_release(expr, &mut self.client);
        let Some(session) = self.zone else {
            self.wait_until(deadline, origin, next_index, |r| r.release_settled() && r.counts_settled());
            return Released { unconfirmed: self.requests.unreleased(), zone: ZoneTally::default() };
        };
        self.wait_until(before(deadline, ZONE_STAGE), origin, next_index, |r| r.release_settled() && r.counts_settled());
        let read = cdp::zone_read_expr(machine.at_ms);
        self.requests.give_zone_back(machine.name.unwrap_or(""), &read, &mut self.client);
        self.wait_until(before(deadline, READ_STAGE), origin, next_index, |r| r.zone_reads_sent() || r.zones_settled());
        self.requests.read_zones_now(&mut self.client);
        self.wait_until(deadline, origin, next_index, Requests::zone_reads_settled);
        Released { unconfirmed: self.requests.unreleased(), zone: self.requests.zone_tally(session, machine.offsets) }
    }

    /// The end of the session: ask for the last counts and wait for them until `deadline`, then take
    /// every clock move a live page has not answered as one it missed. A context that attaches from
    /// here on is let go without a shim.
    pub(crate) fn settle(&mut self, origin: ShimOrigin, next_index: &mut u32, deadline: Instant) {
        self.ending = true;
        self.requests.request_counts(&mut self.client);
        self.wait_until(deadline, origin, next_index, Requests::counts_settled);
        self.requests.settle_moves();
    }

    /// How many pages missed at least one clock move so far.
    pub(crate) fn moves_missed(&self) -> usize {
        self.requests.moves_missed()
    }

    /// Evaluate a JS expression in one live context and return the string it produced, if any.
    /// For a probe reading what a page shows - `document.title` - not for the session.
    pub(crate) fn evaluate_string(&mut self, index: u32, expr: &str) -> Option<String> {
        let sid = self.requests.contexts().iter().find(|c| c.index == index)?.session_id.clone();
        let reply = self
            .client
            .call("Runtime.evaluate", json!({ "expression": expr, "returnByValue": true }), Some(&sid))
            .ok()?;
        reply["result"]["value"].as_str().map(str::to_string)
    }

    /// The live contexts, in attach order.
    pub(crate) fn contexts(&self) -> &[CdpContext] {
        self.requests.contexts()
    }

    /// The audit's rows: every context index shimmed with a row of its own, in attach order, then the
    /// pool's row when contexts went past [`MAX_CONTEXTS`] (R5-P20).
    pub(crate) fn seen(&self) -> Vec<u32> {
        self.seen.iter().copied().chain(self.pool).collect()
    }

    /// How many contexts this attacher shimmed: the ones with a row of their own and the pooled ones.
    pub(crate) fn contexts_covered(&self) -> usize {
        self.seen.len() + self.pooled
    }

    /// How many contexts were counted in the pool's row.
    pub(crate) fn pooled(&self) -> usize {
        self.pooled
    }

    /// How many contexts attached and could not be shimmed, plus the ones shimmed that refused to
    /// attach the workers they start - a worker of theirs would run on the real clock unseen - and the
    /// pages whose shim never showed (R5-D46).
    pub(crate) fn failed(&self) -> usize {
        self.failed + self.awaiting.len()
    }
}

/// A target as it arrived: its session, its type and id as the browser named them, whether it was
/// paused on start (`waitingForDebugger`) - a target attached by name was not - and the session that
/// announced it, `None` for the browser's own, which a detach has to go through.
struct Arrival {
    sid: String,
    ty: String,
    tid: String,
    waiting: bool,
    parent: Option<String>,
}

/// The deadline of one attach: its own budget from `now`, or the caller's deadline when that comes
/// first. The pages that already exist are attached under the deadline of the session's start, and an
/// attach made there that took a fresh budget of its own could double the silence before the first
/// heartbeat - ten seconds to connect, ten more for one busy page (CodeRabbit on #85).
fn attach_deadline(now: Instant, budget: Duration, cap: Option<Instant>) -> Instant {
    let own = now + budget;
    cap.map_or(own, |cap| cap.min(own))
}

/// The instant `stage` before `deadline`, or the deadline itself where that cannot be written.
fn before(deadline: Instant, stage: Duration) -> Instant {
    deadline.checked_sub(stage).unwrap_or(deadline)
}

/// The target ids of the contexts that went away, in a ring of at most `kept`: once it is full, the
/// newest takes the place of the oldest. A `Vec` with its own cursor rather than a `VecDeque`, which
/// would do the same - CodeQL's generated models take every growth of a `VecDeque` for a log sink, and
/// an attacher holds a socket, so a queue of ids here read as log injection (alert #10 on #110, of the
/// class of #1 and #5, false positives checked in the analysis).
#[derive(Default)]
struct DetachedRing {
    ids: Vec<String>,
    next: usize,
}

/// [`Attacher::remember_detached`] over its parts, with the capacity as a parameter so the bound is
/// tested without thousands of targets: `target` joins the ring of the ones that went away, and the id
/// it pushes out is returned to be forgotten from the index map - unless `keep` says it is still needed.
fn remember_detached_in(ring: &mut DetachedRing, target: String, kept: usize, keep: impl Fn(&str) -> bool) -> Option<String> {
    if target.is_empty() || kept == 0 {
        return None;
    }
    if ring.ids.len() < kept {
        ring.ids.push(target);
        return None;
    }
    let slot = ring.next % kept;
    ring.next = (slot + 1) % kept;
    let old = std::mem::replace(&mut ring.ids[slot], target);
    (!keep(&old)).then_some(old)
}

/// Whether a context with this index is past the ceiling of own rows: the ceiling is on contexts ever
/// seen, so a re-attach of a known context (same index) always gets its row back, and only a NEW one
/// past the ceiling goes to the pool.
fn past_ceiling(seen: &[u32], index: u32) -> bool {
    !seen.contains(&index) && seen.len() >= MAX_CONTEXTS
}

/// The targets in a `Target.getTargets` reply worth attaching to: shimmable, named, and not yet
/// known to this attacher. Pure over the reply, so the filter is tested on a made-up browser.
fn new_targets(reply: &serde_json::Value, known: &HashMap<String, u32>) -> Vec<(String, String)> {
    reply["targetInfos"]
        .as_array()
        .map(|targets| {
            targets
                .iter()
                .map(|t| {
                    (
                        t["targetId"].as_str().unwrap_or("").to_string(),
                        t["type"].as_str().unwrap_or("").to_string(),
                    )
                })
                .filter(|(tid, ty)| !tid.is_empty() && cdp::is_shimmable(ty) && !known.contains_key(tid))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// R5-Z2: the ids of contexts that went away are kept for a while, so one that comes back keeps
    /// its index, and past the capacity the oldest is handed back to be forgotten - unless it is still
    /// needed. The ring stays at its bound.
    #[test]
    fn the_targets_that_went_away_are_remembered_up_to_a_bound() {
        let mut ring = DetachedRing::default();
        let keep = |t: &str| t == "T2";
        assert_eq!(remember_detached_in(&mut ring, "T1".into(), 2, keep), None);
        assert_eq!(remember_detached_in(&mut ring, "T2".into(), 2, keep), None, "room for two");
        assert_eq!(remember_detached_in(&mut ring, "T3".into(), 2, keep), Some("T1".into()), "the oldest goes");
        assert_eq!(remember_detached_in(&mut ring, "T4".into(), 2, keep), None, "one still needed is kept");
        assert_eq!(ring.ids.len(), 2, "the ring stays at its bound");
        assert_eq!(remember_detached_in(&mut ring, String::new(), 2, keep), None);
        assert_eq!(ring.ids.len(), 2, "a context with no id is nothing to remember");
    }

    /// CodeRabbit on #110: a context with an audit row of its own that went away keeps its index
    /// however many others went away after it, so when it comes back it is the same context - pushed
    /// out of the ring of ids, it came back under a new index and was counted twice.
    #[test]
    fn a_context_with_a_row_of_its_own_comes_back_as_itself_after_the_ring_turned() {
        let attached = |sid: &str, tid: &str| {
            let params = json!({ "sessionId": sid, "targetInfo": { "targetId": tid, "type": "worker" }, "waitingForDebugger": true });
            (serde_json::Value::Null, json!({ "method": "Target.attachedToTarget", "params": params }))
        };
        let gone = |sid: &str| (serde_json::Value::Null, json!({ "method": "Target.detachedFromTarget", "params": { "sessionId": sid } }));
        let (port, browser) = crate::cdp::fake_browser_holding(move |request| {
            let id = request["id"].clone();
            let session = request["sessionId"].as_str().unwrap_or("").to_string();
            match request["method"].as_str().unwrap_or("") {
                "Target.getTargets" => vec![(id, json!({ "targetInfos": [] })), attached("S1", "W1")],
                "Runtime.evaluate" => vec![(id, json!({ "result": { "value": "installed" } }))],
                // Each worker goes away once released, and the next one arrives: W1, W2, then W1 again.
                "Runtime.runIfWaitingForDebugger" if session == "S1" => vec![(id, json!({})), gone("S1"), attached("S2", "W2")],
                "Runtime.runIfWaitingForDebugger" if session == "S2" => vec![(id, json!({})), gone("S2"), attached("S3", "W1")],
                _ => vec![(id, json!({}))],
            }
        });
        let ws = crate::cdp::WsClient::connect("127.0.0.1", port, "/", Instant::now() + Duration::from_secs(5)).unwrap();
        let mut attacher = Attacher::over(cdp::CdpClient::from_ws(ws), port);
        attacher.set_budgets(Duration::from_millis(20), Duration::from_secs(2));
        attacher.detached_kept = 1;
        let origin = ShimOrigin { fake0: 0, real0: 0, mult: 1, dur: 1, scheduled: None };
        let mut next = 0;
        attacher.attach_existing(origin, &mut next, Instant::now() + Duration::from_secs(2)).unwrap();
        for _ in 0..30 {
            attacher.pump(origin, &mut next);
        }
        assert_eq!(attacher.seen().len(), 2, "W1 and W2, one row each");
        assert_eq!(attacher.contexts_covered(), 2, "W1 came back as itself");
        drop(attacher);
        drop(browser);
    }

    /// R5-Z3: a target the session will not drive - one with no JS of ours to cover, and a second
    /// session on a target that has one - is released and detached from, not left attached for the
    /// rest of the session.
    #[test]
    fn a_target_the_session_will_not_drive_is_released_and_detached_from() {
        let (port, browser) = crate::cdp::fake_browser_holding(move |request| {
            let id = request["id"].clone();
            match request["method"].as_str().unwrap_or("") {
                "Target.getTargets" => {
                    let attached = |sid: &str, tid: &str, ty: &str| {
                        let params = json!({ "sessionId": sid, "targetInfo": { "targetId": tid, "type": ty }, "waitingForDebugger": true });
                        (serde_json::Value::Null, json!({ "method": "Target.attachedToTarget", "params": params }))
                    };
                    // The target with no JS is announced by a page's own auto-attach.
                    let (none, mut by_page) = attached("S-GPU", "T-GPU", "other");
                    by_page["sessionId"] = json!("S-PAGE");
                    vec![
                        (id, json!({ "targetInfos": [] })),
                        (none, by_page),
                        attached("S-W1", "T-W", "worker"),
                        attached("S-W2", "T-W", "worker"),
                    ]
                }
                "Runtime.evaluate" => vec![(id, json!({ "result": { "value": "installed" } }))],
                // As Chromium answers it: the session that went names its target, which the context
                // the session drives shares.
                "Target.detachFromTarget" => {
                    let sid = request["params"]["sessionId"].as_str().unwrap_or("").to_string();
                    let tid = if sid == "S-W2" { "T-W" } else { "T-GPU" };
                    let gone = json!({ "method": "Target.detachedFromTarget", "params": { "sessionId": sid, "targetId": tid } });
                    vec![(id, json!({})), (serde_json::Value::Null, gone)]
                }
                _ => vec![(id, json!({}))],
            }
        });
        let ws = crate::cdp::WsClient::connect("127.0.0.1", port, "/", Instant::now() + Duration::from_secs(5)).unwrap();
        let mut attacher = Attacher::over(cdp::CdpClient::from_ws(ws), port);
        attacher.set_budgets(Duration::from_millis(20), Duration::from_secs(2));
        let origin = ShimOrigin { fake0: 0, real0: 0, mult: 1, dur: 1, scheduled: None };
        let mut next = 0;
        attacher.attach_existing(origin, &mut next, Instant::now() + Duration::from_secs(2)).unwrap();
        for _ in 0..20 {
            attacher.pump(origin, &mut next);
        }
        // Measured live before this was right: the second session's going names the target, and the
        // context the first session drives was dropped with it - a page under the session lost its
        // counts and the verdict said no JS time API was called.
        let live: Vec<&str> = attacher.contexts().iter().map(|c| c.session_id.as_str()).collect();
        assert_eq!(live, vec!["S-W1"], "the duplicate going does not take the context with it");
        drop(attacher);
        let log = browser.join().unwrap();
        let detached: Vec<(&str, &str)> = log
            .iter()
            .filter(|r| r["method"] == "Target.detachFromTarget")
            .map(|r| (r["params"]["sessionId"].as_str().unwrap_or(""), r["sessionId"].as_str().unwrap_or("browser")))
            .collect();
        // Each through the session that announced it: Chromium answers a detach sent to the browser for
        // a session a page announced with "No session with given id" (measured 2026-11-09).
        assert_eq!(
            detached,
            vec![("S-GPU", "S-PAGE"), ("S-W2", "browser")],
            "the target with no JS and the second session, not the shimmed one"
        );
        let resumed = |sid: &str| log.iter().any(|r| r["method"] == "Runtime.runIfWaitingForDebugger" && r["sessionId"] == sid);
        assert!(resumed("S-GPU") && resumed("S-W2"), "each released before it is let go");
    }

    /// A service worker a page registers is announced twice - by the browser's auto-attach and by the
    /// page's own - and a target two sessions hold paused on start answers neither until both release
    /// it, the shim included (measured on two Chromium browsers, 2026-11-09). The second session is let
    /// go as soon as it is announced, through the session that announced it, whether it was queued
    /// before the attach began or came while the shim was awaited. Before, the shim waited out the
    /// attach's deadline, then once more in the second session, and the worker ran on the real clock.
    /// The session the attach is made in, announced again, is not let go.
    #[test]
    fn a_second_session_of_a_target_being_attached_is_let_go_as_it_comes() {
        for during_the_shim in [false, true] {
            let announced = |sid: &str, parent: Option<&str>| {
                let mut event = json!({
                    "method": "Target.attachedToTarget",
                    "params": { "sessionId": sid, "targetInfo": { "targetId": "T-SW", "type": "service_worker" }, "waitingForDebugger": true }
                });
                if let Some(parent) = parent {
                    event["sessionId"] = json!(parent);
                }
                (serde_json::Value::Null, event)
            };
            let installed = json!({ "result": { "value": "installed" } });
            let mut held = None;
            let mut released = false;
            let (port, browser) = crate::cdp::fake_browser_holding(move |request| {
                let id = request["id"].clone();
                let session = request["sessionId"].as_str().unwrap_or("").to_string();
                match request["method"].as_str().unwrap_or("") {
                    "Target.getTargets" if during_the_shim => vec![(id, json!({ "targetInfos": [] })), announced("S-SW1", None)],
                    "Target.getTargets" => {
                        vec![announced("S-SW1", None), announced("S-SW2", Some("S-PAGE")), (id, json!({ "targetInfos": [] }))]
                    }
                    // The worker runs nothing, the shim included, while the second session holds it.
                    "Runtime.evaluate" if session == "S-SW1" && !released => {
                        held = Some(id);
                        if during_the_shim { vec![announced("S-SW1", None), announced("S-SW2", Some("S-PAGE"))] } else { Vec::new() }
                    }
                    "Runtime.runIfWaitingForDebugger" if session == "S-SW2" => {
                        released = true;
                        let mut out = vec![(id, json!({}))];
                        out.extend(held.take().map(|shim| (shim, installed.clone())));
                        out
                    }
                    "Runtime.evaluate" => vec![(id, installed.clone())],
                    _ => vec![(id, json!({}))],
                }
            });
            let ws = crate::cdp::WsClient::connect("127.0.0.1", port, "/", Instant::now() + Duration::from_secs(5)).unwrap();
            let mut attacher = Attacher::over(cdp::CdpClient::from_ws(ws), port);
            attacher.set_budgets(Duration::from_millis(20), Duration::from_secs(2));
            let origin = ShimOrigin { fake0: 0, real0: 0, mult: 1, dur: 1, scheduled: None };
            let mut next = 0;
            attacher.attach_existing(origin, &mut next, Instant::now() + Duration::from_secs(2)).unwrap();
            for _ in 0..10 {
                attacher.pump(origin, &mut next);
            }
            let case = if during_the_shim { "announced while the shim was awaited" } else { "queued before the attach" };
            assert_eq!((attacher.contexts_covered(), attacher.failed()), (1, 0), "{case}: the worker took the shim");
            drop(attacher);
            let log = browser.join().unwrap();
            let detached: Vec<(&str, &str)> = log
                .iter()
                .filter(|r| r["method"] == "Target.detachFromTarget")
                .map(|r| (r["params"]["sessionId"].as_str().unwrap_or(""), r["sessionId"].as_str().unwrap_or("browser")))
                .collect();
            assert_eq!(detached, vec![("S-SW2", "S-PAGE")], "{case}: the second session alone, through the page that announced it");
        }
    }

    /// The two other targets the attach itself lets go are detached through the session that announced
    /// them as well: a context found on the session clock natively, and a second session of a target
    /// that is already covered - announced once the attach is over, so it is the attach that sees it,
    /// not the client (CodeRabbit on #160). Both are announced by a page's own auto-attach here, whose
    /// sessions Chromium does not know at the browser.
    #[test]
    fn a_target_the_attach_lets_go_is_detached_through_the_session_that_announced_it() {
        let session_now = 2_147_483_647_000_i64;
        let announced = |sid: &str, tid: &str, ty: &str, parent: Option<&str>| {
            let mut event = json!({
                "method": "Target.attachedToTarget",
                "params": { "sessionId": sid, "targetInfo": { "targetId": tid, "type": ty }, "waitingForDebugger": true }
            });
            if let Some(parent) = parent {
                event["sessionId"] = json!(parent);
            }
            (serde_json::Value::Null, event)
        };
        let (port, browser) = crate::cdp::fake_browser_holding(move |request| {
            let id = request["id"].clone();
            let session = request["sessionId"].as_str().unwrap_or("").to_string();
            let probe = request["params"]["expression"] == CLOCK_PROBE_EXPR;
            match request["method"].as_str().unwrap_or("") {
                "Target.getTargets" => vec![
                    (id, json!({ "targetInfos": [] })),
                    announced("S-NATIVE", "T-NATIVE", "iframe", Some("S-PAGE")),
                    announced("S-W1", "T-W", "worker", None),
                ],
                // The frame reads the session clock through the native hook, the worker the real one.
                "Runtime.evaluate" if probe && session == "S-NATIVE" => vec![(id, json!({ "result": { "value": session_now.to_string() } }))],
                "Runtime.evaluate" if probe => vec![(id, json!({ "result": { "value": now_epoch_ms().to_string() } }))],
                "Runtime.evaluate" => vec![(id, json!({ "result": { "value": "installed" } }))],
                // The worker is covered once released, and the page announces it again after that.
                "Runtime.runIfWaitingForDebugger" if session == "S-W1" => {
                    vec![(id, json!({})), announced("S-W2", "T-W", "worker", Some("S-PAGE"))]
                }
                _ => vec![(id, json!({}))],
            }
        });
        let ws = crate::cdp::WsClient::connect("127.0.0.1", port, "/", Instant::now() + Duration::from_secs(5)).unwrap();
        let mut attacher = Attacher::over(cdp::CdpClient::from_ws(ws), port);
        attacher.set_budgets(Duration::from_millis(20), Duration::from_secs(2));
        attacher.probe_clock_before_shim();
        let origin = ShimOrigin { fake0: session_now, real0: now_epoch_ms(), mult: 1, dur: 1, scheduled: None };
        let mut next = 0;
        attacher.attach_existing(origin, &mut next, Instant::now() + Duration::from_secs(2)).unwrap();
        for _ in 0..10 {
            attacher.pump(origin, &mut next);
        }
        assert_eq!(attacher.native(), 1, "the frame on the session clock natively was left without the shim");
        let live: Vec<&str> = attacher.contexts().iter().map(|c| c.session_id.as_str()).collect();
        assert_eq!(live, vec!["S-W1"], "the worker is covered once, in its first session");
        drop(attacher);
        let log = browser.join().unwrap();
        let detached: Vec<(&str, &str)> = log
            .iter()
            .filter(|r| r["method"] == "Target.detachFromTarget")
            .map(|r| (r["params"]["sessionId"].as_str().unwrap_or(""), r["sessionId"].as_str().unwrap_or("browser")))
            .collect();
        assert_eq!(detached, vec![("S-NATIVE", "S-PAGE"), ("S-W2", "S-PAGE")], "each through the page that announced it");
    }

    /// R5-D6: a target the by-name pass could not attach to ran unreached, and counts as such - it used
    /// to be skipped without a word, and the verdict could say every context ran on the session clock.
    #[test]
    fn a_target_that_could_not_be_attached_to_counts_as_unreached() {
        let (port, browser) = crate::cdp::fake_browser_holding(move |request| {
            let id = request["id"].clone();
            match request["method"].as_str().unwrap_or("") {
                "Target.getTargets" => vec![(id, json!({ "targetInfos": [{ "targetId": "T1", "type": "page" }, { "targetId": "T2", "type": "page" }] }))],
                "Target.attachToTarget" if request["params"]["targetId"] == "T2" => vec![(id, json!({}))],
                "Target.attachToTarget" => vec![(id, json!({ "sessionId": "S1" }))],
                "Runtime.evaluate" => vec![(id, json!({ "result": { "value": "installed" } }))],
                _ => vec![(id, json!({}))],
            }
        });
        let ws = crate::cdp::WsClient::connect("127.0.0.1", port, "/", Instant::now() + Duration::from_secs(5)).unwrap();
        let mut attacher = Attacher::over(cdp::CdpClient::from_ws(ws), port);
        attacher.set_budgets(Duration::from_millis(20), Duration::from_secs(2));
        let origin = ShimOrigin { fake0: 0, real0: 0, mult: 1, dur: 1, scheduled: None };
        let mut next = 0;
        assert_eq!(attacher.attach_existing(origin, &mut next, Instant::now() + Duration::from_secs(2)).unwrap(), 1);
        assert_eq!(attacher.failed(), 1, "the one it could not attach to");
        drop(attacher);
        drop(browser);
    }

    /// A page auto-attach delivers paused on start, before its first document, answers nothing until
    /// it is released (measured on an Electron application whose window opened after the session armed
    /// auto-attach). The attacher tells the injection the page is paused, so it is released with its
    /// shim and covered, not waited for until the deadline and counted as unreached.
    #[test]
    fn a_page_delivered_paused_on_start_is_covered_though_it_answers_only_once_released() {
        let mut held = Vec::new();
        let (port, browser) = crate::cdp::fake_browser_holding(move |request| {
            let id = request["id"].clone();
            let session = request["sessionId"].as_str().unwrap_or("").to_string();
            let answer = match request["method"].as_str().unwrap_or("") {
                "Target.getTargets" => {
                    let event = json!({
                        "method": "Target.attachedToTarget",
                        "params": { "sessionId": "S-P", "targetInfo": { "targetId": "T-P", "type": "page", "url": "" }, "waitingForDebugger": true }
                    });
                    return vec![(id, json!({ "targetInfos": [] })), (serde_json::Value::Null, event)];
                }
                "Page.addScriptToEvaluateOnNewDocument" => json!({ "identifier": "h1" }),
                "Runtime.evaluate" => json!({ "result": { "value": "already" } }),
                "Runtime.runIfWaitingForDebugger" if session == "S-P" => {
                    let mut now = std::mem::take(&mut held);
                    now.push((id, json!({})));
                    return now;
                }
                _ => json!({}),
            };
            if session == "S-P" {
                held.push((id, answer));
                Vec::new()
            } else {
                vec![(id, answer)]
            }
        });
        let ws = crate::cdp::WsClient::connect("127.0.0.1", port, "/", Instant::now() + Duration::from_secs(5)).unwrap();
        let mut attacher = Attacher::over(cdp::CdpClient::from_ws(ws), port);
        attacher.set_budgets(Duration::from_millis(20), Duration::from_secs(2));
        let origin = ShimOrigin { fake0: 0, real0: 0, mult: 1, dur: 1, scheduled: None };
        let mut next = 0;
        attacher.attach_existing(origin, &mut next, Instant::now() + Duration::from_secs(2)).unwrap();
        for _ in 0..10 {
            attacher.pump(origin, &mut next);
        }
        let live: Vec<&str> = attacher.contexts().iter().map(|c| c.session_id.as_str()).collect();
        assert_eq!(live, vec!["S-P"], "the paused page is covered");
        assert_eq!(attacher.failed(), 0, "and not counted as unreached");
        drop(attacher);
        drop(browser);
    }

    /// R5-P7: a context reached while it was already running is counted, once - a page whose shim
    /// found the page's own scripts before it, and a worker that was not paused on start. A worker
    /// paused on start, and a page whose shim found no scripts, are not.
    #[test]
    fn a_context_reached_while_running_is_counted_once() {
        let (port, browser) = crate::cdp::fake_browser_holding(move |request| {
            let id = request["id"].clone();
            let session = request["sessionId"].as_str().unwrap_or("").to_string();
            match request["method"].as_str().unwrap_or("") {
                "Target.getTargets" => {
                    let attached = |sid: &str, tid: &str, ty: &str, waiting: bool| {
                        let params = json!({ "sessionId": sid, "targetInfo": { "targetId": tid, "type": ty }, "waitingForDebugger": waiting });
                        (serde_json::Value::Null, json!({ "method": "Target.attachedToTarget", "params": params }))
                    };
                    vec![
                        (id, json!({ "targetInfos": [] })),
                        attached("S-LOADED", "T-LOADED", "page", false),
                        attached("S-BLANK", "T-BLANK", "page", false),
                        attached("S-RUN", "T-RUN", "worker", false),
                        attached("S-NEW", "T-NEW", "worker", true),
                    ]
                }
                "Runtime.evaluate" => {
                    let answer = if session == "S-LOADED" { "running" } else { "installed" };
                    vec![(id, json!({ "result": { "value": answer } }))]
                }
                _ => vec![(id, json!({}))],
            }
        });
        let ws = crate::cdp::WsClient::connect("127.0.0.1", port, "/", Instant::now() + Duration::from_secs(5)).unwrap();
        let mut attacher = Attacher::over(cdp::CdpClient::from_ws(ws), port);
        attacher.set_budgets(Duration::from_millis(20), Duration::from_secs(2));
        let origin = ShimOrigin { fake0: 0, real0: 0, mult: 1, dur: 1, scheduled: None };
        let mut next = 0;
        attacher.attach_existing(origin, &mut next, Instant::now() + Duration::from_secs(2)).unwrap();
        for _ in 0..20 {
            attacher.pump(origin, &mut next);
        }
        assert_eq!(attacher.contexts().len(), 4, "all four were shimmed");
        assert_eq!(attacher.into_outcome().reached_running, 2, "the loaded page and the worker that was not paused");
        drop(browser);
    }

    /// A by-name attach is announced again as an attach event in flatten mode (measured on Electron 44),
    /// and that is the session the attacher drives, not a second one: it is neither let go nor
    /// detached. Letting it go was harmless while it meant a resume, and took the page off the session
    /// once it meant a detach (R5-Z3) - measured live, the page's counts never came and the verdict
    /// said no JS time API was called.
    #[test]
    fn a_by_name_attach_announced_again_is_the_session_it_drives() {
        let (port, browser) = crate::cdp::fake_browser_holding(move |request| {
            let id = request["id"].clone();
            match request["method"].as_str().unwrap_or("") {
                "Target.getTargets" => vec![(id, json!({ "targetInfos": [{ "targetId": "T-P", "type": "page" }] }))],
                "Target.attachToTarget" => {
                    let echo = json!({ "method": "Target.attachedToTarget", "params": {
                        "sessionId": "S-P", "targetInfo": { "targetId": "T-P", "type": "page" }, "waitingForDebugger": false } });
                    vec![(id, json!({ "sessionId": "S-P" })), (serde_json::Value::Null, echo)]
                }
                "Runtime.evaluate" => vec![(id, json!({ "result": { "value": "installed" } }))],
                _ => vec![(id, json!({}))],
            }
        });
        let ws = crate::cdp::WsClient::connect("127.0.0.1", port, "/", Instant::now() + Duration::from_secs(5)).unwrap();
        let mut attacher = Attacher::over(cdp::CdpClient::from_ws(ws), port);
        attacher.set_budgets(Duration::from_millis(20), Duration::from_secs(2));
        let origin = ShimOrigin { fake0: 0, real0: 0, mult: 1, dur: 1, scheduled: None };
        let mut next = 0;
        attacher.attach_existing(origin, &mut next, Instant::now() + Duration::from_secs(2)).unwrap();
        for _ in 0..20 {
            attacher.pump(origin, &mut next);
        }
        let live: Vec<&str> = attacher.contexts().iter().map(|c| c.session_id.as_str()).collect();
        assert_eq!(live, vec!["S-P"]);
        drop(attacher);
        let log = browser.join().unwrap();
        assert!(!log.iter().any(|r| r["method"] == "Target.detachFromTarget"), "the session it drives is not detached");
    }

    /// R5-P20: a context past the ceiling of own rows is shimmed like any other and counted in one row
    /// together with the rest past it. It used to be refused the shim and run on the real clock.
    /// Measured on Electron 44 before the change: 300 workers, one after another, gave "contexts: 256",
    /// `partial` and exit code 10.
    #[test]
    fn contexts_past_the_ceiling_are_shimmed_and_counted_together_in_the_last_row() {
        const EXTRA: usize = 4;
        let total = MAX_CONTEXTS + EXTRA;
        let (port, browser) = crate::cdp::fake_browser_holding(move |request| {
            let id = request["id"].clone();
            match request["method"].as_str().unwrap_or("") {
                "Target.getTargets" => {
                    let targets: Vec<_> = (0..total).map(|n| json!({ "targetId": format!("W{n}"), "type": "worker" })).collect();
                    vec![(id, json!({ "targetInfos": targets }))]
                }
                "Target.attachToTarget" => {
                    let target = request["params"]["targetId"].as_str().unwrap_or("");
                    vec![(id, json!({ "sessionId": format!("S-{target}") }))]
                }
                // The shim answers as installed, and every count request finds one call of Date.now.
                "Runtime.evaluate" if request["params"]["expression"].as_str().unwrap_or("").contains("audit") => {
                    vec![(id, json!({ "result": { "value": { "now": 1 } } }))]
                }
                "Runtime.evaluate" => vec![(id, json!({ "result": { "value": "installed" } }))],
                _ => vec![(id, json!({}))],
            }
        });
        let ws = crate::cdp::WsClient::connect("127.0.0.1", port, "/", Instant::now() + Duration::from_secs(5)).unwrap();
        let mut attacher = Attacher::over(cdp::CdpClient::from_ws(ws), port);
        attacher.set_budgets(Duration::from_millis(20), Duration::from_secs(2));
        let origin = ShimOrigin { fake0: 0, real0: 0, mult: 1, dur: 1, scheduled: None };
        let mut next = 0;
        assert_eq!(attacher.attach_existing(origin, &mut next, Instant::now() + Duration::from_secs(30)).unwrap(), total);
        attacher.settle(origin, &mut next, Instant::now() + Duration::from_secs(5));
        let outcome = attacher.into_outcome();
        drop(browser);
        assert_eq!(outcome.failed, 0, "no context was refused");
        assert_eq!(outcome.contexts, total, "every context counts");
        assert_eq!(outcome.pooled, EXTRA);
        assert_eq!(outcome.seen.len(), MAX_CONTEXTS + 1, "the own rows, then the pool's");
        let pool = *outcome.seen.last().unwrap();
        assert_eq!(outcome.counts.get(&(pool, "worker Date.now".to_string())), Some(&(EXTRA as u64)), "the pool's calls, summed");
        assert_eq!(outcome.counts.get(&(outcome.seen[0], "worker Date.now".to_string())), Some(&1), "an own row is its own");
    }

    /// Measured on Chromium: a second page in a renderer process reads the zone override the first
    /// one holds, and goes back to the host's zone when the holder closes. So when a context goes away,
    /// every one left is given the zone again - once, in the turn that saw it go - and the one that
    /// went is not.
    #[test]
    fn a_context_that_goes_away_has_the_zone_given_again_to_the_rest() {
        let mut detach_sent = false;
        let (port, browser) = crate::cdp::fake_browser_holding(move |request| {
            let id = request["id"].clone();
            let session = request["sessionId"].as_str().unwrap_or("");
            match request["method"].as_str().unwrap_or("") {
                "Target.getTargets" => vec![(
                    id,
                    json!({ "targetInfos": [{ "targetId": "T1", "type": "page" }, { "targetId": "T2", "type": "page" }] }),
                )],
                "Target.attachToTarget" => {
                    let target = request["params"]["targetId"].as_str().unwrap_or("");
                    vec![(id, json!({ "sessionId": target.replace('T', "S") }))]
                }
                "Page.addScriptToEvaluateOnNewDocument" => vec![(id, json!({ "identifier": "h" }))],
                "Runtime.evaluate" => vec![(id, json!({ "result": {} }))],
                // The second page is in: the first one closes.
                "Runtime.runIfWaitingForDebugger" if session == "S2" && !detach_sent => {
                    detach_sent = true;
                    let gone = json!({ "method": "Target.detachedFromTarget", "params": { "sessionId": "S1", "targetId": "T1" } });
                    vec![(id, json!({})), (serde_json::Value::Null, gone)]
                }
                _ => vec![(id, json!({}))],
            }
        });
        let ws = crate::cdp::WsClient::connect("127.0.0.1", port, "/", Instant::now() + Duration::from_secs(5)).unwrap();
        let mut attacher = Attacher::over(cdp::CdpClient::from_ws(ws), port);
        attacher.set_budgets(Duration::from_millis(20), Duration::from_secs(2));
        attacher.set_zone(-330);
        let origin = ShimOrigin { fake0: 0, real0: 0, mult: 1, dur: 1, scheduled: None };
        let mut next = 0;
        assert_eq!(attacher.attach_existing(origin, &mut next, Instant::now() + Duration::from_secs(2)).unwrap(), 2);
        let detached = (0..50).any(|_| attacher.pump(origin, &mut next) == Pumped::Detached);
        assert!(detached, "the fake browser said the first page went away");
        attacher.pump(origin, &mut next);
        drop(attacher);
        let log = browser.join().unwrap();
        let zone_to = |session: &str| {
            log.iter()
                .filter(|r| r["method"] == "Emulation.setTimezoneOverride" && r["sessionId"] == session)
                .inspect(|r| assert_eq!(r["params"]["timezoneId"], "GMT+05:30"))
                .count()
        };
        assert_eq!(zone_to("S1"), 1, "the page that went away is not asked again");
        assert_eq!(zone_to("S2"), 2, "the one left is given the zone again, once");
    }

    /// R5-F11, R5-D46: a frame of another site reached before its first document waits for its shim. It
    /// is no covered context and no audit row until an audit answers from its shim - counted as one the
    /// session could not cover while it waits, at the end too - and is covered once one does.
    #[test]
    fn a_frame_waiting_for_its_shim_is_covered_only_once_its_audit_shows_it() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let loaded = Arc::new(AtomicBool::new(false));
        let document = Arc::clone(&loaded);
        let (port, browser) = crate::cdp::fake_browser_holding(move |request| {
            let id = request["id"].clone();
            let expr = request["params"]["expression"].as_str().unwrap_or("");
            match request["method"].as_str().unwrap_or("") {
                "Target.getTargets" => vec![(id, json!({ "targetInfos": [{ "targetId": "F1", "type": "iframe" }] }))],
                "Target.attachToTarget" => vec![(id, json!({ "sessionId": "S1" }))],
                "Page.addScriptToEvaluateOnNewDocument" => vec![(id, json!({ "identifier": "h" }))],
                "Runtime.evaluate" if expr == cdp::COUNTS_EXPR => {
                    let value = if document.load(Ordering::SeqCst) { json!({ "now": 2, "frames": 0 }) } else { serde_json::Value::Null };
                    vec![(id, json!({ "result": { "value": value } }))]
                }
                "Runtime.evaluate" => {
                    vec![(serde_json::Value::Null, json!({ "id": id, "error": { "code": -32000, "message": "Cannot find default execution context" } }))]
                }
                _ => vec![(id, json!({}))],
            }
        });
        let ws = crate::cdp::WsClient::connect("127.0.0.1", port, "/", Instant::now() + Duration::from_secs(5)).unwrap();
        let mut attacher = Attacher::over(cdp::CdpClient::from_ws(ws), port);
        attacher.set_budgets(Duration::from_millis(20), Duration::from_secs(2));
        let origin = ShimOrigin { fake0: 0, real0: 0, mult: 1, dur: 1, scheduled: None };
        let mut next = 0;
        assert_eq!(attacher.attach_existing(origin, &mut next, Instant::now() + Duration::from_secs(2)).unwrap(), 1);
        let audit = |attacher: &mut Attacher, next: &mut u32| {
            attacher.request_counts();
            for _ in 0..20 {
                attacher.pump(origin, next);
            }
        };
        audit(&mut attacher, &mut next);
        assert_eq!((attacher.contexts_covered(), attacher.failed()), (0, 1), "waiting: not covered, counted as not");
        assert!(attacher.seen().is_empty(), "and no row of its own");
        loaded.store(true, Ordering::SeqCst);
        audit(&mut attacher, &mut next);
        assert_eq!((attacher.contexts_covered(), attacher.failed()), (1, 0), "its shim answered: covered");
        assert_eq!(attacher.seen(), [1], "the row of the first context");
        drop(attacher);
        drop(browser.join());
    }

    /// R5-D46, CodeRabbit on #111: a frame that went away before any audit showed its shim stays one the
    /// session could not cover - its going does not take it off the count, at the end too.
    #[test]
    fn a_frame_that_goes_before_its_shim_showed_stays_uncovered() {
        let (port, browser) = crate::cdp::fake_browser_holding(move |request| {
            let id = request["id"].clone();
            match request["method"].as_str().unwrap_or("") {
                "Target.getTargets" => vec![(id, json!({ "targetInfos": [{ "targetId": "F1", "type": "iframe" }] }))],
                "Target.attachToTarget" => vec![(id, json!({ "sessionId": "S1" }))],
                "Page.addScriptToEvaluateOnNewDocument" => vec![(id, json!({ "identifier": "h" }))],
                // Resumed, and gone before its first document - the frame was removed from its page.
                "Runtime.runIfWaitingForDebugger" => vec![
                    (id, json!({})),
                    (serde_json::Value::Null, json!({ "method": "Target.detachedFromTarget", "params": { "sessionId": "S1", "targetId": "F1" } })),
                ],
                "Runtime.evaluate" => {
                    vec![(serde_json::Value::Null, json!({ "id": id, "error": { "code": -32000, "message": "Cannot find default execution context" } }))]
                }
                _ => vec![(id, json!({}))],
            }
        });
        let ws = crate::cdp::WsClient::connect("127.0.0.1", port, "/", Instant::now() + Duration::from_secs(5)).unwrap();
        let mut attacher = Attacher::over(cdp::CdpClient::from_ws(ws), port);
        attacher.set_budgets(Duration::from_millis(20), Duration::from_secs(2));
        let origin = ShimOrigin { fake0: 0, real0: 0, mult: 1, dur: 1, scheduled: None };
        let mut next = 0;
        assert_eq!(attacher.attach_existing(origin, &mut next, Instant::now() + Duration::from_secs(2)).unwrap(), 1);
        let detached = (0..50).any(|_| attacher.pump(origin, &mut next) == Pumped::Detached);
        assert!(detached, "the fake browser said the frame went away");
        assert!(attacher.contexts().is_empty(), "it is no longer driven");
        let outcome = attacher.into_outcome();
        assert_eq!((outcome.contexts, outcome.failed), (0, 1), "and it is still one the session could not cover");
        assert!(outcome.seen.is_empty());
        drop(browser.join());
    }

    /// R5-F10, CodeRabbit on #129: a connection that closes while a context whose audit never answered is
    /// still listed - the application ended, with no event for its contexts - counts that context unread,
    /// once, however often the closed connection is polled.
    #[test]
    fn a_connection_that_closes_counts_the_contexts_it_took_unread() {
        let (port, browser) = crate::cdp::fake_browser_holding(move |request| {
            let id = request["id"].clone();
            match request["method"].as_str().unwrap_or("") {
                "Target.getTargets" => vec![(id, json!({ "targetInfos": [{ "targetId": "W1", "type": "worker" }] }))],
                "Target.attachToTarget" => vec![(id, json!({ "sessionId": "S1" }))],
                "Runtime.evaluate" => vec![(id, json!({ "result": { "value": "installed" } }))],
                _ => vec![(id, json!({}))],
            }
        });
        let ws = crate::cdp::WsClient::connect("127.0.0.1", port, "/", Instant::now() + Duration::from_secs(5)).unwrap();
        let mut attacher = Attacher::over(cdp::CdpClient::from_ws(ws), port);
        attacher.set_budgets(Duration::from_millis(20), Duration::from_secs(2));
        let origin = ShimOrigin { fake0: 0, real0: 0, mult: 1, dur: 1, scheduled: None };
        let mut next = 0;
        assert_eq!(attacher.attach_existing(origin, &mut next, Instant::now() + Duration::from_secs(2)).unwrap(), 1);
        let closed = || Err(io::Error::new(io::ErrorKind::ConnectionReset, "the browser went away"));
        assert_eq!(attacher.handle(closed(), origin, &mut next), Pumped::Closed);
        assert_eq!(attacher.handle(closed(), origin, &mut next), Pumped::Closed);
        let outcome = attacher.into_outcome();
        drop(browser);
        assert_eq!((outcome.contexts, outcome.unread), (1, 1), "covered, and gone unread once");
    }

    /// CodeRabbit on #89: two pages share one renderer and its zone override, and the protocol does not
    /// order commands across sessions. Here the second page's renderer is busy: its last count and its
    /// release wait until it comes free, which is once the attacher has gone quiet after sending the
    /// release, and the count reads another zone if the session's was replaced before then. The release
    /// replaces the zone only after the answers, so the count reads the session's zone, and both pages
    /// are then put on the machine's zone by its name (ADR-21). A renderer that answered the count the
    /// moment the release arrived let an attacher through that replaced the zone right after sending the
    /// release, without waiting for the answers (the registry's entry went through).
    #[test]
    fn the_release_takes_the_zone_away_only_after_the_last_counts() {
        let mut reset = false;
        let mut held: Option<serde_json::Value> = None;
        let mut release: Option<serde_json::Value> = None;
        let count = |reset: bool| json!({ "result": { "value": { "now": 1, "zone": u8::from(reset) } } });
        let (port, browser) = crate::cdp::fake_browser_paced(Some(Duration::from_millis(200)), move |request| {
            let Some(request) = request else {
                // Free again: the count it was busy with, then the release behind it.
                let Some(release_id) = release.take() else {
                    return Vec::new();
                };
                let mut out: Vec<_> = held.take().map(|count_id| (count_id, count(reset))).into_iter().collect();
                out.push((release_id, json!({ "result": { "value": "ok" } })));
                return out;
            };
            let id = request["id"].clone();
            let session = request["sessionId"].as_str().unwrap_or("");
            let expr = request["params"]["expression"].as_str().unwrap_or("");
            match request["method"].as_str().unwrap_or("") {
                "Target.getTargets" => vec![(
                    id,
                    json!({ "targetInfos": [{ "targetId": "T1", "type": "page" }, { "targetId": "T2", "type": "page" }] }),
                )],
                "Target.attachToTarget" => {
                    let target = request["params"]["targetId"].as_str().unwrap_or("");
                    vec![(id, json!({ "sessionId": target.replace('T', "S") }))]
                }
                "Page.addScriptToEvaluateOnNewDocument" => vec![(id, json!({ "identifier": "h" }))],
                "Emulation.setTimezoneOverride" => {
                    reset |= request["params"]["timezoneId"] != "GMT+05:30";
                    vec![(id, json!({}))]
                }
                "Runtime.evaluate" if expr == cdp::COUNTS_EXPR && session == "S2" => {
                    held = Some(id);
                    Vec::new()
                }
                "Runtime.evaluate" if expr == cdp::COUNTS_EXPR => vec![(id, count(reset))],
                // The zone read: the machine's offsets once the zone was given back, the session's before.
                "Runtime.evaluate" if expr.contains("S.zoneRead(") => {
                    let shown = if reset { json!([-60, -120]) } else { json!([-330, -330]) };
                    vec![(id, json!({ "result": { "value": shown } }))]
                }
                "Runtime.evaluate" if expr == cdp::release_expr() && session == "S2" => {
                    release = Some(id);
                    Vec::new()
                }
                "Runtime.evaluate" if expr == cdp::release_expr() => vec![(id, json!({ "result": { "value": "ok" } }))],
                "Runtime.evaluate" => vec![(id, json!({ "result": {} }))],
                _ => vec![(id, json!({}))],
            }
        });
        let ws = crate::cdp::WsClient::connect("127.0.0.1", port, "/", Instant::now() + Duration::from_secs(5)).unwrap();
        let mut attacher = Attacher::over(cdp::CdpClient::from_ws(ws), port);
        attacher.set_budgets(Duration::from_millis(20), Duration::from_secs(2));
        attacher.set_zone(-330);
        let origin = ShimOrigin { fake0: 0, real0: 0, mult: 1, dur: 1, scheduled: None };
        let mut next = 0;
        assert_eq!(attacher.attach_existing(origin, &mut next, Instant::now() + Duration::from_secs(2)).unwrap(), 2);
        let deadline = Instant::now() + Duration::from_secs(2);
        let machine = MachineZone { name: Some("Europe/Warsaw"), at_ms: [1_768_478_400_000, 1_784_116_800_000], offsets: Some([-60, -120]) };
        let released = attacher.release(&cdp::release_expr(), &machine, origin, &mut next, deadline);
        assert_eq!(released.unconfirmed, 0, "both pages confirmed they were let go");
        assert_eq!(released.zone, ZoneTally { back: 2, kept: 0, unanswered: 0 }, "both read back the machine's zone");
        assert_eq!(attacher.into_outcome().zone_missed, 0, "the busy page's last count read the session's zone");
        let log = browser.join().unwrap();
        let zone_of = |r: &serde_json::Value| r["method"] == "Emulation.setTimezoneOverride" && r["params"]["timezoneId"] != "GMT+05:30";
        let resets: Vec<usize> = (0..log.len()).filter(|&i| zone_of(&log[i])).collect();
        assert_eq!(resets.len(), 2, "both pages are given another zone");
        assert!(resets.iter().all(|&i| log[i]["params"]["timezoneId"] == "Europe/Warsaw"), "the machine's, by its name");
        let last_release = log.iter().rposition(|r| r["params"]["expression"] == cdp::release_expr());
        assert!(last_release.is_some_and(|r| resets.iter().all(|&i| i > r)), "the zone goes after the releases");
        let reads: Vec<usize> = (0..log.len())
            .filter(|&i| log[i]["params"]["expression"] == cdp::zone_read_expr(machine.at_ms).as_str())
            .collect();
        assert_eq!(reads.len(), 2, "each page is asked what it shows");
        assert!(reads.iter().all(|&i| resets.iter().all(|&z| i > z)), "the read goes after the zone");
    }

    /// Stability of the end: a page busy with its own JS answers neither the release nor anything after
    /// it, and the release does not wait it out to the deadline before the zone - the zone and its read
    /// keep their part of the deadline, so the page that is not busy is still found back on the machine's
    /// zone, and the busy one is counted as one that did not answer.
    #[test]
    fn a_busy_page_does_not_take_the_zone_check_from_the_others() {
        let (port, browser) = crate::cdp::fake_browser_holding(move |request| {
            let id = request["id"].clone();
            let session = request["sessionId"].as_str().unwrap_or("");
            let expr = request["params"]["expression"].as_str().unwrap_or("");
            match request["method"].as_str().unwrap_or("") {
                "Target.getTargets" => vec![(
                    id,
                    json!({ "targetInfos": [{ "targetId": "T1", "type": "page" }, { "targetId": "T2", "type": "page" }] }),
                )],
                "Target.attachToTarget" => {
                    let target = request["params"]["targetId"].as_str().unwrap_or("");
                    vec![(id, json!({ "sessionId": target.replace('T', "S") }))]
                }
                "Page.addScriptToEvaluateOnNewDocument" => vec![(id, json!({ "identifier": "h" }))],
                // The second page is busy from the release on: it answers nothing more.
                "Runtime.evaluate" if session == "S2" && (expr == cdp::release_expr() || expr.contains("S.zoneRead(")) => {
                    Vec::new()
                }
                "Runtime.evaluate" if expr == cdp::release_expr() => vec![(id, json!({ "result": { "value": "ok" } }))],
                "Runtime.evaluate" if expr.contains("S.zoneRead(") => {
                    vec![(id, json!({ "result": { "value": [-60, -120] } }))]
                }
                _ => vec![(id, json!({}))],
            }
        });
        let ws = crate::cdp::WsClient::connect("127.0.0.1", port, "/", Instant::now() + Duration::from_secs(5)).unwrap();
        let mut attacher = Attacher::over(cdp::CdpClient::from_ws(ws), port);
        attacher.set_budgets(Duration::from_millis(20), Duration::from_secs(2));
        attacher.set_zone(-330);
        let origin = ShimOrigin { fake0: 0, real0: 0, mult: 1, dur: 1, scheduled: None };
        let mut next = 0;
        assert_eq!(attacher.attach_existing(origin, &mut next, Instant::now() + Duration::from_secs(2)).unwrap(), 2);
        let machine = MachineZone { name: Some("Europe/Warsaw"), at_ms: [0, 1], offsets: Some([-60, -120]) };
        let started = Instant::now();
        let released = attacher.release(&cdp::release_expr(), &machine, origin, &mut next, started + END_WAIT);
        assert!(started.elapsed() < END_WAIT + Duration::from_millis(250), "the end's deadline holds: {:?}", started.elapsed());
        assert_eq!(released.unconfirmed, 1, "the busy page did not confirm it was let go");
        assert_eq!(released.zone, ZoneTally { back: 1, kept: 0, unanswered: 1 });
        drop(attacher);
        drop(browser.join());
    }

    /// A page that still shows the session's zone after it was given the machine's - an engine that does
    /// not know the name keeps it there - is counted as kept, and one that never answers the read as
    /// unanswered. A session that set no zone gives none back and reads none.
    #[test]
    fn a_page_still_on_the_sessions_zone_after_the_release_is_counted() {
        let release_with = |answer: Option<serde_json::Value>, zone: Option<i32>| {
            let (port, browser) = crate::cdp::fake_browser_holding(move |request| {
                let id = request["id"].clone();
                let expr = request["params"]["expression"].as_str().unwrap_or("");
                match request["method"].as_str().unwrap_or("") {
                    "Target.getTargets" => vec![(id, json!({ "targetInfos": [{ "targetId": "T1", "type": "page" }] }))],
                    "Target.attachToTarget" => vec![(id, json!({ "sessionId": "S1" }))],
                    "Page.addScriptToEvaluateOnNewDocument" => vec![(id, json!({ "identifier": "h" }))],
                    "Runtime.evaluate" if expr == cdp::release_expr() => vec![(id, json!({ "result": { "value": "ok" } }))],
                    "Runtime.evaluate" if expr.contains("S.zoneRead(") => {
                        answer.clone().map(|v| vec![(id, json!({ "result": { "value": v } }))]).unwrap_or_default()
                    }
                    _ => vec![(id, json!({}))],
                }
            });
            let ws = crate::cdp::WsClient::connect("127.0.0.1", port, "/", Instant::now() + Duration::from_secs(5)).unwrap();
            let mut attacher = Attacher::over(cdp::CdpClient::from_ws(ws), port);
            attacher.set_budgets(Duration::from_millis(20), Duration::from_secs(2));
            if let Some(bias) = zone {
                attacher.set_zone(bias);
            }
            let origin = ShimOrigin { fake0: 0, real0: 0, mult: 1, dur: 1, scheduled: None };
            let mut next = 0;
            assert_eq!(attacher.attach_existing(origin, &mut next, Instant::now() + Duration::from_secs(2)).unwrap(), 1);
            let machine = MachineZone { name: Some("Mars/Olympus"), at_ms: [0, 1], offsets: Some([-60, -120]) };
            let released = attacher.release(&cdp::release_expr(), &machine, origin, &mut next, Instant::now() + Duration::from_millis(750));
            drop(attacher);
            (released, browser.join().unwrap())
        };
        let (kept, _) = release_with(Some(json!([-330, -330])), Some(-330));
        assert_eq!(kept.zone, ZoneTally { back: 0, kept: 1, unanswered: 0 });
        let (silent, _) = release_with(None, Some(-330));
        assert_eq!(silent.zone, ZoneTally { back: 0, kept: 0, unanswered: 1 });
        let (none, log) = release_with(Some(json!([-60, -120])), None);
        assert_eq!(none.zone, ZoneTally::default());
        assert!(log.iter().all(|r| r["method"] != "Emulation.setTimezoneOverride"), "no zone set, none given back");
        assert!(log.iter().all(|r| !r["params"]["expression"].as_str().unwrap_or("").contains("S.zoneRead(")));
    }

    /// R4-S14: a turn takes everything that is already here, not one message - and reports what
    /// mattered most in it, so an attach in the middle of a burst of other events is not lost.
    #[test]
    fn a_pump_turn_takes_the_whole_burst_and_reports_its_strongest_outcome() {
        let mut burst = vec![Pumped::Idle, Pumped::Attached, Pumped::Detached, Pumped::Idle].into_iter();
        let mut taken = 0;
        let turn = drain_turn(Pumped::Idle, || true, || {
            let next = burst.next();
            taken += usize::from(next.is_some());
            next
        });
        assert_eq!(turn, Pumped::Attached);
        assert_eq!(taken, 4, "every message already here was handled in the turn");

        // A closed connection ends the turn at once and is what the turn reports.
        let mut after_close = 0;
        assert_eq!(drain_turn(Pumped::Closed, || true, || { after_close += 1; Some(Pumped::Attached) }), Pumped::Closed);
        assert_eq!(after_close, 0, "nothing is read after the connection closed");

        // A target that never stops talking cannot keep the turn going.
        let mut endless = 0;
        assert_eq!(drain_turn(Pumped::Idle, || true, || { endless += 1; Some(Pumped::Idle) }), Pumped::Idle);
        assert_eq!(endless, PUMP_DRAIN_MAX);
    }

    /// An attach under the caller's deadline ends by it, and one with no deadline of the caller's -
    /// or a later one - keeps its own budget.
    #[test]
    fn an_attach_ends_by_the_callers_deadline_when_that_comes_first() {
        let now = Instant::now();
        let budget = Duration::from_secs(10);
        let soon = now + Duration::from_secs(1);
        assert_eq!(attach_deadline(now, budget, Some(soon)), soon);
        assert_eq!(attach_deadline(now, budget, None), now + budget);
        assert_eq!(attach_deadline(now, budget, Some(now + Duration::from_secs(30))), now + budget);
    }

    /// R4-S10: a turn that has run out of its time stops taking messages, however many are here - a
    /// burst of attaches cannot hold the loop from its heartbeat. The rest waits for the next turn.
    #[test]
    fn a_pump_turn_stops_when_its_time_is_spent() {
        let mut budget = 3;
        let mut taken = 0;
        let turn = drain_turn(
            Pumped::Idle,
            || {
                budget -= 1;
                budget >= 0
            },
            || {
                taken += 1;
                Some(Pumped::Attached)
            },
        );
        assert_eq!(turn, Pumped::Attached);
        assert_eq!(taken, 3, "three messages fitted in the turn, the fourth waits");
    }

    /// The probe decides whether a page inside a natively hooked application gets the shim. Our own
    /// marker means a page that reloaded - it is ours, and it is tracked again. A reading nearer the
    /// session clock than the real one means the native mechanism covers it already, and a shim on
    /// top would scale the fake wall twice - so it is left alone. Anything else is real and gets the
    /// shim: no reply, a reply that is not a number, or a reading nearer the real clock.
    #[test]
    fn the_clock_probe_tells_ours_from_native_from_real() {
        let fake = 1_900_000_000_000;
        let real = 1_700_000_000_000;
        assert_eq!(classify_clock_read(Some("shim"), fake, real), ClockRead::Shim);
        assert_eq!(classify_clock_read(Some("1900000000500"), fake, real), ClockRead::Native);
        assert_eq!(classify_clock_read(Some("1700000000500"), fake, real), ClockRead::Real);
        assert_eq!(classify_clock_read(None, fake, real), ClockRead::Real, "no evidence is no hook");
        assert_eq!(classify_clock_read(Some("undefined"), fake, real), ClockRead::Real);
        // A reading equally far from both only happens with the session clock within milliseconds
        // of the real one, where a second substitution is the identity - so it gets the shim.
        assert_eq!(classify_clock_read(Some("1800000000000"), fake, real), ClockRead::Real);
    }

    /// The reading is the page's own `Date.now`, and a page can make that anything. At the ends of
    /// `i64` the old subtraction overflowed and panicked the core (R4-N23). Both ends, and a session
    /// clock before 1970, so the sign of neither distance is assumed.
    #[test]
    fn a_page_clock_at_the_ends_of_the_range_is_judged_rather_than_a_crash() {
        let fake = 1_900_000_000_000;
        let real = 1_700_000_000_000;
        let min = i64::MIN.to_string();
        let max = i64::MAX.to_string();
        assert_eq!(classify_clock_read(Some(&min), fake, real), ClockRead::Real);
        assert_eq!(classify_clock_read(Some(&max), fake, real), ClockRead::Native);
        assert_eq!(classify_clock_read(Some(&max), -fake, real), ClockRead::Real);
        assert_eq!(classify_clock_read(Some(&min), -fake, real), ClockRead::Native);
    }

    /// The ceiling is on contexts ever seen, and a re-attach of a known context never counts against
    /// it - the one decision the live path cannot exercise without 257 pages.
    #[test]
    fn the_ceiling_refuses_only_a_new_context_and_lets_a_known_one_back_in() {
        let full: Vec<u32> = (1..=MAX_CONTEXTS as u32).collect();
        assert!(past_ceiling(&full, MAX_CONTEXTS as u32 + 1));
        assert!(!past_ceiling(&full, 42));
        assert!(!past_ceiling(&full[..MAX_CONTEXTS - 1], MAX_CONTEXTS as u32 + 1));
        assert!(!past_ceiling(&[], 1));
    }

    /// Existing targets: pages and workers are wanted, a browser target and a target already
    /// indexed are not, and a target without an id is nothing to attach to by name.
    #[test]
    fn existing_targets_are_filtered_to_the_shimmable_unknown_named_ones() {
        let reply = json!({ "targetInfos": [
            { "targetId": "T-page", "type": "page" },
            { "targetId": "T-browser", "type": "browser" },
            { "targetId": "T-known", "type": "page" },
            { "targetId": "", "type": "page" },
            { "targetId": "T-worker", "type": "worker" },
        ]});
        let mut known = HashMap::new();
        known.insert("T-known".to_string(), 7);
        let wanted = new_targets(&reply, &known);
        assert_eq!(wanted, vec![("T-page".to_string(), "page".to_string()), ("T-worker".to_string(), "worker".to_string())]);
        assert!(new_targets(&json!({}), &known).is_empty());
    }
}
