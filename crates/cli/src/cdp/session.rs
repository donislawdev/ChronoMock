//! The JS time shim and its injection into every context of a Chromium target (slice C3). The shim
//! is the CDP mechanism's equivalent of the native hook: it overrides the JS time APIs so the
//! target's own timers run on the session clock. Injection uses auto-attach so it reaches the page
//! AND its Web Workers (where an Electron app's timer often lives - Pomotroid's does).
//!
//! Scope of the shim in this slice: `setInterval`/`setTimeout` scaling (the acceleration) and
//! `Date.now`/`performance.now` on the session clock, with the fake start defaulting to real "now"
//! (pure acceleration). The absolute fake wall moment, the `new Date()` constructor, zone, and frozen
//! mode are the time-model fidelity of slice C5.

use super::CdpClient;
use serde_json::json;
use std::io;

/// The time shim, with `__MULT__`/`__FAKE_START__`/`__REAL_START__` filled in by [`build_shim`].
/// A guard (`__chronomock`) makes re-injection (a page reload re-runs the add-script hook) a no-op,
/// so the originals are wrapped exactly once. `fakeNow` is `fakeStart + (realNow - realStart) * M`,
/// so M = 1 is a pure wall offset and M > 1 accelerates.
///
/// The clock (`M`/`fakeStart`/`realStart` and the duration anchor) lives in the mutable `__chronomock`
/// object, and every override reads it live, so the driver can change the rate or jump the wall in
/// flight by writing new values (slice C7) - the CDP equivalent of the native hook re-reading `Ctl`.
/// A rate change re-anchors the duration axis (`perfBase`/`perfAnchorReal`) so `performance.now` stays
/// continuous and never runs backward when M drops (untouchable rule 3). It cannot, however, reschedule
/// a `setInterval` already queued at the old rate - that stays at its old cadence (the driver warns).
const SHIM_TEMPLATE: &str = r#"(function(){
  if (globalThis.__chronomock) { return 'already'; }
  var _OrigDate = Date;
  var _now = _OrigDate.now.bind(_OrigDate);
  var _perf = (typeof performance !== 'undefined' && performance.now) ? performance.now.bind(performance) : null;
  var S = {
    M: __MULT__,                    /* 0 = frozen, 1 = flow (wall offset only), N = accelerate */
    fakeStart: __FAKE_START__,
    realStart: __REAL_START__,
    perfBase: 0,                    /* accumulated scaled duration up to the last rate change */
    perfAnchorReal: _perf ? _perf() : 0,
    _realNow: _now,
    _realPerf: _perf,
    counts: { si: 0, st: 0, now: 0, perf: 0 }
  };
  globalThis.__chronomock = S;
  function fakeNow(){ return Math.round(S.fakeStart + (_now() - S.realStart) * S.M); }

  /* Replace Date so new Date() (no args) and Date.now() read the session clock; every other form
     (parsing, explicit fields) is unchanged, and instanceof / the prototype are preserved. */
  function CMDate() {
    if (!(this instanceof CMDate)) { return _OrigDate.apply(null, arguments); }
    if (arguments.length === 0) { return new _OrigDate(fakeNow()); }
    return new (Function.prototype.bind.apply(_OrigDate, [null].concat([].slice.call(arguments))))();
  }
  CMDate.prototype = _OrigDate.prototype;
  CMDate.now = function(){ S.counts.now++; return fakeNow(); };
  CMDate.parse = _OrigDate.parse;
  CMDate.UTC = _OrigDate.UTC;
  try { globalThis.Date = CMDate; } catch (e) { try { Date.now = CMDate.now; } catch (e2) {} }

  /* setInterval/setTimeout read the duration scale (M || 1) live, so a NEW timer picks up the current
     rate; one already scheduled keeps its old cadence (the kernel already queued it). */
  var _si = globalThis.setInterval, _st = globalThis.setTimeout;
  if (_si) { globalThis.setInterval = function(fn, d){ S.counts.si++; var a = [].slice.call(arguments, 2); return _si.apply(globalThis, [fn, (d || 0) / (S.M || 1)].concat(a)); }; }
  if (_st) { globalThis.setTimeout = function(fn, d){ S.counts.st++; var a = [].slice.call(arguments, 2); return _st.apply(globalThis, [fn, (d || 0) / (S.M || 1)].concat(a)); }; }
  if (_perf) {
    performance.now = function(){ S.counts.perf++; return S.perfBase + (_perf() - S.perfAnchorReal) * (S.M || 1); };
  }
  return 'installed';
})()"#;

