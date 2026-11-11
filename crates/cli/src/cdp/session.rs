//! The JS time shim and its injection into every context of a Chromium target (slice C3). The shim
//! is the CDP mechanism's equivalent of the native hook: it overrides the JS time APIs so the
//! target's own timers run on the session clock. Injection uses auto-attach so it reaches the page
//! AND its Web Workers, where an Electron app's timer often turns out to live.
//!
//! What the shim covers: `setInterval`/`setTimeout` scaling (the acceleration), `Date.now`,
//! `performance.now`, the `Date` constructor and its function form (`new Date()`, `Date()`, and a
//! subclass of `Date`, which stays an instance of itself), and `Intl.DateTimeFormat` formatting "now".
//! The zone is not the shim's: the engine's own zone override (`Emulation.setTimezoneOverride`) puts
//! every context on the session's fixed offset, and the shim only checks that its context reads it.

use super::CdpClient;
use serde_json::{json, Value};
use std::io;
use std::time::Instant;

/// The time shim, with `__MULT__`/`__DUR__`/`__FAKE_START__`/`__REAL_START__`/`__SCHEDULED__`/
/// `__WALL_MAX__`/`__ZONE__` filled in by [`build_shim`]. A guard (`__chronomock`) keeps the originals
/// wrapped exactly once: a second run in the same document only sets the clock it carries, so of two
/// new-document hooks the one added last decides (R4-W5). `fakeNow` is
/// `fakeStart + (realNow - realStart) * M`, so M = 1 is a pure wall offset and M > 1 accelerates.
///
/// The clock is closed over, and `__chronomock` is a frozen object of methods on a property that is
/// neither enumerable, writable nor configurable (R5-B8): a page's own scripts do not see it among their
/// globals, and cannot move the clock but through the methods the session uses - the CDP equivalent of
/// the native hook re-reading `Ctl`. The clock is one segment (`fakeStart`/`realStart`/`M` for the wall,
/// `perfBase`/`perfAnchorReal`/`D` for the duration axis) plus at most one rate change scheduled for an
/// instant (`scheduled`):
/// - `schedule(at, M, D)` - a rate change in a Chromium session. It takes effect at `at`, the same
///   instant the panel's clock changes at, from wherever this context's own segment stands then, so
///   the wall never steps at the change (R4-S17, ADR-9 R4/14b). A context reached after `at` changes
///   at its own now and says `late`.
/// - `set(fakeStart, realStart, M, D, scheduled, wallMax, Z)` - the whole clock at once: a jump, a
///   second install, and every move of the pages of an embedded engine, which follow the host's clock
///   as it is. The last two are left as they are when not given.
/// - `release()` - the real clock, and the audit started over.
/// - `audit()` - the counts and the facts the session reads once a second.
/// - `zoneRead(a, b)` - the offset the engine's own `Date` shows at two instants, after the release.
///
/// Every read settles the scheduled change first, so it takes effect at its own instant whoever reads
/// first. Every change re-anchors the duration axis at the rate it ran at, so `performance.now` stays
/// continuous and never runs backward when the rate drops (untouchable rule 3). A repeating timer takes
/// a new rate from its next tick (R5-P6). A one-time timer already waiting finishes its wait at the rate
/// it was armed with - the engine holds it (the driver warns).
///
/// The session's commands reach a page's own document, and a frame of the page is another document: the
/// moves, the audit and the release are handed down from here to every frame the page's JS can reach,
/// which carries a shim of its own from the page's new-document hook (R5-P5, measured on Electron 44).
/// The audit says how many it reached, and the session counts the ones it did not against the page's
/// frame tree.
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
       covered - said as a failure rather than reported shimmed (untouchable rule 4). A shim of an older
       build keeps its limits in open fields, which its set does not take. */
    if (typeof O.set !== 'function') { throw new Error('__chronomock is held by something else'); }
    if (typeof O.release !== 'function') { O.wallMax = __WALL_MAX__; O.Z = __ZONE__; }
    O.set(__FAKE_START__, __REAL_START__, __MULT__, __DUR__, __SCHEDULED__, __WALL_MAX__, __ZONE__);
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
    Z: __ZONE__,                    /* getTimezoneOffset() of the session's zone, null when it sets none */
    zoneMissed: 0,                  /* 1 once this context has read another zone */
    hiddenFast: 0,                  /* 1 once this page was hidden while its timers ran faster */
    capped: 0,                      /* 1 once a timer had to run more often than the engine's 4 ms floor */
    stamps: 0,                      /* 1 once the page read a time the engine stamps while timers ran faster */
    animated: 0,                    /* 1 once an animation played while timers ran faster */
    perfBase: _perf ? _perf() : 0,  /* where performance.now stood when the shim arrived (R4-S16) */
    perfAnchorReal: _perf ? _perf() : 0,
    perfRead: 0,                    /* the real reading behind the last performance.now handed out */
    counts: { si: 0, st: 0, now: 0, date: 0, intl: 0, perf: 0 }
  };

  /* What performance.now has reached at the rate it ran at becomes the base at the real reading p, so
     a new rate never integrates the past again (untouchable rule 3). */
  function anchorDuration(p){ S.perfBase += (p - S.perfAnchorReal) * (S.D || 1); S.perfAnchorReal = p; }
  /* The scheduled change takes effect at its instant: the wall goes on from where the segment puts it
     then, and the duration axis from its real reading then - derived through the wall, and never
     below a reading already handed out, so neither steps back whoever reads first. */
  function settle(){
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
  }
  /* A rate change scheduled for the instant at, the same for the panel and for every context. One
     still waiting is replaced - the core replaced it too, at that instant. Received after at, the
     change takes effect now: the clock stays continuous, but stands (now - at) times the change in
     rate away from the panel, and the answer says so. */
  function scheduleHere(at, m, d){
    settle();
    var t = _now();
    S.scheduled = { at: Math.max(at, t), M: m, D: d };
    settle();
    return t > at ? 'late' : 'ok';
  }
  /* The whole clock at once: the duration axis re-anchored at the rate it ran at, then the segment and
     the change still to come, which takes effect at once if its instant has passed. The scheduled
     change is copied, because a frame's shim is handed the page's own object. */
  function setHere(fakeStart, realStart, m, d, scheduled, wallMax, zone){
    settle();
    if (_perf) { anchorDuration(_perf()); }
    if (wallMax !== undefined) { S.wallMax = wallMax; }
    if (zone !== undefined) { S.Z = zone; }
    S.fakeStart = fakeStart; S.realStart = realStart; S.M = m; S.D = d;
    S.scheduled = scheduled ? { at: scheduled.at, M: scheduled.M, D: scheduled.D } : null;
    settle();
  }
  function durationRate(){ if (S.scheduled) { settle(); } return S.D || 1; }
  function fakeNow(){
    if (S.scheduled) { settle(); }
    return Math.round(Math.min(S.fakeStart + (_now() - S.realStart) * S.M, S.wallMax));
  }
  function perfNow(){
    var rate = durationRate(), p = _perf();
    S.perfRead = p;
    return S.perfBase + (p - S.perfAnchorReal) * rate;
  }

  /* window.length is [Replaceable]: a page's script can assign it, and a number the page chose must not
     decide which frames are looked at, or how many times a second (CodeRabbit on #111). The engine's own
     getter is taken now, before the page's scripts, and the count is held under a ceiling - a frame past
     it is one the session counts as not reached, from the frame tree. */
  var _frameCount = (function(){
    for (var o = globalThis, k = 0; o && k < 4; o = Object.getPrototypeOf(o), k++) {
      try {
        var d = Object.getOwnPropertyDescriptor(o, 'length');
        if (d) { return typeof d.get === 'function' ? d.get : null; }
      } catch (e) { return null; }
    }
    return null;
  })();
  var FRAMES_MAX = 1024;
  /* The frames of this document the page's JS reaches - its own origin, with a shim of this build - get
     what this document gets (R5-P5). The session's commands reach the page's own document only. A frame
     of another origin throws on the first look and is left to the session, which counts it against the
     page's frame tree, and nothing is looked for below it. Returns how many took fn. */
  function eachFrame(fn){
    var w = (typeof window !== 'undefined' && window === globalThis) ? window : null, n = 0, reached = 0;
    if (!w) { return 0; }
    try { n = _frameCount ? _frameCount.call(w) : w.length; } catch (e) { return 0; }
    n = Math.min(Number(n) >>> 0, FRAMES_MAX);
    for (var i = 0; i < n; i++) {
      var o = null;
      try { o = w[i].__chronomock; } catch (e) { o = null; }
      if (o && o !== api && typeof o.audit === 'function' && typeof o.release === 'function') {
        try { fn(o); reached++; } catch (e) {}
      }
    }
    return reached;
  }

  /* Whether this context reads the session's zone. The override belongs to the renderer PROCESS and
     has one holder there: a second page in the process reads it without holding it, and goes back to
     the host's zone without a word when the holder closes (measured). So the answer to the override
     proves nothing, and the reading is checked here, at install and on every audit. */
  function checkZone(){
    if (S.Z === null) { return; }
    try { if (new _OrigDate(fakeNow()).getTimezoneOffset() !== S.Z) { S.zoneMissed = 1; } } catch (e) {}
  }
  /* Whether this page is hidden while its timers run faster: a Chromium engine slows the timers of a
     hidden window to about once a second, which takes the speed-up away from them (R4-N28, measured).
     Looked at on install, when the page's visibility changes, and on every audit, so a rate raised
     while hidden is seen within a second. A worker has no document. */
  function checkHidden(){
    try { if (typeof document !== 'undefined' && document.hidden && durationRate() > 1) { S.hiddenFast = 1; } } catch (e) {}
  }
  /* The times the engine stamps itself - the argument of an animation frame callback, an event's
     timeStamp, the document timeline - are on its own high-resolution clock, at real speed. While the
     timers run faster they disagree with performance.now (R5-F9). Not moved yet (R5/5d), but noticed for
     the audit, so the report says so (R5-D58, R5-D60). Once noticed, a read costs one comparison. */
  function stamped(){ if (!S.stamps && durationRate() > 1) { S.stamps = 1; } }
  /* Whether an animation plays while the timers run faster: CSS and Web Animations play on the engine's
     own timeline, at real speed (R5-F9). A finished one that holds its last frame does not play. Looked
     at on every audit, with the engine's own getAnimations, taken now. A worker has no document. */
  var _anims = (typeof document !== 'undefined' && typeof document.getAnimations === 'function') ? document.getAnimations : null;
  function checkAnimated(){
    if (S.animated || !_anims) { return; }
    try {
      if (durationRate() <= 1) { return; }
      var list = _anims.call(document);
      for (var i = 0; i < list.length; i++) { if (list[i].playState === 'running') { S.animated = 1; return; } }
    } catch (e) {}
  }

  var api = {
    set: function(fakeStart, realStart, m, d, scheduled, wallMax, zone){
      setHere(fakeStart, realStart, m, d, scheduled, wallMax, zone);
      eachFrame(function(f){ f.set(fakeStart, realStart, m, d, scheduled, wallMax, zone); });
      return 'ok';
    },
    schedule: function(at, m, d){
      var answer = scheduleHere(at, m, d);
      eachFrame(function(f){ f.schedule(at, m, d); });
      return answer;
    },
    /* The real wall with the duration axis on at rate 1, and the audit started over: what a page reads
       after the session is no evidence about its zone, and a session that reaches the page later must
       not take this one's facts and calls for its own (CodeRabbit on #89) - the counts asked with the
       release run first in the same context, and the session keeps the highest count it was told. */
    release: function(){
      S.Z = null; S.zoneMissed = 0; S.hiddenFast = 0; S.capped = 0; S.stamps = 0; S.animated = 0;
      for (var k in S.counts) { S.counts[k] = 0; }
      setHere(0, 0, 1, 1, null);
      eachFrame(function(f){ f.release(); });
      return 'ok';
    },
    /* What the session reads once a second: the call counts, the facts the end report draws on, and
       how many frames below this document answered - their counts and facts are this page's. */
    audit: function(){
      checkZone();
      checkHidden();
      checkAnimated();
      var o = { zone: S.zoneMissed, hidden: S.hiddenFast, capped: S.capped, stamps: S.stamps, animated: S.animated, frames: 0 }, k;
      for (k in S.counts) { o[k] = S.counts[k]; }
      eachFrame(function(f){
        var a = f.audit();
        for (k in S.counts) { o[k] += a[k] > 0 ? a[k] : 0; }
        o.zone = (o.zone || a.zone) ? 1 : 0;
        o.hidden = (o.hidden || a.hidden) ? 1 : 0;
        o.capped = (o.capped || a.capped) ? 1 : 0;
        o.stamps = (o.stamps || a.stamps) ? 1 : 0;
        o.animated = (o.animated || a.animated) ? 1 : 0;
        o.frames += 1 + (a.frames > 0 ? a.frames : 0);
      });
      return o;
    },
    /* The offset the engine's own Date shows at two instants - a page that replaced Date does not
       answer for the engine (ADR-21). */
    zoneRead: function(a, b){
      try { return [new _OrigDate(a).getTimezoneOffset(), new _OrigDate(b).getTimezoneOffset()]; } catch (e) { return null; }
    }
  };
  Object.freeze(api);
  /* Before anything is replaced: a page whose global object takes no new property fails here, as a
     context the session could not cover, not as one half shimmed. */
  Object.defineProperty(globalThis, '__chronomock', { value: api, enumerable: false, writable: false, configurable: false });

  /* Replace Date so new Date() (no args), Date() and Date.now() read the session clock; every other
     form (parsing, explicit fields) is unchanged. Reflect.construct with new.target keeps a subclass
     (class X extends Date) an X - building a plain Date here dropped its prototype (R4-W6). A Date
     built here, from no value or from one, is built directly: its prototype is the engine's, which
     is this one's (R5-WY3). */
  function CMDate(value) {
    if (!new.target) { S.counts.date++; return _dateString.call(new _OrigDate(fakeNow())); }
    var n = arguments.length;
    if (n === 0) {
      S.counts.date++;
      return new.target === CMDate ? new _OrigDate(fakeNow()) : Reflect.construct(_OrigDate, [fakeNow()], new.target);
    }
    if (n === 1 && new.target === CMDate) { return new _OrigDate(value); }
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

  /* Timers (R5-P6). A delay is the page's, in fake milliseconds, read as the engine reads it - a whole
     number, anything else 0 - and divided by the duration rate, rounded: the engine cut 1000 / 60 down
     to 16 and ran the timer 4% fast (measured). At the rate 1 a one-time timer goes to the engine as it
     came. A repeating timer with a function is a chain of one-time timers, each aimed one step at the
     rate of now past the last tick, so a change of speed reaches it from its next tick - a native one
     kept the cadence it was armed with - and its own lateness is made good at the next tick. A tick
     missed whole is not made up: the chain goes on from now.
     The engine runs a nested timer no sooner than 4 ms past a nesting level of 5 (HTML, measured on
     Electron 44 from the seventh), so a timer that would have to run more often than that at this speed
     runs slower than it - noted for the audit, and a repeating one handed over to the engine's own
     repeating timer, which keeps that floor's cadence best. The level is followed as the engine follows it, through
     the callbacks the shim runs: a timer armed from a promise that a callback left behind is not seen
     at its level, and is not noted. */
  var _si = globalThis.setInterval, _st = globalThis.setTimeout, _ci = globalThis.clearInterval, _ct = globalThis.clearTimeout;
  var level = 0;                    /* the timer nesting level of the callback the shim runs now, 0 outside one */
  var chains = (typeof Map === 'function' && _st && _ct) ? new Map() : null;  /* a repeating timer's id -> its chain */
  var realNow = _perf || _now;
  function fakeDelay(d){ var n = Number(d) | 0; return n > 0 ? n : 0; }
  /* deep: armed past the nesting level the floor starts at. An engine's own repeating timer always gets
     there, from its seventh tick. */
  function floored(n, ms, rate, deep){ if (deep && rate > 1 && n > 0 && ms < 4) { S.capped = 1; } }
  function nested(fn, lv, args){
    return function(){ var prev = level; level = lv; try { return fn.apply(this, args); } finally { level = prev; } };
  }
  /* Arm the chain's next tick from inside the tick before it, as the engine nests it. Under the floor
     a chain would lose the time each tick takes to the next one's 4 ms - 210 ticks a second where the
     engine's own repeating timer keeps 250 (measured on Electron 44) - so there the chain hands over to
     the engine's own, until a tick finds the speed changed. */
  function arm(c){
    var now = realNow(), rate = durationRate(), step = c.n / rate;
    floored(c.n, step, rate, c.level > 5);
    if (c.level > 5 && step < 4 && _si && _ci) {
      c.native = step;
      c.id = _si.call(globalThis, c.run, Math.round(step));
      return c.id;
    }
    c.native = null;
    c.due = c.due + step < now ? now + step : c.due + step;
    c.id = _st.call(globalThis, c.run, Math.max(0, Math.round(c.due - now)));
    return c.id;
  }
  if (_st) {
    globalThis.setTimeout = function(fn, d, ...args){
      S.counts.st++;
      var rate = durationRate();
      if (rate <= 1) { return _st.call(globalThis, fn, d, ...args); }
      var n = fakeDelay(d), ms = Math.round(n / rate);
      floored(n, ms, rate, level > 5);
      if (typeof fn !== 'function') { return _st.call(globalThis, fn, ms, ...args); }
      return _st.call(globalThis, nested(fn, level + 1, args), ms);
    };
  }
  if (_si) {
    globalThis.setInterval = function(fn, d, ...args){
      S.counts.si++;
      if (!chains || typeof fn !== 'function') {
        var rate = durationRate(), n = fakeDelay(d), ms = rate <= 1 ? d : Math.round(n / rate);
        floored(n, ms, rate, true);
        return _si.call(globalThis, fn, ms, ...args);
      }
      var c = { fn: fn, args: args, n: fakeDelay(d), due: realNow(), level: level, id: 0, run: null, native: null };
      c.run = function(){
        var prev = level;
        level = c.level + 1;
        c.level = level;
        if (c.native === null) {
          arm(c);
        } else if (c.n / durationRate() !== c.native) {
          _ci.call(globalThis, c.id);
          c.due = realNow();
          arm(c);
        }
        try { c.fn.apply(this, c.args); } finally { level = prev; }
      };
      var id = arm(c);
      chains.set(id, c);
      return id;
    };
  }
  /* A repeating timer's id is its first tick's, and stands for the tick waiting now. Either clear takes
     either kind, as the engine's do. */
  function clearChain(id){
    var c = chains && chains.get(id);
    if (!c) { return false; }
    chains.delete(id);
    if (c.native === null) { _ct.call(globalThis, c.id); } else { _ci.call(globalThis, c.id); }
    return true;
  }
  if (_ct) { globalThis.clearTimeout = function(id){ if (!clearChain(id)) { _ct.call(globalThis, id); } }; }
  if (_ci) { globalThis.clearInterval = function(id){ if (!clearChain(id)) { _ci.call(globalThis, id); } }; }
  if (_perf) {
    performance.now = function(){ S.counts.perf++; return perfNow(); };
    /* performance.timeOrigin plus performance.now() is how a page builds "now" from the high-resolution
       clock, so the origin is read as the session clock less the duration axis, each time it is read,
       and the sum is Date.now() (R5-P8). performance.toJSON() keeps the engine's own. */
    try {
      Object.defineProperty(performance, 'timeOrigin', { configurable: true, enumerable: true, get: function(){ return fakeNow() - perfNow(); } });
    } catch (e) {}
  }

  /* The engine's own stamps, handed back as the engine gives them (R5-F9): an animation frame is
     noticed when the engine hands its stamp to the page's callback - a frame cancelled before it runs
     gets none (CodeRabbit on #129) - and the two attributes when they are read. The frame's id is the
     engine's, so cancelAnimationFrame takes it, and what is not a function goes to the engine as it
     came, to be refused there. A wrapped attribute keeps the engine's shape - an accessor, enumerable
     or not as it was - and one the engine would not let go of is left as it is. */
  var _raf = globalThis.requestAnimationFrame;
  if (typeof _raf === 'function') {
    globalThis.requestAnimationFrame = function(cb){
      if (typeof cb !== 'function') { return _raf.call(globalThis, cb); }
      return _raf.call(globalThis, function(){ stamped(); return cb.apply(this, arguments); });
    };
  }
  function stampOn(proto, name){
    var d = null;
    try { d = proto ? Object.getOwnPropertyDescriptor(proto, name) : null; } catch (e) { d = null; }
    if (!d || typeof d.get !== 'function' || !d.configurable) { return; }
    var get = d.get;
    try { Object.defineProperty(proto, name, { configurable: true, enumerable: d.enumerable, get: function(){ stamped(); return get.call(this); } }); } catch (e) {}
  }
  stampOn(globalThis.Event && globalThis.Event.prototype, 'timeStamp');
  stampOn(globalThis.AnimationTimeline && globalThis.AnimationTimeline.prototype, 'currentTime');

  try { if (typeof document !== 'undefined' && document.addEventListener) { document.addEventListener('visibilitychange', checkHidden); } } catch (e) {}
  checkZone();
  checkHidden();
  /* Whether the page's own scripts were here before the shim (R5-P7): a script element already in the
     document has run, or is about to, and may have read the real clock first. A new document's hook
     runs before any of them, so it finds none. A worker cannot tell - the session knows that from
     whether the worker was paused on start. */
  return (typeof document !== 'undefined' && document.scripts && document.scripts.length > 0) ? 'running' : 'installed';
})()"#;

