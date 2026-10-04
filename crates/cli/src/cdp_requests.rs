//! The contexts one attacher drives, and every request in flight to them (R4-S10, ADR-20).
//!
//! The attacher used to ask each context in turn and wait for each answer, so a context busy with its
//! own JS held the session loop for a whole call deadline - ten seconds in a Chromium session, two
//! beside a native one, once a second, for every busy context (measured: a page and two workers
//! busy, and the driver stopped the core after fifteen silent seconds). Here a request is sent and
//! written down, and its answer is acted on whenever it comes. A deadline bounds how long the
//! caller waits, never what is learned: a hook identifier that comes late is still kept, a count
//! that comes late still counts, and a request with no answer by the end is a fact for the report.
//!
//! Pure over the sends: [`Outbox`] is the one thing it needs from a connection, so every rule here is
//! tested without a browser.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use serde_json::{json, Value};

use crate::cdp;

/// One shimmed JS context of a Chromium target: the coverage unit of a CDP session (rule 4 - never
/// summed across contexts).
pub(crate) struct CdpContext {
    pub(crate) index: u32,
    pub(crate) session_id: String,
    pub(crate) ty: String,
    /// The CDP targetId, kept because `Target.targetDestroyed` names a target, not a session.
    pub(crate) target_id: String,
    /// A page's new-document hooks, replaced whenever the clock moves (R4-W5). None for a worker,
    /// which has no hook - one started later is a new target and is shimmed from the clock of then.
    pub(crate) hooks: PageHooks,
    /// A count request is in flight. A context that has not answered the last one is not asked again
    /// until it does - a busy context does not pile up requests (R4-S10).
    counting: bool,
    /// Let go of at the end of the session: from here on it gets no clock move, and a hook that comes
    /// back is removed at once.
    released: bool,
    /// How it answered the release, once it has.
    release_answer: Option<bool>,
}

impl CdpContext {
    pub(crate) fn new(index: u32, session_id: String, ty: String, target_id: String, script: Option<String>) -> CdpContext {
        CdpContext {
            index,
            session_id,
            ty,
            target_id,
            hooks: PageHooks { current: script, unremoved: Vec::new() },
            counting: false,
            released: false,
            release_answer: None,
        }
    }
}

/// The new-document hooks of one page: the one that carries the clock now, and earlier ones whose
/// removal the page did not confirm. Those are still there as far as anyone knows, so every later
/// move and the release try again - forgotten, one would put a document loaded after the release
/// back on a session clock.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PageHooks {
    current: Option<String>,
    unremoved: Vec<String>,
}

impl PageHooks {
    /// No hook at all - what a page let go of has, once every removal is confirmed.
    const NONE: PageHooks = PageHooks { current: None, unremoved: Vec::new() };
}

/// What a connection does for the requests: send one and say its id, `None` when it could not be sent.
pub(crate) trait Outbox {
    fn ask(&mut self, method: &str, params: Value, session: &str) -> Option<u64>;
}

impl Outbox for cdp::CdpClient {
    fn ask(&mut self, method: &str, params: Value, session: &str) -> Option<u64> {
        self.send(method, params, Some(session)).ok()
    }
}

/// A request in flight, by what it was for.
#[derive(Debug)]
enum Asked {
    Counts { session: String, index: u32, ty: String },
    /// A new hook for a move. The move's expression goes to the page when the hook comes back, so the
    /// page has the new hook before its live document moves (R4-W5) - however long it takes.
    Hook { session: String, expr: Arc<str> },
    Unhook { session: String, script: String },
    Move { session: String },
    Release { session: String },
}

impl Asked {
    fn session(&self) -> &str {
        match self {
            Asked::Counts { session, .. }
            | Asked::Hook { session, .. }
            | Asked::Unhook { session, .. }
            | Asked::Move { session, .. }
            | Asked::Release { session } => session,
        }
    }
}

const ADD_HOOK: &str = "Page.addScriptToEvaluateOnNewDocument";
const REMOVE_HOOK: &str = "Page.removeScriptToEvaluateOnNewDocument";
const EVALUATE: &str = "Runtime.evaluate";

