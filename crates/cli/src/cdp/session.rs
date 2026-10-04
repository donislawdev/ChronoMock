//! The JS time shim and its injection into every context of a Chromium target (slice C3). The shim
//! is the CDP mechanism's equivalent of the native hook: it overrides the JS time APIs so the
//! target's own timers run on the session clock. Injection uses auto-attach so it reaches the page
//! AND its Web Workers, where an Electron app's timer often turns out to live.
//!
//! What the shim covers: `setInterval`/`setTimeout` scaling (the acceleration), `Date.now`,
//! `performance.now`, the `Date` constructor and its function form (`new Date()`, `Date()`, and a
//! subclass of `Date`, which stays an instance of itself), and `Intl.DateTimeFormat` formatting "now".
//! The zone is the host's - the instant is faked, not the local-time getters.

use super::CdpClient;
use serde_json::{json, Value};
use std::io;
use std::time::Instant;

/// The time shim, with `__MULT__`/`__DUR__`/`__FAKE_START__`/`__REAL_START__`/`__SCHEDULED__`/
/// `__WALL_MAX__` filled in by [`build_shim`]. A guard (`__chronomock`) keeps the originals wrapped
/// exactly once: a second run in the same document only sets the clock it carries, so of two
/// new-document hooks the one added last decides (R4-W5). `fakeNow` is
/// `fakeStart + (realNow - realStart) * M`, so M = 1 is a pure wall offset and M > 1 accelerates.
///
/// The clock lives in the mutable `__chronomock` object and every override reads it live. It is one
/// segment (`fakeStart`/`realStart`/`M` for the wall, `perfBase`/`perfAnchorReal`/`D` for the
/// duration axis) plus at most one rate change scheduled for an instant (`scheduled`), and the
/// driver moves it only through the object's own methods - the CDP equivalent of the native hook
/// re-reading `Ctl`:
/// - `schedule(at, M, D)` - a rate change in a Chromium session. It takes effect at `at`, the same
///   instant the panel's clock changes at, from wherever this context's own segment stands then, so
///   the wall never steps at the change (R4-S17, ADR-9 R4/14b). A context reached after `at` changes
///   at its own now and says `late`.
/// - `set(fakeStart, realStart, M, D, scheduled)` - the whole clock at once: a jump, the release, a
///   second install, and every move of the pages of an embedded engine, which follow the host's clock
///   as it is.
/// - `settle()` - the scheduled change folded in once its instant has come. Every read calls it
///   first, so a change takes effect at its own instant whoever reads first.
///
/// Every change re-anchors the duration axis at the rate it ran at, so `performance.now` stays
/// continuous and never runs backward when the rate drops (untouchable rule 3). A rate change cannot,
/// however, reschedule a `setInterval` already queued at the old rate - that stays at its old cadence
/// (the driver warns).
///
/// The wall and the duration axis have separate rates. `M` moves the wall (`Date`), `D` moves the
/// timers and `performance.now`. A Chromium session sets both to the multiplier. A page inside a
/// natively hooked application follows that application instead, whose duration axis scales only
/// under `scale_duration` (docs/09 section 12.6) - one rule for one application, whichever half of
/// it a timer runs in.
const SHIM_TEMPLATE: &str = r#"(function(){
  var O = globalThis.__chronomock;
  if (O) {
    /* Installed already: by an older new-document script that ran first while this newer one was
       being registered (R4-W5), or by an earlier evaluate - this session's, or one that let the page go
       before this one reached it, which must not keep it on the clock it left behind. Scripts run in
       the order they were added, so the newest runs last and its whole clock stands, the change still
       to come included. Anything else under that name is not this shim, and the context is not
       covered - said as a failure rather than reported shimmed (untouchable rule 4). */
    if (typeof O.set !== 'function') { throw new Error('__chronomock is held by something else'); }
    O.wallMax = __WALL_MAX__;
    O.set(__FAKE_START__, __REAL_START__, __MULT__, __DUR__, __SCHEDULED__);
    return 'already';
  }
  var _OrigDate = Date;
  var _now = _OrigDate.now.bind(_OrigDate);
  /* Taken now, before the page's own scripts: a native Date() never calls the prototype's toString,
     so one the page replaced must not reach Date() either. */
  var _dateString = _OrigDate.prototype.toString;
  var _perf = (typeof performance !== 'undefined' && performance.now) ? performance.now.bind(performance) : null;
  var S = {
    M: __MULT__,                    /* wall rate: 0 = frozen, 1 = flow (wall offset only), N = accelerate */
    D: __DUR__,                     /* duration rate for timers and performance.now, never below 1 */
    fakeStart: __FAKE_START__,
    realStart: __REAL_START__,
    scheduled: __SCHEDULED__,       /* the one rate change waiting for its instant, { at, M, D } (R4-S17) */
    wallMax: __WALL_MAX__,          /* the last instant the session clock can hold - it stands there */
    perfBase: _perf ? _perf() : 0,  /* where performance.now stood when the shim arrived (R4-S16) */
    perfAnchorReal: _perf ? _perf() : 0,
    perfRead: 0,                    /* the real reading behind the last performance.now handed out */
    _realNow: _now,
    _realPerf: _perf,
    counts: { si: 0, st: 0, now: 0, date: 0, intl: 0, perf: 0 }
  };
  globalThis.__chronomock = S;

  /* What performance.now has reached at the rate it ran at becomes the base at the real reading p, so
     a new rate never integrates the past again (untouchable rule 3). */
  function anchorDuration(p){ S.perfBase += (p - S.perfAnchorReal) * (S.D || 1); S.perfAnchorReal = p; }
  /* The scheduled change takes effect at its instant: the wall goes on from where the segment puts it
     then, and the duration axis from its real reading then - derived through the wall, and never
     below a reading already handed out, so neither steps back whoever reads first. */
  S.settle = function(){
    var c = S.scheduled;
    if (!c) { return; }
    var t = _now();
    if (t < c.at) { return; }
    S.scheduled = null;
    S.fakeStart = Math.min(S.fakeStart + (c.at - S.realStart) * S.M, S.wallMax);
    S.realStart = c.at;
    S.M = c.M;
    if (_perf) { anchorDuration(Math.max(_perf() - (t - c.at), S.perfAnchorReal, S.perfRead)); }
    S.D = c.D;
  };
  /* A rate change scheduled for the instant at, the same for the panel and for every context. One
     still waiting is replaced - the core replaced it too, at that instant. Received after at, the
     change takes effect now: the clock stays continuous, but stands (now - at) times the change in
     rate away from the panel, and the answer says so. */
  S.schedule = function(at, m, d){
    S.settle();
    var t = _now();
    S.scheduled = { at: Math.max(at, t), M: m, D: d };
    S.settle();
    return t > at ? 'late' : 'ok';
  };
  /* The whole clock at once: the duration axis re-anchored at the rate it ran at, then the segment and
     the change still to come, which takes effect at once if its instant has passed. */
  S.set = function(fakeStart, realStart, m, d, scheduled){
    S.settle();
    if (_perf) { anchorDuration(_perf()); }
    S.fakeStart = fakeStart; S.realStart = realStart; S.M = m; S.D = d; S.scheduled = scheduled;
    S.settle();
    return 'ok';
  };
  function durationRate(){ if (S.scheduled) { S.settle(); } return S.D || 1; }
  function fakeNow(){
    if (S.scheduled) { S.settle(); }
    return Math.round(Math.min(S.fakeStart + (_now() - S.realStart) * S.M, S.wallMax));
  }

  /* Replace Date so new Date() (no args), Date() and Date.now() read the session clock; every other
     form (parsing, explicit fields) is unchanged. Reflect.construct with new.target keeps a subclass
     (class X extends Date) an X - building a plain Date here dropped its prototype (R4-W6). */
  function CMDate() {
    if (!new.target) { S.counts.date++; return _dateString.call(new _OrigDate(fakeNow())); }
    if (arguments.length === 0) { S.counts.date++; return Reflect.construct(_OrigDate, [fakeNow()], new.target); }
    return Reflect.construct(_OrigDate, arguments, new.target);
  }
  CMDate.prototype = _OrigDate.prototype;
  CMDate.now = function(){ S.counts.now++; return fakeNow(); };
  CMDate.parse = _OrigDate.parse;
  CMDate.UTC = _OrigDate.UTC;
  /* The prototype is the original's, so its constructor has to point here, or new d.constructor()
     reads the real clock - and the name and arity are the native ones. */
  try { Object.defineProperty(_OrigDate.prototype, 'constructor', { value: CMDate, writable: true, configurable: true }); } catch (e) {}
  try { Object.defineProperty(CMDate, 'name', { value: 'Date' }); Object.defineProperty(CMDate, 'length', { value: 7 }); } catch (e) {}
  try { globalThis.Date = CMDate; } catch (e) { try { Date.now = CMDate.now; } catch (e2) {} }

  /* Intl.DateTimeFormat formats "now" when it is given no date, and reads that now itself - the real
     clock. format is a getter that hands out one bound function per formatter, so the wrapper is kept
     per formatter the same way. */
  var _DTF = globalThis.Intl && globalThis.Intl.DateTimeFormat;
  if (_DTF && _DTF.prototype) {
    var _fmtGet = (Object.getOwnPropertyDescriptor(_DTF.prototype, 'format') || {}).get;
    var _fmtOf = typeof WeakMap === 'function' ? new WeakMap() : null;
    if (_fmtGet && _fmtOf) {
      try {
        Object.defineProperty(_DTF.prototype, 'format', { configurable: true, get: function(){
          var f = _fmtOf.get(this);
          if (!f) {
            var real = _fmtGet.call(this);
            f = function(d){ if (d === undefined) { S.counts.intl++; d = fakeNow(); } return real(d); };
            _fmtOf.set(this, f);
          }
          return f;
        } });
      } catch (e) {}
    }
    var _ftp = _DTF.prototype.formatToParts;
    if (_ftp) {
      _DTF.prototype.formatToParts = function(d){ if (d === undefined) { S.counts.intl++; d = fakeNow(); } return _ftp.call(this, d); };
    }
  }

  /* setInterval/setTimeout read the duration rate live, so a NEW timer picks up the current rate; one
     already scheduled keeps its old cadence (the kernel already queued it). */
  var _si = globalThis.setInterval, _st = globalThis.setTimeout;
  if (_si) { globalThis.setInterval = function(fn, d){ S.counts.si++; var a = [].slice.call(arguments, 2); return _si.apply(globalThis, [fn, (d || 0) / durationRate()].concat(a)); }; }
  if (_st) { globalThis.setTimeout = function(fn, d){ S.counts.st++; var a = [].slice.call(arguments, 2); return _st.apply(globalThis, [fn, (d || 0) / durationRate()].concat(a)); }; }
  if (_perf) {
    performance.now = function(){
      S.counts.perf++;
      var rate = durationRate(), p = _perf();
      S.perfRead = p;
      return S.perfBase + (p - S.perfAnchorReal) * rate;
    };
  }
  return 'installed';
})()"#;

