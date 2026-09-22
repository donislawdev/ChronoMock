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

use serde_json::json;

use crate::cdp;
use crate::cdp_audit::context_index_for;

/// One shimmed JS context of a Chromium target: the coverage unit of a CDP session (rule 4 - never
/// summed across contexts).
pub(crate) struct CdpContext {
    pub(crate) index: u32,
    pub(crate) session_id: String,
    pub(crate) ty: String,
    /// The CDP targetId, kept because `Target.targetDestroyed` names a target, not a session.
    pub(crate) target_id: String,
}

/// The clock origin a shim is built from: fake start, real start (both Unix-epoch ms) and the rate.
pub(crate) type ShimOrigin = (i64, i64, i64);

/// How many contexts one attacher will shim over its lifetime. The pid registry has the same shape
/// of ceiling (256 slots, `coverage.pid_registry_full`), for the same reason: a family that fans
/// out without bound must not grow the audit without bound. A context past this is counted in
/// `overflow` and not shimmed, and the caller says so (untouchable rule 4).
pub(crate) const MAX_CONTEXTS: usize = 256;

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

pub(crate) struct Attacher {
    client: cdp::CdpClient,
    port: u16,
    /// Who we still TALK to - polling or broadcasting to a dead session costs the full read
    /// deadline inside the caller's loop.
    contexts: Vec<CdpContext>,
    /// Who this attacher ever COVERED, in attach order, append-only: the audit is a record of what
    /// happened, not of what is still open, so a context that reloaded or closed keeps its evidence
    /// (R2-W1). Once per CONTEXT rather than once per attach.
    seen: Vec<u32>,
    counts: BTreeMap<(u32, String), u64>,
    failed: usize,
    overflow: usize,
    /// targetId -> context index, so a re-attached context keeps the identity it already had.
    index_by_target: HashMap<String, u32>,
}