/// The live contexts of one attacher, the requests in flight to them, and what their answers said.
#[derive(Default)]
pub(crate) struct Requests {
    contexts: Vec<CdpContext>,
    asked: HashMap<u64, Asked>,
    /// Per-context call counts, keyed by `(context index, "type api")`, merged by max so a peak
    /// survives a reload.
    counts: BTreeMap<(u32, String), u64>,
    /// The pages that missed a clock move: one that did not take its new hook, one that answered the
    /// move with anything but `ok` or `no-shim` (`late` from R4-S17), and one that had not answered by
    /// the end. A page once, however many moves it missed - the warning is about pages, and the set
    /// stays as small as the list of contexts (CodeRabbit on #85).
    missed: BTreeSet<String>,
}

impl Requests {
    pub(crate) fn push(&mut self, context: CdpContext) {
        self.contexts.push(context);
    }

    /// The live contexts, in attach order.
    pub(crate) fn contexts(&self) -> &[CdpContext] {
        &self.contexts
    }

    /// Drop a context that went away - a closed window, a recycled worker - with its requests in
    /// flight. They are not counted as missed: a page that is gone has no clock to stand apart. A
    /// reload does not detach its target, so it is not this. `true` when something was dropped.
    pub(crate) fn forget(&mut self, session: &str, target: &str) -> bool {
        let gone: Vec<String> = self
            .contexts
            .iter()
            .filter(|c| (!session.is_empty() && c.session_id == session) || (!target.is_empty() && c.target_id == target))
            .map(|c| c.session_id.clone())
            .collect();
        self.contexts.retain(|c| !gone.contains(&c.session_id));
        self.asked.retain(|_, a| !gone.iter().any(|s| s == a.session()));
        !gone.is_empty()
    }

    /// Ask every context that is not still answering the last request for its call counts, without
    /// waiting (R4-S10).
    pub(crate) fn request_counts(&mut self, out: &mut impl Outbox) {
        for ctx in self.contexts.iter_mut().filter(|c| !c.counting) {
            let params = json!({ "expression": cdp::COUNTS_EXPR, "returnByValue": true });
            if let Some(id) = out.ask(EVALUATE, params, &ctx.session_id) {
                ctx.counting = true;
                self.asked.insert(
                    id,
                    Asked::Counts { session: ctx.session_id.clone(), index: ctx.index, ty: ctx.ty.clone() },
                );
            }
        }
    }

    /// Start a clock move: a new hook for every page, built on `shim`, and `expr` for every live
    /// document - a page's once its new hook is back, a worker's at once. Returns the hook requests,
    /// for a caller that waits a bounded time for them before it acknowledges the move.
    pub(crate) fn start_move(&mut self, expr: &str, shim: &str, out: &mut impl Outbox) -> Vec<u64> {
        let expr: Arc<str> = Arc::from(expr);
        let mut hooks = Vec::new();
        for ctx in self.contexts.iter().filter(|c| !c.released) {
            let session = ctx.session_id.clone();
            if ctx.hooks.current.is_none() {
                send_move(&mut self.asked, &mut self.missed, &session, &expr, out);
                continue;
            }
            match out.ask(ADD_HOOK, json!({ "source": shim }), &session) {
                Some(id) => {
                    self.asked.insert(id, Asked::Hook { session, expr: Arc::clone(&expr) });
                    hooks.push(id);
                }
                None => {
                    // Not sent: the page keeps the hook it has, and its live document still moves.
                    self.missed.insert(session.clone());
                    send_move(&mut self.asked, &mut self.missed, &session, &expr, out);
                }
            }
        }
        hooks
    }

    /// Whether none of these requests is still in flight.
    pub(crate) fn answered(&self, ids: &[u64]) -> bool {
        ids.iter().all(|id| !self.asked.contains_key(id))
    }