/// Read a context's per-API call counts (or `null` if the shim is not installed there). The counts
/// make an honest "covered means the app actually called it" report, the same way the native audit
/// counts channel queries - an override that was installed but never exercised is not "covered".
pub const COUNTS_EXPR: &str = "(globalThis.__chronomock && globalThis.__chronomock.counts) || null";

/// The APIs the shim counts, as (the name the report gives them, the key in the shim's `counts`).
/// `new Date` counts every read of the clock through the constructor, `new Date()` and `Date()` alike -
/// an app that reads the time only that way was reported as having called no time API at all (R4-S19).
/// `Intl.DateTimeFormat` counts a format of "now" (`format()` or `formatToParts()` with no date).
pub const COUNTED_APIS: [(&str, &str); 6] = [
    ("setInterval", "si"),
    ("setTimeout", "st"),
    ("Date.now", "now"),
    ("new Date", "date"),
    ("Intl.DateTimeFormat", "intl"),
    ("performance.now", "perf"),
];

/// Build the shim source for a session clock: `fake_start_ms`/`real_start_ms` are Unix-epoch ms, `mult`
/// the wall rate (0 freezes it) and `dur` the duration rate for timers and `performance.now` (never
/// below 1 - a frozen wall does not stop a timer, untouchable rule 3). The browser's own `Date.now`
/// supplies "real now" at run time, so all contexts share one clock origin as long as the driver's and
/// the browser's wall clocks agree (same machine).
///
/// `scheduled` is a rate change still waiting for its instant: a document that starts before it takes
/// it at that instant, one that starts after takes it at once, from the instant on (R4-S17).
///
/// `wall_max_ms` is the last instant the wall may show. The page's clock stands there, as the native
/// hook's does, instead of running on past what the session can name (R4-S8). A parameter rather than
/// a constant of this module, because this transport client knows nothing of the session's range.
pub fn build_shim(
    fake_start_ms: i64,
    real_start_ms: i64,
    mult: i64,
    dur: i64,
    scheduled: Option<ScheduledRate>,
    wall_max_ms: i64,
) -> String {
    SHIM_TEMPLATE
        .replace("__MULT__", &mult.to_string())
        .replace("__DUR__", &dur.max(1).to_string())
        .replace("__FAKE_START__", &fake_start_ms.to_string())
        .replace("__REAL_START__", &real_start_ms.to_string())
        .replace("__SCHEDULED__", &scheduled_js(scheduled))
        .replace("__WALL_MAX__", &wall_max_ms.to_string())
}