/// Read a context's per-API call counts and its audit facts (or `null` if the shim is not installed
/// there). The counts make an honest "covered means the app actually called it" report, the same way
/// the native audit counts channel queries - an override that was installed but never exercised is not
/// "covered". A shim without `audit` (left in a page by an older build) still answers its counts.
pub const COUNTS_EXPR: &str =
    "(globalThis.__chronomock && (globalThis.__chronomock.audit ? globalThis.__chronomock.audit() : globalThis.__chronomock.counts)) || null";

/// The session's zone as the engine's zone override names it: the fixed offset `GMT+HH:MM` or
/// `GMT-HH:MM`, from the session's bias in Windows' sense (minutes west of UTC, so `+02:00` is -120).
/// A fixed offset, like the zone the native hook hands an application - no daylight saving. Measured on
/// an Electron page and its worker: `GMT+05:30`, `GMT+05:45`, `GMT+14:00`, `GMT-12:00` and
/// `GMT+00:00` (read back as `UTC`) all take, and read back the offset they name in January and July
/// alike.
pub fn zone_id(bias_min: i32) -> String {
    let east = -i64::from(bias_min);
    let sign = if east < 0 { '-' } else { '+' };
    let minutes = east.unsigned_abs();
    format!("GMT{sign}{:02}:{:02}", minutes / 60, minutes % 60)
}

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
///
/// `zone_bias_min` is the session's zone bias (minutes west of UTC), the offset the shim checks its
/// context reads - `None` when the session sets no zone, and then nothing is checked.
pub fn build_shim(
    fake_start_ms: i64,
    real_start_ms: i64,
    mult: i64,
    dur: i64,
    scheduled: Option<ScheduledRate>,
    wall_max_ms: i64,
    zone_bias_min: Option<i32>,
) -> String {
    SHIM_TEMPLATE
        .replace("__MULT__", &mult.to_string())
        .replace("__DUR__", &dur.max(1).to_string())
        .replace("__FAKE_START__", &fake_start_ms.to_string())
        .replace("__REAL_START__", &real_start_ms.to_string())
        .replace("__SCHEDULED__", &scheduled_js(scheduled))
        .replace("__WALL_MAX__", &wall_max_ms.to_string())
        .replace("__ZONE__", &zone_bias_min.map_or_else(|| "null".to_string(), |z| z.to_string()))
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
    set_call("", fake0, real0, mult, dur, scheduled)
}