    /// Let every context go: every hook it may have removed, and `expr` evaluated. From here on no
    /// clock move reaches it - a move still waiting for its hook would put the page back on the
    /// session clock after the session - and a hook that comes back is removed at once.
    pub(crate) fn start_release(&mut self, expr: &str, out: &mut impl Outbox) {
        for ctx in &mut self.contexts {
            ctx.released = true;
            ctx.hooks.unremoved.extend(ctx.hooks.current.take());
            unhook_all(ctx, &mut self.asked, out);
            let params = json!({ "expression": expr, "returnByValue": true });
            match out.ask(EVALUATE, params, &ctx.session_id) {
                Some(id) => {
                    self.asked.insert(id, Asked::Release { session: ctx.session_id.clone() });
                }
                None => ctx.release_answer = Some(false),
            }
        }
    }

    /// Whether every release and every hook request has been answered.
    pub(crate) fn release_settled(&self) -> bool {
        !self.asked.values().any(|a| matches!(a, Asked::Release { .. } | Asked::Hook { .. } | Asked::Unhook { .. }))
    }

    /// Whether every count request has been answered.
    pub(crate) fn counts_settled(&self) -> bool {
        !self.asked.values().any(|a| matches!(a, Asked::Counts { .. }))
    }

    /// How many contexts did not confirm they were let go: one that answered the release with
    /// anything but `ok` or `no-shim`, one that had not answered it, and one that may still have a
    /// hook - a removal it did not confirm, or a hook still on its way back.
    pub(crate) fn unreleased(&self) -> u32 {
        let still = |session: &str| {
            self.asked
                .values()
                .any(|a| a.session() == session && matches!(a, Asked::Hook { .. } | Asked::Unhook { .. }))
        };
        let count = self
            .contexts
            .iter()
            .filter(|c| c.release_answer != Some(true) || c.hooks != PageHooks::NONE || still(&c.session_id))
            .count();
        u32::try_from(count).unwrap_or(u32::MAX)
    }

    /// At the end of the session: every move a live page has not answered is a move it missed.
    pub(crate) fn settle_moves(&mut self) {
        for asked in self.asked.values() {
            if let Asked::Hook { session, .. } | Asked::Move { session } = asked {
                self.missed.insert(session.clone());
            }
        }
    }

    /// How many pages missed at least one clock move.
    pub(crate) fn moves_missed(&self) -> usize {
        self.missed.len()
    }

    pub(crate) fn into_counts(self) -> BTreeMap<(u32, String), u64> {
        self.counts
    }

    /// Act on the answer to request `id`, whenever it comes. `reply` is the result, or `Err` for an
    /// error answer. An answer to a request this table did not make, or one dropped with its context,
    /// is nothing to act on.
    pub(crate) fn on_reply(&mut self, id: u64, reply: Result<Value, ()>, out: &mut impl Outbox) {
        let Some(asked) = self.asked.remove(&id) else {
            return;
        };
        let at = self.contexts.iter().position(|c| c.session_id == asked.session());
        match asked {
            Asked::Counts { index, ty, .. } => {
                if let Some(i) = at {
                    self.contexts[i].counting = false;
                }
                if let Ok(reply) = reply {
                    merge_counts(&mut self.counts, index, &ty, &reply);
                }
            }
            Asked::Hook { session, expr } => {
                let new = reply.ok().as_ref().and_then(cdp::script_identifier);
                if new.is_none() {
                    // The page did not take the new hook: it keeps the one it had, and a reload brings
                    // back that hook's clock.
                    self.missed.insert(session.clone());
                }
                let Some(i) = at else {
                    return;
                };
                let ctx = &mut self.contexts[i];
                if let Some(new) = new {
                    if ctx.released {
                        ctx.hooks.unremoved.push(new);
                    } else {
                        ctx.hooks.unremoved.extend(ctx.hooks.current.replace(new));
                    }
                    unhook_all(ctx, &mut self.asked, out);
                }
                if !ctx.released {
                    send_move(&mut self.asked, &mut self.missed, &session, &expr, out);
                }
            }
            Asked::Unhook { script, .. } => {
                if reply.is_err()
                    && let Some(i) = at
                {
                    self.contexts[i].hooks.unremoved.push(script);
                }
            }
            Asked::Move { session } => {
                if !confirmed(&reply) {
                    self.missed.insert(session);
                }
            }
            Asked::Release { .. } => {
                if let Some(i) = at {
                    self.contexts[i].release_answer = Some(confirmed(&reply));
                }
            }
        }
    }
}