/// A rate change scheduled for one instant, the same for the panel and every context (R4-S17): from
/// `at_ms` (Unix-epoch ms) the wall runs at `mult` and the duration axis at `dur`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScheduledRate {
    pub at_ms: i64,
    pub mult: i64,
    pub dur: i64,
}

/// A scheduled change as the shim reads it, `null` for none. The duration rate is floored at 1, as in
/// [`build_shim`]: a frozen wall never stops a timer (untouchable rule 3).
pub fn scheduled_js(scheduled: Option<ScheduledRate>) -> String {
    match scheduled {
        Some(s) => format!("{{ at: {}, M: {}, D: {} }}", s.at_ms, s.mult, s.dur.max(1)),
        None => "null".to_string(),
    }
}

/// The expression that puts a live document on a whole clock at once through the shim's `set`: a
/// jump, a resync, or - on the clock `(0, 0, 1, 1)`, the real wall with the duration axis on at rate
/// 1 - the release. `no-shim` from a document the shim is not in. The one place this call is written,
/// for the session's clock moves and for taking back an injection that failed.
pub fn set_expr(fake0: i64, real0: i64, mult: i64, dur: i64, scheduled: Option<ScheduledRate>) -> String {
    format!(
        "(function(){{var S=globalThis.__chronomock;if(!S)return 'no-shim';return S.set({},{},{},{},{});}})()",
        fake0,
        real0,
        mult,
        dur.max(1),
        scheduled_js(scheduled)
    )
}