/// The release: the shim's own `release`, the clock of [`set_expr`] on `(0, 0, 1, 1)` with the zone
/// no longer judged and the audit started over, in the page and the frames it reaches. What a page
/// reads after the session is no evidence about the session's zone, so a count asked after the release
/// cannot report that zone as missed (CodeRabbit on #89). And a page let go keeps its shim, so a
/// session that reaches it later would take this session's zone miss, hidden-window fact and calls for
/// its own (CodeRabbit on #89, second review): they are zeroed after this session read them - the counts
/// asked with the release run first in the same context, and the session keeps the highest count it
/// was told. A shim of an older build has no `release`, and its open fields are zeroed here instead.
pub fn release_expr() -> String {
    set_call(
        "if(typeof S.release==='function')return S.release();S.Z=null;S.zoneMissed=0;S.hiddenFast=0;for(var k in S.counts){S.counts[k]=0;}",
        0,
        0,
        1,
        1,
        None,
    )
}

/// The expression that reads what a context shows for the zone after the release: `getTimezoneOffset()`
/// at the two instants `at_ms` (Unix-epoch ms), as an array of two numbers, `null` when it could not.
/// Through the engine's own `Date` the shim kept (its `zoneRead`), so a page that replaced `Date` cannot
/// answer for it - through the `_Date` a shim of an older build kept open, and through `Date` where
/// there is no shim. A date given as a number neither reads the session clock nor counts as a call
/// (ADR-21).
pub fn zone_read_expr(at_ms: [i64; 2]) -> String {
    let [a, b] = at_ms;
    format!(
        "(function(){{var S=globalThis.__chronomock;if(S&&typeof S.zoneRead==='function')return S.zoneRead({a},{b});var D=(S&&S._Date)||Date;try{{return [new D({a}).getTimezoneOffset(),new D({b}).getTimezoneOffset()];}}catch(e){{return null;}}}})()"
    )
}