impl Attacher {
    /// Connect to the browser endpoint on a loopback port and arm auto-attach, so every page and
    /// worker the browser creates from now on arrives as an `attachedToTarget` event, paused until
    /// the shim is in. The pages that ALREADY exist are the caller's next call.
    pub(crate) fn connect(port: u16) -> io::Result<Attacher> {
        let mut client = cdp::CdpClient::connect_to_port("127.0.0.1", port)?;
        client.call(
            "Target.setAutoAttach",
            json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true }),
            None,
        )?;
        Ok(Attacher {
            client,
            port,
            contexts: Vec::new(),
            seen: Vec::new(),
            counts: BTreeMap::new(),
            failed: 0,
            overflow: 0,
            index_by_target: HashMap::new(),
        })
    }

    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    /// Attach to every shimmable target the browser already has - the pages an embedded engine
    /// opened before the session found its port. Whether auto-attach reaches existing targets is a
    /// question this tool has never measured (docs/09 section 2), so this asks for them by name and
    /// stays idempotent: a target the index map already knows is skipped, whichever road it came by.
    /// Returns how many were newly shimmed.
    pub(crate) fn attach_existing(&mut self, origin: ShimOrigin, next_index: &mut u32) -> io::Result<usize> {
        let reply = self.client.call("Target.getTargets", json!({}), None)?;
        let mut attached = 0;
        for (tid, ty) in new_targets(&reply, &self.index_by_target) {
            let Ok(reply) = self.client.call("Target.attachToTarget", json!({ "targetId": tid, "flatten": true }), None)
            else {
                continue;
            };
            let sid = reply["sessionId"].as_str().unwrap_or("").to_string();
            if sid.is_empty() {
                continue;
            }
            self.shim(sid, ty, tid, origin, next_index);
            attached += 1;
        }
        Ok(attached)
    }

    /// One turn: poll the connection (bounded by the client's poll interval) and act on what came.
    pub(crate) fn pump(&mut self, origin: ShimOrigin, next_index: &mut u32) -> Pumped {
        match self.client.poll() {
            Ok(Some(cdp::Msg::Event { method, params, .. })) if method == "Target.attachedToTarget" => {
                let sid = params["sessionId"].as_str().unwrap_or("").to_string();
                let ty = params["targetInfo"]["type"].as_str().unwrap_or("").to_string();
                let tid = params["targetInfo"]["targetId"].as_str().unwrap_or("").to_string();
                if sid.is_empty() || !cdp::is_shimmable(&ty) {
                    return Pumped::Idle;
                }
                self.shim(sid, ty, tid, origin, next_index);
                Pumped::Attached
            }
            // A context that went away - a reload, a closed window, a recycled worker. Dropped from
            // the live list only: its counts stay in `counts` and its index in `seen`.
            Ok(Some(cdp::Msg::Event { method, params, .. }))
                if method == "Target.detachedFromTarget" || method == "Target.targetDestroyed" =>
            {
                let sid = params["sessionId"].as_str().unwrap_or("");
                let tid = params["targetId"].as_str().unwrap_or("");
                let before = self.contexts.len();
                self.contexts.retain(|c| {
                    let gone = (!sid.is_empty() && c.session_id == sid) || (!tid.is_empty() && c.target_id == tid);
                    !gone
                });
                if self.contexts.len() < before { Pumped::Detached } else { Pumped::Idle }
            }
            Ok(_) => Pumped::Idle,
            Err(_) => Pumped::Closed,
        }
    }

    /// Give a newly attached context its index and its shim. The index is keyed by the CDP targetId,
    /// which Chromium keeps across re-attaches (R3-7). The shim is built from the clock's CURRENT
    /// origin, never the session's initial one.
    fn shim(&mut self, sid: String, ty: String, tid: String, origin: ShimOrigin, next_index: &mut u32) {
        // A target reached twice while it is live - auto-attach and the by-name attach can both
        // deliver the same page - stays one context: the shim itself is idempotent, but a second
        // session on the list would be polled and broadcast to twice.
        if !tid.is_empty() && self.contexts.iter().any(|c| c.target_id == tid) {
            return;
        }
        let index = context_index_for(&tid, &mut self.index_by_target, next_index);
        if past_ceiling(&self.seen, index) {
            self.overflow += 1;
            return;
        }
        let (fake0, real0, mult) = origin;
        let shim = cdp::build_shim(fake0, real0, mult);
        let injected = if cdp::is_worker(&ty) {
            cdp::inject_worker(&mut self.client, &sid, &shim)
        } else {
            cdp::inject_page(&mut self.client, &sid, &shim)
        };
        match injected {
            Ok(()) => {
                if !self.seen.contains(&index) {
                    self.seen.push(index);
                }
                self.contexts.push(CdpContext {
                    index,
                    session_id: sid,
                    // The target named its own context type, and that name becomes a coverage key
                    // in the report and on the wire. Cleaned here, at the one place a context is
                    // built.
                    ty: cdp::sanitise_target_text(&ty),
                    target_id: tid,
                });
            }
            Err(_) => self.failed += 1,
        }
    }

    /// Evaluate a JS expression in every live context (best-effort: a context that just closed
    /// errors and is skipped, so an in-flight update stays honest for the rest).
    pub(crate) fn broadcast(&mut self, expr: &str) {
        for ctx in &self.contexts {
            let _ = self.client.call(
                "Runtime.evaluate",
                json!({ "expression": expr, "returnByValue": true }),
                Some(&ctx.session_id),
            );
        }
    }

    /// Evaluate a JS expression in one live context and return the string it produced, if any.
    /// For a probe reading what a page shows - `document.title` - not for the session.
    pub(crate) fn evaluate_string(&mut self, index: u32, expr: &str) -> Option<String> {
        let sid = self.contexts.iter().find(|c| c.index == index)?.session_id.clone();
        let reply = self
            .client
            .call("Runtime.evaluate", json!({ "expression": expr, "returnByValue": true }), Some(&sid))
            .ok()?;
        reply["result"]["value"].as_str().map(str::to_string)
    }

    /// Read each live context's per-API call counts and merge them (by max, so a peak survives a
    /// reload) into the counts, keyed by `(context index, "type api")`. Returns whether any context
    /// answered - a dead context simply errors and is skipped, so the audit stays honest.
    pub(crate) fn poll_counts(&mut self) -> bool {
        let mut any = false;
        for c in &self.contexts {
            let read = self.client.call(
                "Runtime.evaluate",
                json!({ "expression": cdp::COUNTS_EXPR, "returnByValue": true }),
                Some(&c.session_id),
            );
            if let Ok(v) = read
                && let Some(obj) = v.get("result").and_then(|x| x.get("value")).and_then(serde_json::Value::as_object)
            {
                any = true;
                for (api, key) in [("setInterval", "si"), ("setTimeout", "st"), ("Date.now", "now"), ("performance.now", "perf")] {
                    if let Some(n) = obj.get(key).and_then(serde_json::Value::as_u64) {
                        let entry = self.counts.entry((c.index, format!("{} {}", c.ty, api))).or_insert(0);
                        *entry = (*entry).max(n);
                    }
                }
            }
        }
        any
    }

    /// The live contexts, in attach order.
    pub(crate) fn contexts(&self) -> &[CdpContext] {
        &self.contexts
    }

    /// Every context index this attacher ever shimmed, in attach order.
    pub(crate) fn seen(&self) -> &[u32] {
        &self.seen
    }

    /// The counts, handed over for the end-of-session fold.
    pub(crate) fn into_counts(self) -> BTreeMap<(u32, String), u64> {
        self.counts
    }

    /// How many contexts attached and could not be shimmed.
    pub(crate) fn failed(&self) -> usize {
        self.failed
    }

    /// How many contexts were refused past [`MAX_CONTEXTS`].
    pub(crate) fn overflow(&self) -> usize {
        self.overflow
    }
}

/// Whether a context with this index is one too many: the ceiling is on contexts ever seen, so a
/// re-attach of a known context (same index) always gets back in, and only a NEW one past the
/// ceiling is refused.
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