/// True for a CDP target type that runs the target's own JS (and so is worth shimming). GPU, browser,
/// and other infrastructure targets have no app timer to cover.
pub fn is_shimmable(target_type: &str) -> bool {
    matches!(
        target_type,
        "page" | "iframe" | "webview" | "worker" | "shared_worker" | "service_worker" | "dedicated_worker"
    )
}

/// Whether a CDP target type is a worker (vs a page/frame). Workers get the shim directly - pages also
/// cascade auto-attach so their own workers are reached.
pub fn is_worker(target_type: &str) -> bool {
    target_type.contains("worker")
}

/// Whether a context of this type can start workers of its own - the ones auto-attach set on it is
/// there to reach. A service worker cannot (`Worker` is not in its scope), so one that refuses
/// auto-attach leaves nothing on the real clock.
pub fn starts_workers(target_type: &str) -> bool {
    is_shimmable(target_type) && target_type != "service_worker"
}

/// What installing the shim into one context left behind.
pub struct Injected {
    /// A page's new-document hook, which the caller replaces when the clock moves (R4-W5). `None` for
    /// a worker, and for a page that answered without an identifier.
    pub script: Option<String>,
    /// Whether the context took auto-attach (see [`auto_attach_children`]).
    pub children: bool,
}

/// Install the shim into a page (or frame) session: as an add-script hook so every future document
/// gets it before its own scripts run, plus an immediate evaluate for the document already loaded.
/// Then cascade auto-attach so the page's Web Workers are attached and shimmed too.
///
/// One `deadline` for the whole sequence (R4-S10): each call used to have a deadline of its own, so a
/// page in a busy renderer could hold the session for five of them.
///
/// The enable, the hook and the shim go out together, in that order, before any answer is awaited
/// (R4-15b, 2026-10-04). A page whose document is already loading when it is reached - the first
/// window of an app whose browser answers only once that window is open - runs its startup scripts
/// as soon as its renderer is free, and the shim has to be in the renderer's queue by then. Measured
/// on an Electron app: the renderer answered the enable, and the page's startup scripts ran before a
/// shim sent only after that answer - the page read the real clock at start and showed it for the
/// whole session. All three go to the page's own session, which takes its commands in the order
/// they came. Only the hook and the shim are awaited - the enable's answer changes nothing. The
/// auto-attach and the release stay one after another, after the shim: the auto-attach is the
/// browser's and the release the renderer's, and the order of those two is not documented. The
/// release itself is not waited for: nothing follows it here, and its answer changes nothing.
pub fn inject_page(client: &mut CdpClient, session_id: &str, shim: &str, deadline: Instant) -> io::Result<Injected> {
    client.send("Page.enable", json!({}), Some(session_id))?;
    let hook = client.send("Page.addScriptToEvaluateOnNewDocument", json!({ "source": shim }), Some(session_id))?;
    let shimmed = send_shim(client, session_id, shim)?;
    let script = match client.reply_until(hook, "Page.addScriptToEvaluateOnNewDocument", deadline) {
        Ok(added) => script_identifier(&added),
        Err(e) => {
            take_back(client, session_id, None);
            return Err(e);
        }
    };
    if let Err(e) = shim_reply(client, shimmed, deadline) {
        take_back(client, session_id, script.as_deref());
        return Err(e);
    }
    let children = auto_attach_children(client, session_id, deadline);
    let _ = client.send("Runtime.runIfWaitingForDebugger", json!({}), Some(session_id));
    Ok(Injected { script, children })
}