/// Send a move's expression to one live document.
fn send_move(asked: &mut HashMap<u64, Asked>, missed: &mut BTreeSet<String>, session: &str, expr: &str, out: &mut impl Outbox) {
    match out.ask(EVALUATE, json!({ "expression": expr, "returnByValue": true }), session) {
        Some(id) => {
            asked.insert(id, Asked::Move { session: session.to_string() });
        }
        None => {
            missed.insert(session.to_string());
        }
    }
}

/// Ask a page to remove every hook it should not have any more. One that could not even be asked
/// stays on the list for the next try.
fn unhook_all(ctx: &mut CdpContext, asked: &mut HashMap<u64, Asked>, out: &mut impl Outbox) {
    let scripts = std::mem::take(&mut ctx.hooks.unremoved);
    for script in scripts {
        match out.ask(REMOVE_HOOK, json!({ "identifier": script }), &ctx.session_id) {
            Some(id) => {
                asked.insert(id, Asked::Unhook { session: ctx.session_id.clone(), script });
            }
            None => ctx.hooks.unremoved.push(script),
        }
    }
}

/// Merge one context's counts reply into the table, by max per `(index, "type api")`.
fn merge_counts(counts: &mut BTreeMap<(u32, String), u64>, index: u32, ty: &str, reply: &Value) {
    let Some(obj) = reply.get("result").and_then(|r| r.get("value")).and_then(Value::as_object) else {
        return;
    };
    for (api, key) in cdp::COUNTED_APIS {
        if let Some(n) = obj.get(key).and_then(Value::as_u64) {
            let entry = counts.entry((index, format!("{ty} {api}"))).or_insert(0);
            *entry = (*entry).max(n);
        }
    }
}