/// Read a context's per-API call counts (or `null` if the shim is not installed there). The counts
/// make an honest "covered means the app actually called it" report, the same way the native audit
/// counts channel queries - an override that was installed but never exercised is not "covered".
pub const COUNTS_EXPR: &str = "(globalThis.__chronomock && globalThis.__chronomock.counts) || null";

/// Build the shim source for a session clock: `fake_start_ms`/`real_start_ms` are Unix-epoch ms, `mult`
/// the speed-up (>= 1). The browser's own `Date.now` supplies "real now" at run time, so all contexts
/// share one clock origin as long as the driver's and the browser's wall clocks agree (same machine).
pub fn build_shim(fake_start_ms: i64, real_start_ms: i64, mult: i64) -> String {
    SHIM_TEMPLATE
        .replace("__MULT__", &mult.to_string())
        .replace("__FAKE_START__", &fake_start_ms.to_string())
        .replace("__REAL_START__", &real_start_ms.to_string())
}

/// True for a CDP target type that runs the target's own JS (and so is worth shimming). GPU, browser,
/// and other infrastructure targets have no app timer to cover.
pub fn is_shimmable(target_type: &str) -> bool {
    matches!(
        target_type,
        "page" | "iframe" | "webview" | "worker" | "shared_worker" | "service_worker" | "dedicated_worker"
    )
}

/// Whether a CDP target type is a worker (vs a page/frame). Workers get the shim directly; pages also
/// cascade auto-attach so their own workers are reached.
pub fn is_worker(target_type: &str) -> bool {
    target_type.contains("worker")
}

/// Install the shim into a page (or frame) session: as an add-script hook so every future document
/// gets it before its own scripts run, plus an immediate evaluate for the document already loaded.
/// Then cascade auto-attach so the page's Web Workers are attached and shimmed too.
pub fn inject_page(client: &mut CdpClient, session_id: &str, shim: &str) -> io::Result<()> {
    client.call("Page.enable", json!({}), Some(session_id)).ok();
    client.call(
        "Page.addScriptToEvaluateOnNewDocument",
        json!({ "source": shim }),
        Some(session_id),
    )?;
    evaluate_shim(client, session_id, shim)?;
    client
        .call(
            "Target.setAutoAttach",
            json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true }),
            Some(session_id),
        )
        .ok();
    client.call("Runtime.runIfWaitingForDebugger", json!({}), Some(session_id)).ok();
    Ok(())
}

/// Install the shim into a worker session, before its script runs when the worker was paused on start
/// (waitForDebuggerOnStart), or immediately for a worker that is already alive but has not yet armed a
/// timer. Then release a paused worker so it proceeds with the overridden globals in place.
pub fn inject_worker(client: &mut CdpClient, session_id: &str, shim: &str) -> io::Result<()> {
    evaluate_shim(client, session_id, shim)?;
    client.call("Runtime.runIfWaitingForDebugger", json!({}), Some(session_id)).ok();
    Ok(())
}

/// Evaluate the shim in a session's global context and surface a thrown exception as an error (the
/// shim must never fail silently - an uncovered context is an honest non-effect, not a hidden one).
fn evaluate_shim(client: &mut CdpClient, session_id: &str, shim: &str) -> io::Result<()> {
    let r = client.call(
        "Runtime.evaluate",
        json!({ "expression": shim, "returnByValue": true }),
        Some(session_id),
    )?;
    if let Some(exc) = r.get("exceptionDetails") {
        let text = exc.get("text").and_then(serde_json::Value::as_str).unwrap_or("shim threw");
        return Err(io::Error::other(format!("shim evaluate failed: {text}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shim_substitutes_its_parameters() {
        let s = build_shim(1_700_000_000_000, 1_600_000_000_000, 60);
        assert!(s.contains("M: 60,"));
        assert!(s.contains("fakeStart: 1700000000000,"));
        assert!(s.contains("realStart: 1600000000000,"));
        assert!(!s.contains("__MULT__"));
        assert!(!s.contains("__FAKE_START__"));
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
}