/// Take back an injection that failed after its first steps went in (CodeRabbit on #85). The caller
/// counts the context as uncovered and lets it go, and nothing moves or releases it after that - but
/// the new-document hook is registered, and a shim evaluate that ran out of time is still queued in
/// the context and runs once it is free. So the hook is removed, and the release is queued after that
/// evaluate, in the same session, so the document ends on the real clock whichever runs. Not waited
/// for: the context may be busy for as long as it likes.
fn take_back(client: &mut CdpClient, session_id: &str, script: Option<&str>) {
    if let Some(script) = script {
        let _ = client.send("Page.removeScriptToEvaluateOnNewDocument", json!({ "identifier": script }), Some(session_id));
    }
    let release = set_expr(0, 0, 1, 1, None);
    let _ = client.send("Runtime.evaluate", json!({ "expression": release, "returnByValue": true }), Some(session_id));
}

/// Install the shim into a worker session, before its script runs when the worker was paused on start
/// (waitForDebuggerOnStart), or immediately for a worker that is already alive but has not yet armed a
/// timer. Then release a paused worker so it proceeds with the overridden globals in place. A worker
/// has no new-document hook - one started later is a new target, shimmed from the clock of then. One
/// deadline for the sequence, as for a page.
pub fn inject_worker(client: &mut CdpClient, session_id: &str, shim: &str, deadline: Instant) -> io::Result<Injected> {
    if let Err(e) = evaluate_shim(client, session_id, shim, deadline) {
        take_back(client, session_id, None);
        return Err(e);
    }
    // A worker can start workers of its own, and auto-attach set on the page does not reach them: a
    // worker started by a worker read the real clock (R4-N25, measured on an Electron page). Set before
    // the worker is released, so one it starts in its first script is paused for the shim too.
    let children = auto_attach_children(client, session_id, deadline);
    let _ = client.send("Runtime.runIfWaitingForDebugger", json!({}), Some(session_id));
    Ok(Injected { script: None, children })
}

/// Ask a context to attach the workers it starts, paused for the shim. `false` when it answered with
/// an error or not at all: the context itself is shimmed, but a worker it starts would run on the
/// real clock unseen, and the caller has to count that (rule 4). Measured on Chromium 153: a page, a
/// dedicated worker, a nested one, a shared worker and a service worker all take it.
fn auto_attach_children(client: &mut CdpClient, session_id: &str, deadline: Instant) -> bool {
    client
        .call_until(
            "Target.setAutoAttach",
            json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true }),
            Some(session_id),
            deadline,
        )
        .is_ok()
}

/// The identifier `Page.addScriptToEvaluateOnNewDocument` answered with, if it gave one. The caller
/// keeps it, because a hook can only be removed by it.
pub fn script_identifier(reply: &Value) -> Option<String> {
    reply.get("identifier").and_then(Value::as_str).map(str::to_string)
}

/// Evaluate the shim in a session's global context and surface a thrown exception as an error (the
/// shim must never fail silently - an uncovered context is an honest non-effect, not a hidden one).
fn evaluate_shim(client: &mut CdpClient, session_id: &str, shim: &str, deadline: Instant) -> io::Result<()> {
    let shimmed = send_shim(client, session_id, shim)?;
    shim_reply(client, shimmed, deadline)
}

/// Send the shim to the context's current document, without waiting.
fn send_shim(client: &mut CdpClient, session_id: &str, shim: &str) -> io::Result<u64> {
    client.send("Runtime.evaluate", json!({ "expression": shim, "returnByValue": true }), Some(session_id))
}

/// Wait by `deadline` for the shim's reply, an error when it threw.
fn shim_reply(client: &mut CdpClient, shimmed: u64, deadline: Instant) -> io::Result<()> {
    let r = client.reply_until(shimmed, "Runtime.evaluate", deadline)?;
    if let Some(exc) = r.get("exceptionDetails") {
        return Err(shim_error(exc));
    }
    Ok(())
}