/// Whether a context confirmed an evaluate that moves or lets go of its clock: `ok`, or `no-shim`
/// from one with no shim to move. Anything else did not take it - `late` from a page that got a
/// scheduled rate change after its instant (R4-S17), an error, or a reply with no value.
fn confirmed(reply: &Result<Value, ()>) -> bool {
    reply.as_ref().is_ok_and(|r| matches!(r["result"]["value"].as_str(), Some("ok" | "no-shim")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A connection that records what was asked and hands out ids in order. `refuse` makes every
    /// later send fail, the way a connection that is over does.
    #[derive(Default)]
    struct Wire {
        sent: Vec<(u64, String, String, Value)>,
        refuse: bool,
    }

    impl Outbox for Wire {
        fn ask(&mut self, method: &str, params: Value, session: &str) -> Option<u64> {
            if self.refuse {
                return None;
            }
            let id = self.sent.len() as u64 + 1;
            self.sent.push((id, method.to_string(), session.to_string(), params));
            Some(id)
        }
    }

    impl Wire {
        /// The ids of what was sent, by method and session, in order.
        fn ids(&self, method: &str, session: &str) -> Vec<u64> {
            self.sent.iter().filter(|(_, m, s, _)| m == method && s == session).map(|(id, ..)| *id).collect()
        }

        /// What was sent, as `method session` lines, from position `from` on.
        fn lines(&self, from: usize) -> Vec<String> {
            self.sent[from..].iter().map(|(_, m, s, _)| format!("{m} {s}")).collect()
        }
    }

    fn page(session: &str, script: &str) -> CdpContext {
        CdpContext::new(1, session.into(), "page".into(), format!("T-{session}"), Some(script.into()))
    }

    fn worker(session: &str) -> CdpContext {
        CdpContext::new(2, session.into(), "worker".into(), format!("T-{session}"), None)
    }

    fn value(v: &str) -> Result<Value, ()> {
        Ok(json!({ "result": { "type": "string", "value": v } }))
    }

    fn hook(id: &str) -> Result<Value, ()> {
        Ok(json!({ "identifier": id }))
    }

    fn requests(contexts: Vec<CdpContext>) -> Requests {
        let mut r = Requests::default();
        for c in contexts {
            r.push(c);
        }
        r
    }

    /// R4-W5 kept without waiting on anyone: a page gets its move only after its new hook is back,
    /// and then the old hook goes. A worker, with no hook, gets its move at once. Nothing is sent to
    /// a page for its live document before its hook comes back, however long that takes.
    #[test]
    fn a_page_moves_after_its_new_hook_is_back_and_a_worker_moves_at_once() {
        let mut r = requests(vec![page("P", "h0"), worker("W")]);
        let mut wire = Wire::default();
        let waiting = r.start_move("MOVE", "SHIM", &mut wire);
        assert_eq!(wire.lines(0), [format!("{ADD_HOOK} P"), format!("{EVALUATE} W")]);
        assert_eq!(waiting, wire.ids(ADD_HOOK, "P"));
        assert!(!r.answered(&waiting));

        r.on_reply(waiting[0], hook("h1"), &mut wire);
        assert!(r.answered(&waiting));
        assert_eq!(wire.lines(2), [format!("{REMOVE_HOOK} P"), format!("{EVALUATE} P")]);
        assert_eq!(r.contexts()[0].hooks, PageHooks { current: Some("h1".into()), unremoved: vec![] });
    }

    /// The deadline bounds the wait, not what is learned: a hook that comes back after the caller
    /// stopped waiting is kept, and the page gets its move then. A move answered `ok` late is taken.
    #[test]
    fn a_hook_that_comes_back_late_is_kept_and_its_move_follows() {
        let mut r = requests(vec![page("P", "h0")]);
        let mut wire = Wire::default();
        let waiting = r.start_move("MOVE", "SHIM", &mut wire);
        // The caller gave up waiting here. Later:
        r.on_reply(waiting[0], hook("h1"), &mut wire);
        let moved = wire.ids(EVALUATE, "P");
        assert_eq!(moved.len(), 1);
        r.on_reply(moved[0], value("ok"), &mut wire);
        r.settle_moves();
        assert_eq!(r.moves_missed(), 0, "a late ok is a move taken");
        assert_eq!(r.contexts()[0].hooks.current.as_deref(), Some("h1"));
    }

    /// Whether a page missed a move is read from its answers, not from when they came: `late` from
    /// S17, a hook it did not take, and a move with no answer by the end are missed - each page once.
    #[test]
    fn a_missed_move_is_read_from_the_answer() {
        let mut r = requests(vec![page("A", "a0"), page("B", "b0"), worker("C"), worker("D")]);
        let mut wire = Wire::default();
        let waiting = r.start_move("MOVE", "SHIM", &mut wire);
        r.on_reply(waiting[0], hook("a1"), &mut wire);
        r.on_reply(waiting[1], Err(()), &mut wire);
        let a = wire.ids(EVALUATE, "A")[0];
        r.on_reply(a, value("late"), &mut wire);
        let b = wire.ids(EVALUATE, "B")[0];
        r.on_reply(b, value("ok"), &mut wire);
        let c = wire.ids(EVALUATE, "C")[0];
        r.on_reply(c, value("no-shim"), &mut wire);
        // D never answers.
        assert_eq!(r.moves_missed(), 2, "A said late, B kept its old hook");
        r.settle_moves();
        assert_eq!(r.moves_missed(), 3, "D had not answered by the end");
        assert_eq!(r.contexts()[1].hooks.current.as_deref(), Some("b0"), "B keeps the hook it had");
    }

    /// A page that misses two moves is one page that missed a move - the warning is about pages.
    #[test]
    fn a_page_is_counted_once_however_many_moves_it_missed() {
        let mut r = requests(vec![worker("W")]);
        let mut wire = Wire::default();
        r.start_move("ONE", "SHIM", &mut wire);
        r.start_move("TWO", "SHIM", &mut wire);
        for id in wire.ids(EVALUATE, "W") {
            r.on_reply(id, value("late"), &mut wire);
        }
        assert_eq!(r.moves_missed(), 1);
    }

    /// A removal the page did not confirm stays on the list and is tried again with the next move -
    /// and one ADD per move, however many old hooks wait (the review of #84, kept).
    #[test]
    fn a_hook_whose_removal_failed_is_tried_again_at_the_next_move() {
        let mut r = requests(vec![page("P", "h0")]);
        let mut wire = Wire::default();
        let first = r.start_move("ONE", "SHIM", &mut wire);
        r.on_reply(first[0], hook("h1"), &mut wire);
        let remove = wire.ids(REMOVE_HOOK, "P")[0];
        r.on_reply(remove, Err(()), &mut wire);
        assert_eq!(r.contexts()[0].hooks, PageHooks { current: Some("h1".into()), unremoved: vec!["h0".into()] });

        let mark = wire.sent.len();
        let second = r.start_move("TWO", "SHIM", &mut wire);
        r.on_reply(second[0], hook("h2"), &mut wire);
        let lines = wire.lines(mark);
        assert_eq!(lines.iter().filter(|l| l.starts_with(ADD_HOOK)).count(), 1);
        assert_eq!(lines.iter().filter(|l| l.starts_with(REMOVE_HOOK)).count(), 2, "h0 again and h1");
    }

    /// The release wins over a move still waiting for its hook: that move's expression would put the
    /// page back on the session clock after the session, so it is never sent, and the hook that comes
    /// back is removed at once. Until it is, the page counts as not let go.
    #[test]
    fn a_hook_that_comes_back_after_the_release_is_removed_and_its_move_never_sent() {
        let mut r = requests(vec![page("P", "h0")]);
        let mut wire = Wire::default();
        let waiting = r.start_move("MOVE", "SHIM", &mut wire);
        r.start_release("RELEASE", &mut wire);
        let release = wire.ids(EVALUATE, "P");
        assert_eq!(release.len(), 1, "only the release, no move");
        r.on_reply(release[0], value("ok"), &mut wire);
        let unhook_h0 = wire.ids(REMOVE_HOOK, "P")[0];
        r.on_reply(unhook_h0, Ok(json!({})), &mut wire);
        assert_eq!(r.unreleased(), 1, "a hook is still on its way back");
        assert!(!r.release_settled());

        r.on_reply(waiting[0], hook("h1"), &mut wire);
        assert_eq!(wire.ids(EVALUATE, "P").len(), 1, "the move was never sent");
        let (unhook_h1, .., params) = wire.sent.last().unwrap().clone();
        assert_eq!(params, json!({ "identifier": "h1" }), "the hook that came back is the one removed");
        assert_eq!(r.unreleased(), 1, "until its removal is confirmed");
        r.on_reply(unhook_h1, Ok(json!({})), &mut wire);
        assert!(r.release_settled());
        assert_eq!(r.unreleased(), 0);
    }

    /// A page that did not confirm the release, did not answer it, or still has a hook it did not
    /// confirm removed is counted - each once.
    #[test]
    fn the_release_counts_every_page_it_cannot_vouch_for() {
        let mut r = requests(vec![page("A", "a0"), page("B", "b0"), worker("C"), worker("D")]);
        let mut wire = Wire::default();
        r.start_release("RELEASE", &mut wire);
        for s in ["A", "B", "C"] {
            let id = wire.ids(EVALUATE, s)[0];
            r.on_reply(id, value(if s == "C" { "late" } else { "ok" }), &mut wire);
        }
        r.on_reply(wire.ids(REMOVE_HOOK, "A")[0], Ok(json!({})), &mut wire);
        r.on_reply(wire.ids(REMOVE_HOOK, "B")[0], Err(()), &mut wire);
        // D never answers.
        assert_eq!(r.unreleased(), 3, "B kept a hook, C did not confirm, D did not answer");
    }

    /// A busy context is not asked for its counts again while the last request is in flight, and an
    /// answer that comes late still counts - by max, under the index and type it was asked for.
    #[test]
    fn a_busy_context_is_asked_for_its_counts_once_and_a_late_answer_counts() {
        let mut r = requests(vec![page("P", "h0"), worker("W")]);
        let mut wire = Wire::default();
        r.request_counts(&mut wire);
        let w = wire.ids(EVALUATE, "W")[0];
        r.on_reply(w, Ok(json!({ "result": { "value": { "now": 4 } } })), &mut wire);
        r.request_counts(&mut wire);
        assert_eq!(wire.ids(EVALUATE, "P").len(), 1, "P is still answering the first");
        assert_eq!(wire.ids(EVALUATE, "W").len(), 2, "W answered, so it is asked again");
        assert!(!r.counts_settled());
        let p = wire.ids(EVALUATE, "P")[0];
        r.on_reply(p, Ok(json!({ "result": { "value": { "now": 7 } } })), &mut wire);
        let counts = r.into_counts();
        assert_eq!(counts.get(&(1, "page Date.now".to_string())), Some(&7));
        assert_eq!(counts.get(&(2, "worker Date.now".to_string())), Some(&4));
    }

    /// A context that went away takes its requests with it, and they are not missed moves: a page
    /// that is gone has no clock to stand apart. Its late answers are nothing to act on.
    #[test]
    fn a_context_that_went_away_takes_its_requests_and_misses_nothing() {
        let mut r = requests(vec![page("P", "h0"), worker("W")]);
        let mut wire = Wire::default();
        let waiting = r.start_move("MOVE", "SHIM", &mut wire);
        assert!(r.forget("", "T-P"));
        assert!(r.answered(&waiting));
        r.on_reply(waiting[0], hook("h1"), &mut wire);
        assert!(wire.ids(EVALUATE, "P").is_empty(), "nothing is sent to a page that is gone");
        r.settle_moves();
        assert_eq!(r.moves_missed(), 1, "only W, which never answered");
        assert!(!r.forget("nobody", ""), "nothing to drop");
    }

    /// A connection that can no longer send: the move is missed and the release is not confirmed -
    /// said, rather than waited for.
    #[test]
    fn a_request_that_could_not_be_sent_is_a_fact_at_once() {
        let mut r = requests(vec![page("P", "h0"), worker("W")]);
        let mut wire = Wire { refuse: true, ..Wire::default() };
        assert!(r.start_move("MOVE", "SHIM", &mut wire).is_empty());
        assert_eq!(r.moves_missed(), 2);
        r.start_release("RELEASE", &mut wire);
        assert!(r.release_settled());
        assert_eq!(r.unreleased(), 2);
        assert_eq!(r.contexts()[0].hooks.unremoved, ["h0"], "a hook that could not be asked about is kept");
    }

    /// Only `ok`, and `no-shim` from a context with no shim to move, confirm a clock move or a
    /// release. `late` from a page that got a scheduled change after its instant does not (R4-S17),
    /// nor an error or a reply with no value.
    #[test]
    fn only_ok_and_no_shim_confirm_a_clock_move() {
        assert!(confirmed(&value("ok")));
        assert!(confirmed(&value("no-shim")));
        assert!(!confirmed(&value("late")));
        assert!(!confirmed(&Ok(json!({ "result": {} }))));
        assert!(!confirmed(&Err(())));
    }

    /// An answer this table did not ask for - a reply to a call that timed out, a resume nobody
    /// waits on - changes nothing.
    #[test]
    fn an_answer_nobody_asked_for_changes_nothing() {
        let mut r = requests(vec![page("P", "h0")]);
        let mut wire = Wire::default();
        r.on_reply(99, hook("stranger"), &mut wire);
        assert!(wire.sent.is_empty());
        assert_eq!(r.contexts()[0].hooks.current.as_deref(), Some("h0"));
    }
}