fn set_call(before: &str, fake0: i64, real0: i64, mult: i64, dur: i64, scheduled: Option<ScheduledRate>) -> String {
    format!(
        "(function(){{var S=globalThis.__chronomock;if(!S)return 'no-shim';{before}return S.set({},{},{},{},{});}})()",
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
    /// Whether the shim found the page's own scripts already in its document (R5-P7) - they may have
    /// read the real clock before it. Always `false` for a worker, which the caller judges by whether
    /// it was paused on start.
    pub running: bool,
    /// The page had no document to evaluate in yet, and its shim is the new-document hook's to give
    /// (R5-F11): covered only once its audit shows the shim is there (R5-D46).
    pub awaiting: bool,
}

/// Whether a failed evaluate found no document to run in - a frame of another site, attached paused
/// before its first document, has none (R5-F11, measured on Electron 44). The text is the engine's:
/// read here, at the one place that judges it, and nowhere else.
fn found_no_document(error: &io::Error) -> bool {
    error.to_string().contains("Cannot find default execution context")
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
///
/// The zone override, when the session sets one, goes first of all, so the page's own first script
/// already reads the session's zone. Not waited for either: whether it took is read in the page by the
/// shim (see [`set_zone`]).
///
/// A frame of another site is a target of its own, attached paused before its first document: the
/// evaluate finds nothing to run in. The hook gives that document its shim before its own scripts, so
/// with the hook in such a frame is not a failure - it used to be taken back and left on the real clock
/// (R5-F11). The caller counts it covered once its audit shows the shim (`awaiting`).
///
/// A page that auto-attach delivered `paused` on start can answer nothing at all until it is released:
/// not the enable, not the hook, not the shim. Measured 2026-11-11 on an Electron application that opens
/// its window after the session armed auto-attach: the page came with no address yet, and every answer
/// came within 25 ms of its release - after the ten seconds the session waited for the hook, when it had
/// already given the page up and sent it back to the real clock, its hook taken and its shim in. So such
/// a page gets the auto-attach for its workers and its release with the rest, before any answer is
/// awaited. The order holds: the page's session takes its commands in the order they came, for a page
/// paused on start the auto-attach and the release are both the browser's, and the hook and the shim
/// reach its first document before its own scripts (measured: the shim found itself installed by the
/// hook). A page that was not paused keeps the order above.
pub fn inject_page(
    client: &mut CdpClient,
    session_id: &str,
    shim: &str,
    zone: Option<&str>,
    paused: bool,
    deadline: Instant,
) -> io::Result<Injected> {
    set_zone(client, session_id, zone)?;
    client.send("Page.enable", json!({}), Some(session_id))?;
    let hook = client.send("Page.addScriptToEvaluateOnNewDocument", json!({ "source": shim }), Some(session_id))?;
    let shimmed = send_shim(client, session_id, shim)?;
    let armed = if paused {
        let armed = client.send("Target.setAutoAttach", auto_attach_params(), Some(session_id))?;
        let _ = client.send("Runtime.runIfWaitingForDebugger", json!({}), Some(session_id));
        Some(armed)
    } else {
        None
    };
    let script = match client.reply_until(hook, "Page.addScriptToEvaluateOnNewDocument", deadline) {
        Ok(added) => script_identifier(&added),
        Err(e) => {
            take_back(client, session_id, None);
            return Err(e);
        }
    };
    let (running, awaiting) = match shim_reply(client, shimmed, deadline) {
        Ok(answer) => (answer == "running", false),
        Err(e) if script.is_some() && found_no_document(&e) => (false, true),
        Err(e) => {
            take_back(client, session_id, script.as_deref());
            return Err(e);
        }
    };
    let children = match armed {
        Some(id) => client.reply_until(id, "Target.setAutoAttach", deadline).is_ok(),
        None => {
            let children = auto_attach_children(client, session_id, deadline);
            let _ = client.send("Runtime.runIfWaitingForDebugger", json!({}), Some(session_id));
            children
        }
    };
    Ok(Injected { script, children, running, awaiting })
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
/// deadline for the sequence, as for a page. The zone override goes first here too: a worker reads its
/// process's override, and one in a process no page holds one in - a service worker, say - takes it
/// from its own session (measured: a dedicated worker's session answers "already in effect" while its
/// page holds the process's override, and reads it).
pub fn inject_worker(client: &mut CdpClient, session_id: &str, shim: &str, zone: Option<&str>, deadline: Instant) -> io::Result<Injected> {
    set_zone(client, session_id, zone)?;
    if let Err(e) = evaluate_shim(client, session_id, shim, deadline) {
        take_back(client, session_id, None);
        return Err(e);
    }
    // A worker can start workers of its own, and auto-attach set on the page does not reach them: a
    // worker started by a worker read the real clock (R4-N25, measured on an Electron page). Set before
    // the worker is released, so one it starts in its first script is paused for the shim too.
    let children = auto_attach_children(client, session_id, deadline);
    let _ = client.send("Runtime.runIfWaitingForDebugger", json!({}), Some(session_id));
    Ok(Injected { script: None, children, running: false, awaiting: false })
}

/// Put a context on the session's zone, without waiting: `zone` is a [`zone_id`], `None` for a session
/// that sets none, and an empty string takes the override away (the release). The override is the
/// renderer process's, held by the first context that sets it, so the answer - `ok`, or "already in
/// effect" - says nothing about what this context reads, and is not read. The shim reads the offset in
/// the context instead.
fn set_zone(client: &mut CdpClient, session_id: &str, zone: Option<&str>) -> io::Result<()> {
    if let Some(zone) = zone {
        client.send("Emulation.setTimezoneOverride", json!({ "timezoneId": zone }), Some(session_id))?;
    }
    Ok(())
}

/// Ask a context to attach the workers it starts, paused for the shim. `false` when it answered with
/// an error or not at all: the context itself is shimmed, but a worker it starts would run on the
/// real clock unseen, and the caller has to count that (rule 4). Measured on Chromium 153: a page, a
/// dedicated worker, a nested one, a shared worker and a service worker all take it.
fn auto_attach_children(client: &mut CdpClient, session_id: &str, deadline: Instant) -> bool {
    client.call_until("Target.setAutoAttach", auto_attach_params(), Some(session_id), deadline).is_ok()
}

/// What a context is asked for to attach the workers it starts, paused for the shim.
fn auto_attach_params() -> Value {
    json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true })
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
    shim_reply(client, shimmed, deadline).map(|_| ())
}

/// Send the shim to the context's current document, without waiting.
fn send_shim(client: &mut CdpClient, session_id: &str, shim: &str) -> io::Result<u64> {
    client.send("Runtime.evaluate", json!({ "expression": shim, "returnByValue": true }), Some(session_id))
}

/// Wait by `deadline` for the shim's reply, an error when it threw - its answer otherwise
/// (`installed`, `running`, `already`, empty when it gave none).
fn shim_reply(client: &mut CdpClient, shimmed: u64, deadline: Instant) -> io::Result<String> {
    let r = client.reply_until(shimmed, "Runtime.evaluate", deadline)?;
    if let Some(exc) = r.get("exceptionDetails") {
        return Err(shim_error(exc));
    }
    Ok(r["result"]["value"].as_str().unwrap_or("").to_string())
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
        let s = build_shim(1_700_000_000_000, 1_600_000_000_000, 60, 60, None, 900_000_000_000_000, None);
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
        let s = build_shim(1_700_000_000_000, 1_600_000_000_000, 60, 60, Some(next), 900_000_000_000_000, None);
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

    /// The two rates are filled in independently, and a frozen wall never stops a timer (untouchable
    /// rule 3), so the duration rate is floored at 1 whatever the caller passes. What the timers do with
    /// them is run in Node (`shim_exec`: a timer keeps its delay when only the wall runs fast).
    #[test]
    fn the_wall_rate_and_the_duration_rate_are_filled_in_separately() {
        let s = build_shim(0, 0, 60, 1, None, 0, None);
        assert!(s.contains("M: 60,"), "{s}");
        assert!(s.contains("D: 1,"), "{s}");

        let frozen = build_shim(0, 0, 0, 0, None, 0, None);
        assert!(frozen.contains("M: 0,"), "{frozen}");
        assert!(frozen.contains("D: 1,"), "a frozen wall keeps timers at real speed: {frozen}");
    }

    /// R4-W5: a second run of the shim in one document - the newer of two new-document hooks, which
    /// runs last - sets the whole clock it carries, the scheduled change, the last instant and the zone
    /// included. A shim of an older build gets the last two in its open fields. Anything else holding
    /// the name is a failure, not a page reported shimmed. Run in Node (`shim_exec`: a second install,
    /// and the clock closed over) - this pins what the guard is given.
    #[test]
    fn a_second_run_sets_the_clock_it_carries() {
        let next = ScheduledRate { at_ms: 2_500, mult: 60, dur: 60 };
        let s = build_shim(1_000, 2_000, 1, 1, Some(next), 3_000, Some(-60));
        let guard = &s[..s.find("return 'already';").expect("the guard returns early")];
        assert!(guard.contains("if (typeof O.release !== 'function') { O.wallMax = 3000; O.Z = -60; }"), "{guard}");
        assert!(guard.contains("O.set(1000, 2000, 1, 1, { at: 2500, M: 60, D: 60 }, 3000, -60);"), "{guard}");
        assert!(guard.contains("if (typeof O.set !== 'function') { throw "), "{guard}");
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
        let s = build_shim(0, 0, 60, 60, None, 0, None);
        assert!(s.contains("Reflect.construct(_OrigDate, arguments, new.target)"), "a subclass keeps its prototype");
        assert!(s.contains("if (!new.target) { S.counts.date++; return _dateString.call(new _OrigDate(fakeNow())); }"));
        assert!(s.contains("Object.defineProperty(_OrigDate.prototype, 'constructor', { value: CMDate"));
        assert!(s.contains("if (d === undefined) { S.counts.intl++; d = fakeNow(); }"), "Intl formats the session's now");
        assert!(s.contains("perfBase: _perf ? _perf() : 0,"), "performance.now does not restart at 0");
    }

    /// The zone read of the release (ADR-21) asks the shim's `zoneRead`, which reads through the engine's
    /// own `Date`, so a page that replaced `Date` does not answer for the engine - through the `_Date` a
    /// shim of an older build kept open, and through `Date` where there is no shim. It reads the two
    /// instants it is given and nothing else. Run in Node (`shim_exec`: the session moves, and the clock
    /// closed over) - this pins the instants.
    #[test]
    fn the_zone_read_goes_through_the_engines_own_date() {
        let read = zone_read_expr([1_768_478_400_000, 1_784_116_800_000]);
        assert!(read.contains("return S.zoneRead(1768478400000,1784116800000);"), "{read}");
        assert!(read.contains("D=(S&&S._Date)||Date;"), "{read}");
        assert!(read.contains("[new D(1768478400000).getTimezoneOffset(),new D(1784116800000).getTimezoneOffset()]"), "{read}");
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
        assert!(inject_page(&mut client, "P", "SHIM", None, false, soon()).is_err());
        assert!(inject_page(&mut client, "Q", "SHIM", None, false, soon()).is_err());
        assert!(inject_worker(&mut client, "W", "SHIM", None, soon()).is_err());
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
        let injected = inject_page(&mut client, "P", "SHIM", None, false, Instant::now() + Duration::from_secs(2)).expect("the page took the shim");
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

    /// A page auto-attach delivered paused on start, before its first document, answers nothing until
    /// it is released - measured on an Electron application whose window opened after the session
    /// armed auto-attach. This browser holds every answer of the page until its release, as that page
    /// did: the injection sends the auto-attach for the page's workers and the release with the hook
    /// and the shim, in that order and before it waits, and the page is covered by its hook and its
    /// shim, with no second release.
    #[test]
    fn a_page_paused_on_start_is_released_with_its_shim_before_any_answer() {
        use std::time::Duration;
        let mut held = Vec::new();
        let (port, browser) = super::super::ws::tests::fake_browser_holding(move |request| {
            let id = request["id"].clone();
            let answer = match request["method"].as_str().unwrap_or("") {
                "Page.addScriptToEvaluateOnNewDocument" => json!({ "identifier": "h1" }),
                "Runtime.evaluate" => json!({ "result": { "type": "string", "value": "already" } }),
                "Runtime.runIfWaitingForDebugger" => {
                    let mut now = std::mem::take(&mut held);
                    now.push((id, json!({})));
                    return now;
                }
                _ => json!({}),
            };
            held.push((id, answer));
            Vec::new()
        });
        let ws = super::super::WsClient::connect("127.0.0.1", port, "/", Instant::now() + Duration::from_secs(5)).unwrap();
        let mut client = CdpClient::from_ws(ws);
        // A page that answers only once released never answers an injection that waits first: the deadline
        // passes, and the injection fails.
        let injected = inject_page(&mut client, "P", "SHIM", Some("GMT+01:00"), true, Instant::now() + Duration::from_secs(2))
            .expect("the paused page took the shim");
        assert_eq!(injected.script.as_deref(), Some("h1"));
        assert!(injected.children, "its workers are attached");
        assert!(!injected.running && !injected.awaiting, "the hook put the shim in before the page's own scripts");
        drop(client);
        let methods: Vec<String> =
            browser.join().unwrap().iter().map(|r| r["method"].as_str().unwrap_or("").to_string()).collect();
        assert_eq!(
            methods,
            [
                "Emulation.setTimezoneOverride",
                "Page.enable",
                "Page.addScriptToEvaluateOnNewDocument",
                "Runtime.evaluate",
                "Target.setAutoAttach",
                "Runtime.runIfWaitingForDebugger",
            ]
        );
    }

    /// R5-F11: a frame of another site, attached paused before its first document, has no context for
    /// the shim's evaluate - the engine's own words, measured on Electron 44. With the hook in, the
    /// document it gets takes the shim from the hook, so the frame is not taken back but handed on as
    /// waiting for its shim, and resumed. Any other failure is still a failure, and so is this one for a
    /// page whose hook gave no identifier - nothing would carry the shim to its document.
    #[test]
    fn a_frame_with_no_document_yet_waits_for_its_shim_from_the_hook() {
        use std::time::Duration;
        let inject = |message: &'static str, hook_id: Option<&'static str>| {
            let (port, browser) = super::super::ws::tests::fake_browser_holding(move |request| {
                let id = request["id"].clone();
                match request["method"].as_str().unwrap_or("") {
                    "Page.addScriptToEvaluateOnNewDocument" => {
                        vec![(id, hook_id.map_or_else(|| json!({}), |h| json!({ "identifier": h })))]
                    }
                    "Runtime.evaluate" if request["params"]["expression"] == "SHIM" => {
                        vec![(Value::Null, json!({ "id": id, "error": { "code": -32000, "message": message } }))]
                    }
                    _ => vec![(id, json!({}))],
                }
            });
            let ws = super::super::WsClient::connect("127.0.0.1", port, "/", Instant::now() + Duration::from_secs(5)).unwrap();
            let mut client = CdpClient::from_ws(ws);
            let injected = inject_page(&mut client, "F", "SHIM", None, false, Instant::now() + Duration::from_secs(2));
            drop(client);
            let methods: Vec<String> =
                browser.join().unwrap().iter().map(|r| r["method"].as_str().unwrap_or("").to_string()).collect();
            (injected, methods)
        };
        let (waits, methods) = inject("Cannot find default execution context", Some("h1"));
        let waits = waits.expect("a frame with no document is not a failure");
        assert!(waits.awaiting && !waits.running, "it waits for its shim, and nothing of it ran yet");
        assert_eq!(waits.script.as_deref(), Some("h1"));
        assert!(methods.iter().all(|m| m != "Page.removeScriptToEvaluateOnNewDocument"), "its hook stays: {methods:?}");
        assert_eq!(methods.last().map(String::as_str), Some("Runtime.runIfWaitingForDebugger"), "and it is resumed");

        let (other, methods) = inject("Execution context was destroyed.", Some("h1"));
        assert!(other.is_err(), "another failure is a failure");
        assert!(methods.iter().any(|m| m == "Page.removeScriptToEvaluateOnNewDocument"), "taken back: {methods:?}");
        let (hookless, _) = inject("Cannot find default execution context", None);
        assert!(hookless.is_err(), "with no hook to carry the shim, no document is a failure");
    }

    /// The methods a page and a worker are sent when the session sets a zone, against a browser that
    /// answers everything at once - the zone answer included, which is not waited for.
    fn methods_sent(worker: bool, zone: Option<&str>) -> Vec<(String, Value)> {
        use std::time::Duration;
        let (port, browser) = super::super::ws::tests::fake_browser_holding(move |request| {
            let id = request["id"].clone();
            match request["method"].as_str().unwrap_or("") {
                "Page.addScriptToEvaluateOnNewDocument" => vec![(id, json!({ "identifier": "h1" }))],
                "Runtime.evaluate" => vec![(id, json!({ "result": {} }))],
                _ => vec![(id, json!({}))],
            }
        });
        let ws = super::super::WsClient::connect("127.0.0.1", port, "/", Instant::now() + Duration::from_secs(5)).unwrap();
        let mut client = CdpClient::from_ws(ws);
        let deadline = Instant::now() + Duration::from_secs(2);
        let injected = if worker {
            inject_worker(&mut client, "W", "SHIM", zone, deadline)
        } else {
            inject_page(&mut client, "P", "SHIM", zone, false, deadline)
        };
        assert!(injected.is_ok(), "the context took the shim");
        drop(client);
        browser
            .join()
            .unwrap()
            .iter()
            .map(|r| (r["method"].as_str().unwrap_or("").to_string(), r["params"].clone()))
            .collect()
    }

    /// The session's zone goes to a page and to a worker before anything else, so their first script
    /// already reads it, and with the session's offset in the engine's own spelling. A session that
    /// sets no zone sends none.
    #[test]
    fn the_zone_override_goes_out_first_to_a_page_and_to_a_worker() {
        for worker in [false, true] {
            let sent = methods_sent(worker, Some("GMT+05:30"));
            assert_eq!(sent[0].0, "Emulation.setTimezoneOverride", "worker: {worker}");
            assert_eq!(sent[0].1["timezoneId"], "GMT+05:30");
            assert_eq!(sent.iter().filter(|(m, _)| m == "Emulation.setTimezoneOverride").count(), 1);
            let none = methods_sent(worker, None);
            assert!(none.iter().all(|(m, _)| m != "Emulation.setTimezoneOverride"), "worker: {worker}");
        }
    }

    /// R5-P7: the shim says whether the page's own scripts were in its document before it - they may
    /// have read the real clock first - and the session counts that page as reached while running. A
    /// new document's hook runs before them and finds none.
    #[test]
    fn the_shim_says_when_it_found_the_pages_scripts_before_it() {
        let s = build_shim(0, 0, 1, 1, None, 0, None);
        assert!(
            s.contains("return (typeof document !== 'undefined' && document.scripts && document.scripts.length > 0) ? 'running' : 'installed';"),
            "{s}"
        );
    }

    /// The zone the override names is the session's fixed offset, from a bias in Windows' sense.
    #[test]
    fn the_zone_is_named_as_a_fixed_offset_from_the_bias() {
        assert_eq!(zone_id(-120), "GMT+02:00");
        assert_eq!(zone_id(-330), "GMT+05:30");
        assert_eq!(zone_id(-345), "GMT+05:45");
        assert_eq!(zone_id(180), "GMT-03:00");
        assert_eq!(zone_id(720), "GMT-12:00");
        assert_eq!(zone_id(-840), "GMT+14:00");
        assert_eq!(zone_id(0), "GMT+00:00");
        assert_eq!(zone_id(30), "GMT-00:30", "a negative offset under an hour keeps its sign");
    }

    /// The shim carries the offset it checks its context reads - the answer to the override is no proof
    /// (a second page in a process reads the holder's override and loses it with the holder) - and a
    /// second install carries it too. A session with no zone carries none. The check itself, at install
    /// and at every audit, is run in Node (`shim_exec`: the audit notes a zone it did not read).
    #[test]
    fn the_shim_carries_the_zone_its_context_reads() {
        let s = build_shim(0, 0, 1, 1, None, 0, Some(-330));
        assert!(s.contains("Z: -330,"), "{s}");
        assert!(s.contains("O.Z = -330;"), "a second install over an older shim carries the zone too");
        assert!(s.contains("O.set(0, 0, 1, 1, null, 0, -330);"), "and over one of this build");
        assert!(build_shim(0, 0, 1, 1, None, 0, None).contains("Z: null,"));
        assert!(COUNTS_EXPR.contains("__chronomock.audit()"));
    }
}