/// The exception a target's JS engine reported for the shim, as an `io::Error`. Split out for the
/// reason `target_error` was: the text is written by the TARGET, so the one place that quotes it has
/// a name and a test.
///
/// Folded HERE, at the source, rather than at the one place that prints it today - the session loop
/// currently discards this error, and the obvious improvement (saying WHY a context could not be
/// shimmed) would carry the raw text into the report, which is evidence (untouchable rule 4).
fn shim_error(exception_details: &Value) -> io::Error {
    let text = exception_details
        .get("text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("shim threw");
    io::Error::other(format!("shim evaluate failed: {}", super::sanitise_target_text(text)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shim runs inside the target's own JS engine, so the exception text it throws is the
    /// target's words. A newline there would add a line to output the tool did not write - the same
    /// property `target_error` protects for CDP errors, and the reason evidence stays trustworthy.
    #[test]
    fn a_shim_exception_cannot_forge_a_line() {
        let thrown = json!({ "text": "Uncaught\nchrono core: verdict: works" });
        let text = shim_error(&thrown).to_string();
        assert!(!text.contains('\n'), "target words reached the error raw: {text}");
        assert!(text.contains("Uncaught"), "the target's own words are still readable: {text}");

        // No `text` field at all is the ordinary shape of a malformed report, not a panic.
        assert!(shim_error(&json!({})).to_string().contains("shim threw"));
    }

    #[test]
    fn shim_substitutes_its_parameters() {
        let s = build_shim(1_700_000_000_000, 1_600_000_000_000, 60, 60, None, 900_000_000_000_000);
        assert!(s.contains("M: 60,"));
        assert!(s.contains("D: 60,"));
        assert!(s.contains("fakeStart: 1700000000000,"));
        assert!(s.contains("realStart: 1600000000000,"));
        assert!(s.contains("scheduled: null,"));
        assert!(s.contains("wallMax: 900000000000000,"));
        assert!(!s.contains("__MULT__"));
        assert!(!s.contains("__DUR__"));
        assert!(!s.contains("__FAKE_START__"));
        assert!(!s.contains("__SCHEDULED__"));
        assert!(!s.contains("__WALL_MAX__"));
        // The wall is read through the end of the range, never past it (R4-S8).
        assert!(s.contains("Math.min(S.fakeStart + (_now() - S.realStart) * S.M, S.wallMax)"), "{s}");

        let next = ScheduledRate { at_ms: 1_600_000_000_250, mult: 1, dur: 1 };
        let s = build_shim(1_700_000_000_000, 1_600_000_000_000, 60, 60, Some(next), 900_000_000_000_000);
        assert!(s.contains("scheduled: { at: 1600000000250, M: 1, D: 1 },"), "{s}");
    }

    /// A scheduled change reaches the shim as the object it reads, `null` for none, and its duration
    /// rate is floored at 1 like the segment's: a change to freeze keeps the timers at real speed.
    #[test]
    fn a_scheduled_change_is_written_as_the_shim_reads_it() {
        assert_eq!(scheduled_js(None), "null");
        let freeze = ScheduledRate { at_ms: 7, mult: 0, dur: 0 };
        assert_eq!(scheduled_js(Some(freeze)), "{ at: 7, M: 0, D: 1 }");
    }

    /// The two rates are independent: a page inside a natively hooked application keeps its timers
    /// real while its wall runs fast (docs/09 section 12.6), and a frozen wall never stops a timer
    /// (untouchable rule 3), so the duration rate is floored at 1 whatever the caller passes.
    #[test]
    fn the_wall_rate_and_the_duration_rate_are_filled_in_separately() {
        let s = build_shim(0, 0, 60, 1, None, 0);
        assert!(s.contains("M: 60,"), "{s}");
        assert!(s.contains("D: 1,"), "{s}");
        assert!(s.contains("function durationRate(){ if (S.scheduled) { S.settle(); } return S.D || 1; }"));
        assert_eq!(s.matches("(d || 0) / durationRate()").count(), 2, "both timers read the duration rate");
        assert!(!s.contains("/ (S.M || 1)"), "no timer divides by the wall rate any more");

        let frozen = build_shim(0, 0, 0, 0, None, 0);
        assert!(frozen.contains("M: 0,"), "{frozen}");
        assert!(frozen.contains("D: 1,"), "a frozen wall keeps timers at real speed: {frozen}");
    }

    /// R4-W5: a second run of the shim in one document - the newer of two new-document hooks, which
    /// runs last - sets the whole clock it carries, the scheduled change included, instead of
    /// returning untouched. Anything else holding the name is a failure, not a page reported shimmed.
    /// The behaviour is measured in Node (tools/probes/r4-14/w5-test.mjs) and on a live page - this
    /// pins the source.
    #[test]
    fn a_second_run_sets_the_clock_it_carries() {
        let next = ScheduledRate { at_ms: 2_500, mult: 60, dur: 60 };
        let s = build_shim(1_000, 2_000, 1, 1, Some(next), 3_000);
        let guard = &s[..s.find("return 'already';").expect("the guard returns early")];
        assert!(guard.contains("O.wallMax = 3000;"), "{guard}");
        assert!(guard.contains("O.set(1000, 2000, 1, 1, { at: 2500, M: 60, D: 60 });"), "{guard}");
        assert!(guard.contains("if (typeof O.set !== 'function') { throw "), "{guard}");
    }

    /// R4-S17 in the source the pages get: every move goes through the shim's own methods, every read
    /// settles the scheduled change first, and a change takes effect at its instant from the
    /// segment, with the duration axis re-anchored at the old rate before the new one is written. The
    /// behaviour is measured in Node (tools/probes/r4-14/s17-test.mjs) and on a live page - this pins
    /// the source.
    #[test]
    fn a_scheduled_change_takes_effect_at_its_instant_whoever_reads_first() {
        let s = build_shim(0, 0, 60, 60, None, 0);
        let settle = &s[s.find("S.settle = function(){").expect("settle")..s.find("S.schedule =").expect("schedule")];
        assert!(settle.contains("if (t < c.at) { return; }"), "{settle}");
        assert!(settle.contains("S.fakeStart = Math.min(S.fakeStart + (c.at - S.realStart) * S.M, S.wallMax);"));
        assert!(settle.contains("anchorDuration(Math.max(_perf() - (t - c.at), S.perfAnchorReal, S.perfRead))"));
        assert!(settle.find("anchorDuration(").unwrap() < settle.find("S.D = c.D;").unwrap(), "the old rate first");

        let schedule = &s[s.find("S.schedule =").unwrap()..s.find("S.set =").expect("set")];
        assert!(schedule.contains("S.scheduled = { at: Math.max(at, t), M: m, D: d };"), "{schedule}");
        assert!(schedule.contains("return t > at ? 'late' : 'ok';"), "{schedule}");

        let set = &s[s.find("S.set =").unwrap()..s.find("function durationRate").unwrap()];
        assert!(set.find("anchorDuration(_perf())").unwrap() < set.find("S.D = d;").unwrap(), "{set}");

        assert!(s.contains("function fakeNow(){\n    if (S.scheduled) { S.settle(); }"), "the wall settles first");
        assert!(s.contains("var rate = durationRate(), p = _perf();"), "performance.now settles first");
    }

    /// Every API the report names has its counter in the shim, and the shim counts nothing the report
    /// would drop - a key on one side only is a count that is never read or a row that is always zero.
    #[test]
    fn every_counted_api_has_its_counter_in_the_shim() {
        let start = SHIM_TEMPLATE.find("counts: {").expect("the shim declares its counters");
        let end = start + SHIM_TEMPLATE[start..].find('}').expect("the counters close");
        let declared: Vec<&str> = SHIM_TEMPLATE[start + "counts: {".len()..end]
            .split(',')
            .filter_map(|kv| kv.split(':').next().map(str::trim))
            .filter(|k| !k.is_empty())
            .collect();
        let reported: Vec<&str> = COUNTED_APIS.iter().map(|(_, key)| *key).collect();
        assert_eq!(declared, reported, "the shim's counters and the report's rows must match, in order");
    }

    /// R4-W6 and R4-S16 in the source the pages get. A subclass of Date is built with its own
    /// constructor, `Date()` reads the session clock, the prototype points back at the replacement,
    /// "now" formatted by Intl is the session's, and performance.now starts where it stood. The
    /// behaviour is measured in Node and on a live page (tools/probes/r4-14) - this pins the source.
    #[test]
    fn the_date_replacement_keeps_subclasses_and_the_clock_it_reports() {
        let s = build_shim(0, 0, 60, 60, None, 0);
        assert!(s.contains("Reflect.construct(_OrigDate, arguments, new.target)"), "a subclass keeps its prototype");
        assert!(s.contains("if (!new.target) { S.counts.date++; return _dateString.call(new _OrigDate(fakeNow())); }"));
        assert!(s.contains("Object.defineProperty(_OrigDate.prototype, 'constructor', { value: CMDate"));
        assert!(s.contains("if (d === undefined) { S.counts.intl++; d = fakeNow(); }"), "Intl formats the session's now");
        assert!(s.contains("perfBase: _perf ? _perf() : 0,"), "performance.now does not restart at 0");
    }

    /// A context that refused auto-attach is counted only when it can start workers of its own: a
    /// page or a dedicated or shared worker can, a service worker cannot, and a type that is never
    /// shimmed is never counted.
    #[test]
    fn only_a_context_that_can_start_workers_loses_them_by_refusing_auto_attach() {
        for ty in ["page", "iframe", "webview", "worker", "dedicated_worker", "shared_worker"] {
            assert!(starts_workers(ty), "{ty}");
        }
        assert!(!starts_workers("service_worker"));
        assert!(!starts_workers("browser"));
    }

    #[test]
    fn classifies_target_types() {
        assert!(is_shimmable("page"));
        assert!(is_shimmable("worker"));
        assert!(is_shimmable("service_worker"));
        assert!(!is_shimmable("browser"));
        assert!(!is_shimmable("other"));
        assert!(is_worker("dedicated_worker"));
        assert!(!is_worker("page"));
    }

    /// CodeRabbit on #85: an injection whose shim evaluate runs out of time takes itself back before
    /// the context is let go. The hook it added is removed, and the release is queued behind the
    /// evaluate that may still run late, in the same session - otherwise the page went on to the
    /// session clock with nothing left to move or release it. A worker has no hook, only the release,
    /// and so has a page whose hook never answered - its shim went out with the hook (R4-15b).
    #[test]
    fn an_injection_that_runs_out_of_time_takes_its_hook_back_and_queues_the_release() {
        use std::time::Duration;
        let (port, browser) = super::super::ws::tests::fake_browser(|request| match request["method"].as_str()? {
            "Page.addScriptToEvaluateOnNewDocument" if request["sessionId"] == "Q" => None,
            "Page.addScriptToEvaluateOnNewDocument" => Some(json!({ "identifier": "h1" })),
            // A busy context: no evaluate is ever answered.
            "Runtime.evaluate" => None,
            _ => Some(json!({})),
        });
        let ws = super::super::WsClient::connect("127.0.0.1", port, "/", Instant::now() + Duration::from_secs(5)).unwrap();
        let mut client = CdpClient::from_ws(ws);
        let soon = || Instant::now() + Duration::from_millis(300);
        assert!(inject_page(&mut client, "P", "SHIM", soon()).is_err());
        assert!(inject_page(&mut client, "Q", "SHIM", soon()).is_err());
        assert!(inject_worker(&mut client, "W", "SHIM", soon()).is_err());
        drop(client);
        let log = browser.join().unwrap();
        let steps = |session: &str| -> Vec<String> {
            log.iter()
                .filter(|r| r["sessionId"] == session)
                .map(|r| {
                    let what = r["params"]["identifier"].as_str().or(r["params"]["expression"].as_str()).unwrap_or("");
                    let what = if what.contains("S.set(0,0,1,1,null)") { "release" } else { what };
                    format!("{} {what}", r["method"].as_str().unwrap_or(""))
                })
                .collect()
        };
        assert_eq!(
            steps("P"),
            [
                "Page.enable ",
                "Page.addScriptToEvaluateOnNewDocument ",
                "Runtime.evaluate SHIM",
                "Page.removeScriptToEvaluateOnNewDocument h1",
                "Runtime.evaluate release",
            ]
        );
        assert_eq!(
            steps("Q"),
            [
                "Page.enable ",
                "Page.addScriptToEvaluateOnNewDocument ",
                "Runtime.evaluate SHIM",
                "Runtime.evaluate release",
            ]
        );
        assert_eq!(steps("W"), ["Runtime.evaluate SHIM", "Runtime.evaluate release"]);
    }

    /// R4-15b (2026-10-04): a page reached while its document is loading runs its startup scripts as
    /// soon as its renderer is free, so the shim goes out with the enable and the hook, not after
    /// their answers. This browser answers neither until the shim has come, like a renderer busy
    /// with the document - a client that waited for either first would wait out its deadline - and
    /// then answers the shim first: a reply that came while another was awaited is not lost.
    #[test]
    fn the_shim_goes_out_with_the_enable_and_the_hook_before_any_answer() {
        use std::time::Duration;
        let mut held = Vec::new();
        let (port, browser) = super::super::ws::tests::fake_browser_holding(move |request| {
            let id = request["id"].clone();
            match request["method"].as_str().unwrap_or("") {
                "Page.enable" => {
                    held.push((id, json!({})));
                    Vec::new()
                }
                "Page.addScriptToEvaluateOnNewDocument" => {
                    held.push((id, json!({ "identifier": "h1" })));
                    Vec::new()
                }
                "Runtime.evaluate" => {
                    let mut now = vec![(id, json!({ "result": {} }))];
                    now.append(&mut held);
                    now
                }
                _ => vec![(id, json!({}))],
            }
        });
        let ws = super::super::WsClient::connect("127.0.0.1", port, "/", Instant::now() + Duration::from_secs(5)).unwrap();
        let mut client = CdpClient::from_ws(ws);
        let injected = inject_page(&mut client, "P", "SHIM", Instant::now() + Duration::from_secs(2)).expect("the page took the shim");
        assert_eq!(injected.script.as_deref(), Some("h1"), "the hook answered after the shim is still the page's hook");
        assert!(injected.children);
        drop(client);
        let methods: Vec<String> =
            browser.join().unwrap().iter().map(|r| r["method"].as_str().unwrap_or("").to_string()).collect();
        assert_eq!(
            methods,
            [
                "Page.enable",
                "Page.addScriptToEvaluateOnNewDocument",
                "Runtime.evaluate",
                "Target.setAutoAttach",
                "Runtime.runIfWaitingForDebugger",
            ]
        );
    }
}
