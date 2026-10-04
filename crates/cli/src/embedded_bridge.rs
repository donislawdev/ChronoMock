//! The bridge: what a native session holds to reach the web pages inside the application it hooked.
//!
//! An application with an embedded web engine (WebView2, Qt WebEngine) runs its pages in a process
//! the native hook does not enter, so those pages read the real clock while the host reads the
//! session's (slice A named them, slice B built the pieces, this is slice C - docs/09 section 12).
//! The session asks the engine to open a local debugging port through two environment variables,
//! a thread keeps looking for that port among the family's listeners, and every page and worker
//! behind it gets the same JS shim a Chromium session installs - built from the HOST's clock, so
//! there is one anchor and one rate for the whole application.
//!
//! One value in the session loop rather than seven variables, for the reason `Attacher` exists. The
//! bridge owns discovery, the attachers, the shared context index and the account of what was
//! covered. It does not own the clock: every call that needs an origin is handed one read off the
//! native session at that moment. And nothing network-bound runs on the session thread - connecting
//! to a found endpoint, which can take twenty seconds when an engine has gone quiet, happens on a
//! short-lived thread of its own, and the loop keeps its heartbeat (docs/09 section 12.17).

use std::collections::{BTreeMap, HashSet};
use std::io;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use chrono_mech::{ModuleProbe, UncoveredChild};
use chrono_proto::{ReachedEngine, TargetSpec};

use crate::cdp;
use crate::cdp_attach::{Attacher, AttacherOutcome, Pumped, ShimOrigin};
use crate::cdp_audit::KEY_CLOCK_MOVE_MISSED;
use crate::cdp_clock::{cdp_release_expr, cdp_set_expr, drift_ms};
use crate::cdp_discover::{Discovered, Discovery, Notice};
use crate::embedded::engine_env;
use crate::output::diag;
use crate::policy_session::{PolicySession, KEY_NAME_MISMATCH};
use crate::unhooked_tree::UnhookedTree;

/// How long a quiet engine socket may block one turn of the session loop, and how long a call to an
/// engine waits for its reply. The client's own defaults (500 ms and 10 s) suit a loop that drives
/// nothing else - this loop polls children every 100 ms and beats a heartbeat every second.
const POLL: Duration = Duration::from_millis(10);
const CALL: Duration = Duration::from_secs(2);

/// Past this much fake drift between what the pages hold and what the host reads, the pages are
/// re-anchored to the host (docs/09 section 12.5). At x60 that is 17 ms of real drift - below what
/// the two clock bases disagree by in ordinary running, so a re-anchor happens across a sleep or a
/// clock correction and not otherwise.
pub(crate) const DRIFT_MS: i64 = 1_000;

/// A DevTools endpoint was reached inside the application: a debugging port stands open in it for
/// as long as its engine runs, and the port is named in `session_verdict.engines`.
pub(crate) const KEY_DEBUG_PORT_OPEN: &str = "embedded.debug_port_open";
/// The pages of the application ran on the session clock through its engine's debugging port - said
/// in place of the strong claim that they read the real clock, which the reached pages refute.
pub(crate) const KEY_WEB_ENGINE_REACHED: &str = "embedded.web_engine_reached";
/// The strong claim slice A makes when a renderer ran without the hook, replaced by the one above.
const KEY_WEB_ENGINE_UNCOVERED: &str = "embedded.web_engine_uncovered";
/// An endpoint answered as DevTools and could not be attached to.
pub(crate) const KEY_ENGINE_UNREACHABLE: &str = "embedded.engine_unreachable";
/// The session could not look for engines at all: no port to reserve, no thread, no table.
pub(crate) const KEY_DISCOVERY_UNAVAILABLE: &str = "embedded.discovery_unavailable";
/// The port reserved for a Qt engine was held by something else when the engine would have bound it.
pub(crate) const KEY_QT_PORT_TAKEN: &str = "embedded.qt_port_taken";
/// The pages read the host machine's time zone, not the session's (ADR-8) - said when they differ.
pub(crate) const KEY_ZONE_IS_HOST: &str = "embedded.zone_is_host";
/// A WebView2 policy value in the registry was hidden by the session's variable for its duration.
pub(crate) const KEY_REGISTRY_ARGUMENTS_HIDDEN: &str = "embedded.registry_arguments_hidden";
/// A page still open when the session ended did not confirm it was let go, so it may keep the session
/// clock until it is reloaded or closed.
const KEY_PAGES_NOT_RELEASED: &str = "embedded.pages_not_released";
/// The application loaded WebView2 and the session never reached its web engine, for whatever reason
/// no other key names - so its pages may have run on the real clock (docs/09 section 12.12).
pub(crate) const KEY_WEBVIEW2_NOT_REACHED: &str = "embedded.webview2_not_reached";
/// Said beside the key above when the application's token is elevated: WebView2 ignores the
/// environment variable the session reaches its engine through for an elevated host (Microsoft Learn,
/// "Develop secure WebView2 apps"), which is the one reason the channel cannot do its work there.
pub(crate) const KEY_ELEVATED_HOST: &str = "embedded.elevated_host";

/// The library a WebView2 client loads into its own process when it creates an environment: the
/// application's own web view, as opposed to the processes of the engine it starts. Its presence in a
/// process of the family is what says the application uses WebView2 at all.
const WEBVIEW2_CLIENT_LIBRARY: &str = "EmbeddedBrowserWebView.dll";

/// How often the family is asked whether it has loaded WebView2. A look at a process's modules costs a
/// millisecond or two and the answer only ever turns from no to yes, so once a second is plenty.
const LOOK_EVERY: Duration = Duration::from_secs(1);

/// A process of the family seen with the WebView2 client library loaded, with what its token said and
/// the file it was started from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WebView2Host {
    pub(crate) pid: u32,
    /// `None` when the token could not be read - never taken for "not elevated".
    pub(crate) elevated: Option<bool>,
    /// The file name of its executable, `None` when it could not be asked.
    pub(crate) image: Option<String>,
}

/// What the closing words about an unreached WebView2 depend on beyond the host itself: whether the
/// channel was on, whether this core (and so the application) is elevated, whether the tester asked for
/// the registry value, and the name it was written under (docs/09 section 12.19).
#[derive(Debug, Clone, Default)]
pub(crate) struct Reach {
    /// The core's own token is elevated. An application started from it is too.
    pub(crate) elevated_core: bool,
    /// The tester asked for the registry value.
    opted_in: bool,
    /// The name the value was written under, while this session wrote one.
    written_name: Option<String>,
}

impl Reach {
    fn of(elevated_core: bool) -> Reach {
        Reach { elevated_core, ..Reach::default() }
    }
}

/// The warnings for an application that loaded WebView2 while the session never reached a web engine
/// in it. Said only when all three hold: a host was seen, no engine was reached, and no other key of
/// the channel already names why (an endpoint that answered and could not be attached to, or a
/// discovery that could not look) - a second line for the same fact would only repeat it.
///
/// Two lines may ride beside the first. The elevated one, when the token said elevated, the channel
/// was on and the tester did NOT ask for the registry value: it is the advice that value answers, and
/// advice for an option already taken would be wrong, as would advice to give up administrator rights
/// to a tester who turned the channel off. The mismatch one, when the value was written under one name
/// and WebView2 was loaded by a program of another.
///
/// Pure, so each combination is tested without a session.
fn unreached_warnings(host: Option<&WebView2Host>, reached: bool, explained: bool, channel_on: bool, reach: &Reach) -> Vec<String> {
    let Some(host) = host else {
        return Vec::new();
    };
    if reached || explained {
        return Vec::new();
    }
    let mut out = vec![KEY_WEBVIEW2_NOT_REACHED.to_string()];
    if host.elevated == Some(true) && channel_on && !reach.opted_in {
        out.push(KEY_ELEVATED_HOST.to_string());
    }
    if let (Some(written), Some(image)) = (&reach.written_name, &host.image)
        && written.to_lowercase() != image.to_lowercase()
    {
        out.push(KEY_NAME_MISMATCH.to_string());
    }
    out
}

/// Whether a registry value the tester put in is hidden by the session's variable. Only for an
/// application that is not elevated: an elevated one ignores the variable and honours the machine
/// registry (Microsoft Learn, "Develop secure WebView2 apps"), so nothing is hidden from it, and the
/// registry is not even asked. Pure over the question, so the claim is tested without a machine that
/// happens to have such a value.
fn hidden_by_variable(elevated: bool, present: impl FnOnce() -> bool) -> bool {
    !elevated && present()
}

/// The three questions put to a process when the family is looked at for WebView2: whether it has the
/// client library loaded, whether its token is elevated, and the file it was started from. A struct,
/// so a test hands in three answers of its own making and the look still has one parameter for them.
struct Probes<H, E, I> {
    has: H,
    elevated: E,
    image: I,
}

/// The questions as the machine answers them.
type MachineProbes = Probes<fn(u32, &str) -> ModuleProbe, fn(u32) -> Option<bool>, fn(u32) -> Option<String>>;

impl Probes<(), (), ()> {
    fn machine() -> MachineProbes {
        Probes {
            has: chrono_mech::process_has_module,
            elevated: chrono_mech::process_elevated,
            image: chrono_mech::process_image_name,
        }
    }
}

/// What a native start needs from the channel before the target launches: the variables that make
/// an engine open its port, the port reserved for a Qt engine, and what there already is to say.
pub(crate) struct Launch {
    pub(crate) env: Vec<(String, String)>,
    qt_port: Option<u16>,
    enabled: bool,
    /// No loopback port could be reserved, so no variable was set and no engine will be found.
    unavailable: bool,
    /// A registry policy value the session's variable hides is there (docs/09 section 12.10).
    pub(crate) registry_hidden: bool,
    /// What the closing words about WebView2 depend on beyond the channel itself.
    reach: Reach,
}

impl Launch {
    /// The channel turned off: nothing in the environment, nothing to ask of the engine. What every
    /// session gets under `--no-embedded`, and what a Chromium target would get if it came this way.
    /// The bridge still looks at which program loaded WebView2, because a tester who left the pages
    /// alone has not asked to be told nothing about them.
    pub(crate) fn off() -> Launch {
        Launch {
            env: Vec::new(),
            qt_port: None,
            enabled: false,
            unavailable: false,
            registry_hidden: false,
            reach: Reach::default(),
        }
    }

    /// Prepare the channel for a target: reserve the port a Qt engine needs telling, build the two
    /// variables on top of what the tester already has, and look whether a registry policy is about
    /// to be hidden. The reads of the machine happen here, the composition in `compose`.
    ///
    /// `elevated` is the core's own token. An elevated application ignores the variables (Microsoft
    /// Learn, "Develop secure WebView2 apps") and honours the machine registry, so nothing is hidden
    /// from it by them and the policy value is not looked for under that claim.
    pub(crate) fn for_target(target: &TargetSpec, elevated: bool) -> Launch {
        if !target.embedded {
            return Launch { reach: Reach::of(elevated), ..Launch::off() };
        }
        let exe_name = std::path::Path::new(&target.path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let qt_port = cdp::free_loopback_port().ok();
        let registry_hidden = hidden_by_variable(elevated, || chrono_mech::webview2_arguments_policy_present(&exe_name));
        Launch { reach: Reach::of(elevated), ..Launch::compose(&chrono_mech::current_environment(), qt_port, registry_hidden) }
    }

    /// Say what the registry value came to, so the closing words can be chosen: whether the tester asked
    /// for it and the name it was written under.
    pub(crate) fn note_policy(&mut self, policy: &PolicySession) {
        self.reach.opted_in = policy.opted_in();
        self.reach.written_name = policy.written_name().map(str::to_string);
    }

    /// The channel's launch from what the machine said: the tester's environment, the port reserved
    /// for a Qt engine (none when the machine could not hand one out), and whether a registry policy
    /// is about to be hidden. A machine without a port gets no variables at all - half a channel
    /// would find WebView2 and silently never Qt. Pure, so the shapes are tested with an environment
    /// of the test's choosing rather than whatever the tester's machine carries.
    fn compose(base: &[(String, String)], qt_port: Option<u16>, registry_hidden: bool) -> Launch {
        let Some(qt_port) = qt_port else {
            return Launch { enabled: true, unavailable: true, registry_hidden, ..Launch::off() };
        };
        Launch {
            env: engine_env(base, qt_port),
            qt_port: Some(qt_port),
            enabled: true,
            registry_hidden,
            ..Launch::off()
        }
    }
}

/// What the bridge covered, handed over at the end of the session for the family verdict, the
/// per-context coverage events and the session warnings.
pub(crate) struct Outcome {
    pub(crate) seen: Vec<u32>,
    pub(crate) counts: BTreeMap<(u32, String), u64>,
    pub(crate) failed: usize,
    pub(crate) overflow: usize,
    /// Whether any endpoint was attached to at all. Without one there is nothing to judge.
    pub(crate) reached: bool,
    rate_changed: bool,
    pub(crate) engines: Vec<ReachedEngine>,
    warnings: Vec<String>,
    /// A process of the family that had WebView2 loaded, when one was seen at any point of the session.
    webview2: Option<WebView2Host>,
    /// Whether the channel was on, and what else the closing words depend on.
    channel_on: bool,
    reach: Reach,
    /// How many processes of an elevated host's engine the process tree turned up that nobody else
    /// had named (docs/09 section 12.19). Named ones are in the session's list, this is the count.
    pub(crate) tree_found: u32,
}

impl Outcome {
    /// Whether the application used WebView2 and the session never reached its engine, with nothing
    /// else in this outcome already saying why. The family cannot be `works` then: its pages may have
    /// run on the real clock, and a verdict that does not say so would be the audit claiming a channel
    /// it did not reach (untouchable rule 4).
    pub(crate) fn engine_missed(&self) -> bool {
        !self.unreached_warnings().is_empty()
    }

    fn unreached_warnings(&self) -> Vec<String> {
        let explained =
            self.warnings.iter().any(|w| w == KEY_ENGINE_UNREACHABLE || w == KEY_DISCOVERY_UNAVAILABLE);
        unreached_warnings(self.webview2.as_ref(), self.reached, explained, self.channel_on, &self.reach)
    }

    /// The warnings the session verdict carries for this channel, each said only when it happened.
    /// `zone_differs` is whether the session zone is not the host machine's: the pages read the
    /// host's (ADR-8), which is a fact to say only when it makes a difference.
    pub(crate) fn session_warnings(&self, zone_differs: bool) -> Vec<String> {
        let mut out = self.warnings.clone();
        out.extend(self.unreached_warnings());
        let pages = !self.seen.is_empty();
        if self.reached {
            out.push(KEY_DEBUG_PORT_OPEN.to_string());
        }
        if self.overflow > 0 {
            out.push("chromium.context_ceiling_reached".to_string());
        }
        if self.rate_changed && pages {
            out.push("chromium.rate_change_affects_running_timers".to_string());
        }
        if pages && zone_differs {
            out.push(KEY_ZONE_IS_HOST.to_string());
        }
        out
    }

    /// Whether at least one page or worker was put on the session clock.
    pub(crate) fn pages_reached(&self) -> bool {
        !self.seen.is_empty()
    }
}

/// Reconcile slice A's claim with what this channel did: a renderer the hook never entered still
/// makes the strong warning, but once its pages were reached over the debugging port the claim that
/// they read the real clock is false, and the warning that replaces it says what happened instead.
pub(crate) fn reconcile_engine_warnings(warnings: &mut [String], pages_reached: bool) {
    if !pages_reached {
        return;
    }
    for w in warnings.iter_mut() {
        if w == KEY_WEB_ENGINE_UNCOVERED {
            *w = KEY_WEB_ENGINE_REACHED.to_string();
        }
    }
}

pub(crate) struct EmbeddedBridge {
    scale_duration: bool,
    discovery: Option<Discovery>,
    connects_tx: Sender<(Discovered, io::Result<Attacher>)>,
    connects: Receiver<(Discovered, io::Result<Attacher>)>,
    /// Ports a connect thread is working on, so a second `Found` for the same port waits for it.
    connecting: HashSet<u16>,
    attachers: Vec<Attacher>,
    /// What the attachers the engine closed had covered - kept, because the audit is a record of
    /// what happened (R2-W1), and an attacher dropped without it takes its evidence along.
    closed: Vec<AttacherOutcome>,
    /// One counter for every attacher: the index is a context's identity on the wire, and two
    /// attachers minting their own would both call their first page context 1.
    next_index: u32,
    engines: Vec<ReachedEngine>,
    warnings: Vec<String>,
    family: HashSet<u32>,
    /// The pids the process tree under an elevated host showed at its last look. Kept apart from
    /// `family`, which only grows: a pid that has left the tree may be handed to an unrelated process,
    /// and a debugging port that process opens is not the host's.
    tree_family: HashSet<u32>,
    /// The origin the pages hold, for the drift check - the last one broadcast, or the first one a
    /// page was shimmed from.
    pushed: Option<ShimOrigin>,
    rate_changed: bool,
    reached: bool,
    /// Whether the channel is on. The family is asked about WebView2 either way.
    channel_on: bool,
    /// The first process of the family seen with the WebView2 client library loaded.
    host: Option<WebView2Host>,
    last_look: Option<Instant>,
    /// What the closing words depend on beyond the channel: elevation and the registry value.
    reach: Reach,
    /// The engine of an elevated host, found by the process tree once the host is seen.
    tree: UnhookedTree,
}

impl EmbeddedBridge {
    /// Start looking for engines in a family, or stand inert when the channel is off or could not be
    /// set up - saying so in the latter case. Never fails: the session goes on either way.
    pub(crate) fn start(launch: &Launch, family: Vec<u32>, scale_duration: bool) -> EmbeddedBridge {
        let (connects_tx, connects) = mpsc::channel();
        let mut bridge = EmbeddedBridge {
            scale_duration,
            discovery: None,
            connects_tx,
            connects,
            connecting: HashSet::new(),
            attachers: Vec::new(),
            closed: Vec::new(),
            next_index: 0,
            engines: Vec::new(),
            warnings: Vec::new(),
            family: family.iter().copied().collect(),
            tree_family: HashSet::new(),
            pushed: None,
            rate_changed: false,
            reached: false,
            channel_on: launch.enabled,
            host: None,
            last_look: None,
            reach: launch.reach.clone(),
            tree: UnhookedTree::default(),
        };
        if !launch.enabled {
            return bridge;
        }
        if launch.unavailable {
            bridge.warn(KEY_DISCOVERY_UNAVAILABLE);
            return bridge;
        }
        match Discovery::start(family, launch.qt_port) {
            Ok(d) => bridge.discovery = Some(d),
            Err(e) => {
                diag!("chrono core: the engine discovery thread did not start: {e}");
                bridge.warn(KEY_DISCOVERY_UNAVAILABLE);
            }
        }
        bridge
    }

    /// Whether a turn of the loop has anything to pump. An inert bridge, or one still waiting for
    /// its first engine, costs the loop nothing.
    pub(crate) fn is_active(&self) -> bool {
        self.discovery.is_some() || !self.attachers.is_empty() || !self.connecting.is_empty()
    }

    /// The duration rate the pages run at: the host's, which scales only under `scale_duration`.
    pub(crate) fn scale_duration(&self) -> bool {
        self.scale_duration
    }

    /// The family as the session now knows it. Pushed to discovery only when it grew - pids are
    /// only ever added to a family.
    pub(crate) fn family(&mut self, pids: impl IntoIterator<Item = u32>) {
        let before = self.family.len();
        self.family.extend(pids);
        if self.family.len() > before {
            self.push_family();
        }
    }

    /// The family discovery looks in: what the session knows of, and what the tree shows now.
    fn looked_in(&self) -> Vec<u32> {
        self.family.union(&self.tree_family).copied().collect()
    }

    fn push_family(&self) {
        if let Some(d) = &self.discovery {
            d.update_family(self.looked_in());
        }
    }

    /// Ask the family whether any process of it has loaded WebView2, at most once a second and only
    /// until one has. `hosts` are the processes the hook is in - the application itself - and not the
    /// engine's own processes, which carry the engine and not the client library. The hook cannot
    /// answer this for an elevated host, whose engine a system service starts, so it is asked from
    /// outside (docs/09 section 12.12). Asked whether or not the channel is on: a tester who left the
    /// pages alone has not asked the report to say nothing about them.
    pub(crate) fn look_for_webview2(&mut self, hosts: &[u32]) {
        self.look_with(hosts, false, Instant::now(), Probes::machine());
    }

    /// The same look once more as the session closes, whatever the cadence says: what turned up in the
    /// last second counts.
    pub(crate) fn look_for_webview2_last(&mut self, hosts: &[u32]) {
        self.look_with(hosts, true, Instant::now(), Probes::machine());
    }

    /// The look over any three questions, so the cadence, the stopping once found and the order are
    /// tested with answers of the test's making.
    fn look_with(&mut self, hosts: &[u32], forced: bool, now: Instant, probes: Probes<impl Fn(u32, &str) -> ModuleProbe, impl Fn(u32) -> Option<bool>, impl Fn(u32) -> Option<String>>) {
        if self.host.is_some() {
            return;
        }
        if !forced && self.last_look.is_some_and(|t| now.saturating_duration_since(t) < LOOK_EVERY) {
            return;
        }
        self.last_look = Some(now);
        self.host = hosts.iter().find(|&&pid| (probes.has)(pid, WEBVIEW2_CLIENT_LIBRARY) == ModuleProbe::Loaded).map(
            |&pid| WebView2Host { pid, elevated: (probes.elevated)(pid), image: (probes.image)(pid) },
        );
    }

    /// Follow the engine of an elevated host through the process tree, once the host is seen and only
    /// for a core that is itself elevated - an application under an ordinary token is followed by the
    /// hook, and nothing here is needed (docs/09 section 12.19). Returns the processes that nobody had
    /// named, for the session's list of what ran on the real clock. The pids of the whole tree are in the
    /// family discovery looks in while they are in the tree, which is how the debugging port of an engine
    /// a service started is found.
    ///
    /// `known` is every pid the session already accounts for.
    pub(crate) fn follow_host_tree(&mut self, known: &[u32]) -> Vec<UncoveredChild> {
        self.follow_tree(known, false)
    }

    /// The same look once more as the session closes, whatever the cadence says.
    pub(crate) fn follow_host_tree_last(&mut self, known: &[u32]) -> Vec<UncoveredChild> {
        self.follow_tree(known, true)
    }

    fn follow_tree(&mut self, known: &[u32], forced: bool) -> Vec<UncoveredChild> {
        self.follow_tree_with(known, forced, Instant::now(), chrono_mech::descendants_of, chrono_mech::name_unhooked)
    }

    fn follow_tree_with(
        &mut self,
        known: &[u32],
        forced: bool,
        now: Instant,
        tree: impl Fn(u32) -> Result<Vec<(u32, u32)>, String>,
        name: impl Fn(u32, u32) -> UncoveredChild,
    ) -> Vec<UncoveredChild> {
        let Some(host) = self.host.as_ref().map(|h| h.pid).filter(|_| self.reach.elevated_core) else {
            return Vec::new();
        };
        match self.tree.follow_with(host, known, forced, now, tree, name) {
            Some(found) => {
                // The latest look replaces the last one: a process that has left the tree is not looked in.
                if found.pids != self.tree_family {
                    self.tree_family = found.pids;
                    self.push_family();
                }
                found.named
            }
            None => Vec::new(),
        }
    }

    /// The pids the tree showed at its last look, for the session to tell a process that is still in the
    /// tree from one that has left it.
    pub(crate) fn tree_pids(&self) -> &HashSet<u32> {
        &self.tree_family
    }

    /// One turn: take what discovery found and start connecting to it, take what connected and
    /// attach to its pages, and pump every attacher. `origin` is the host's clock now - every page
    /// shimmed this turn starts from it.
    pub(crate) fn pump(&mut self, origin: ShimOrigin) {
        self.take_notices();
        self.take_connects(origin);
        let mut i = 0;
        while i < self.attachers.len() {
            if self.attachers[i].pump(origin, &mut self.next_index) == Pumped::Closed {
                // The engine closed the connection - the application closed its web view, or the
                // engine restarted. Its evidence is handed over before the attacher goes, and a new
                // endpoint for the same pages will be found as a new port.
                let gone = self.attachers.swap_remove(i);
                self.closed.push(gone.into_outcome());
            } else {
                i += 1;
            }
        }
    }

    fn take_notices(&mut self) {
        let Some(discovery) = &self.discovery else {
            return;
        };
        let mut notices = Vec::new();
        while let Some(notice) = discovery.try_recv() {
            notices.push(notice);
        }
        for notice in notices {
            match notice {
                Notice::Found(found) => {
                    // One attacher per port: a socket that left the table for one sweep and came
                    // back is found twice, and a connect already under way is not started again.
                    let known = self.attachers.iter().any(|a| a.port() == found.port)
                        || self.connecting.contains(&found.port);
                    if !known {
                        self.spawn_connect(found);
                    }
                }
                Notice::PortTaken(_) => self.warn(KEY_QT_PORT_TAKEN),
                Notice::Unavailable(why) => {
                    diag!("chrono core: engine discovery stopped: {why}");
                    self.warn(KEY_DISCOVERY_UNAVAILABLE);
                    self.discovery = None;
                }
            }
        }
    }

    /// Connect on a thread of its own: `/json/version` and the WebSocket handshake each wait up to
    /// ten seconds on an engine that has gone quiet, and the session loop cannot stand still for that.
    fn spawn_connect(&mut self, found: Discovered) {
        let tx = self.connects_tx.clone();
        let port = found.port;
        let spawned = thread::Builder::new().name("chrono-attach".into()).spawn(move || {
            let result = Attacher::connect(found.host, found.port, Instant::now() + cdp::CONNECT_DEADLINE);
            let _ = tx.send((found, result));
        });
        match spawned {
            Ok(_) => {
                self.connecting.insert(port);
            }
            Err(e) => {
                diag!("chrono core: cannot start a thread to reach the engine on port {port}: {e}");
                self.warn(KEY_ENGINE_UNREACHABLE);
            }
        }
    }

    fn take_connects(&mut self, origin: ShimOrigin) {
        loop {
            match self.connects.try_recv() {
                Ok((found, Ok(mut attacher))) => {
                    self.connecting.remove(&found.port);
                    // Budgets that keep the loop's cadence, and the probe that keeps a page the hook
                    // already covers from being shimmed a second time.
                    attacher.set_budgets(POLL, CALL);
                    attacher.probe_clock_before_shim();
                    if self.pushed.is_none() {
                        self.pushed = Some(origin);
                    }
                    if let Err(e) = attacher.attach_existing(origin, &mut self.next_index, Instant::now() + CALL) {
                        diag!("chrono core: engine on port {}: {e}", found.port);
                    }
                    self.reached = true;
                    self.engines.push(ReachedEngine { pid: found.pid, port: found.port, browser: found.browser });
                    self.attachers.push(attacher);
                }
                Ok((found, Err(e))) => {
                    self.connecting.remove(&found.port);
                    diag!("chrono core: cannot reach the engine on port {}: {e}", found.port);
                    self.warn(KEY_ENGINE_UNREACHABLE);
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => return,
            }
        }
    }

    /// Ask every live context for its call counts (once a second, like the Chromium session), without
    /// waiting - the answers are taken by the next turns (R4-S10).
    pub(crate) fn request_counts(&mut self) {
        for attacher in &mut self.attachers {
            attacher.request_counts();
        }
    }

    /// Bring the pages back to the host's clock when the two have drifted apart - across a sleep, or
    /// a correction of the system clock (docs/09 section 12.5). The wall alone moves, as in a jump.
    pub(crate) fn resync(&mut self, fresh: ShimOrigin) {
        if self.attachers.is_empty() {
            return;
        }
        if let Some(pushed) = self.pushed
            && drift_ms(pushed, fresh).abs() > DRIFT_MS
        {
            self.move_clock(&cdp_set_expr(fresh), fresh);
            self.pushed = Some(fresh);
        }
    }

    /// The host changed its rate: push the host's new origin and both rates to every page. Pushed as
    /// it is rather than scheduled, as a Chromium session schedules its own (ADR-9 R4/14b): the host
    /// changed at the moment of the command, and a page agrees with it only on the host's own
    /// segment, so it takes the change when the change reaches it, and its wall steps by that delay
    /// times the change in rate, as the host's moment says it should.
    pub(crate) fn set_multiplier(&mut self, fresh: ShimOrigin) {
        if !self.attachers.is_empty() {
            self.rate_changed = true;
        }
        self.move_clock(&cdp_set_expr(fresh), fresh);
        self.pushed = Some(fresh);
    }

    /// The host jumped its wall: push the new origin. The rates in it are the host's, unchanged.
    pub(crate) fn jump(&mut self, fresh: ShimOrigin) {
        self.move_clock(&cdp_set_expr(fresh), fresh);
        self.pushed = Some(fresh);
    }

    /// Let every page go before the connections close, the way the hook lets the host go: the wall
    /// back on the real clock and the duration axis on from where it stands at rate 1. For a session
    /// whose application outlives it - the caller decides that, a page of an application that has
    /// exited is gone and has nothing to let go of.
    ///
    /// Measured before this existed (2026-09-24, WebView2 host at x60): the host went back to the real
    /// clock at `end` and its page stayed on the session date, running on at the session rate for as
    /// long as it lived - also with no opt-in at all, because this channel is on by default. A page
    /// that does not confirm is named in the report rather than assumed let go (rule 6).
    ///
    /// Waits until `end_by` at the latest - the same deadline as [`EmbeddedBridge::finish`], because
    /// a GUI gives the core two seconds after `end` and both have to fit in them (ADR-20).
    pub(crate) fn release_pages(&mut self, origin: ShimOrigin, end_by: Instant) {
        let expr = cdp_release_expr();
        let next_index = &mut self.next_index;
        let unconfirmed: u32 = self.attachers.iter_mut().map(|a| a.release(&expr, origin, next_index, end_by)).sum();
        if unconfirmed > 0 {
            self.warn(KEY_PAGES_NOT_RELEASED);
        }
    }

    /// Every page of every engine onto `origin`: new-document hooks renewed, live documents told. A
    /// page that does not take the move stands apart from the host until the next jump - the resync
    /// does not see it, because it measures the drift of what was pushed - so it is said at the end
    /// (rule 6, [`EmbeddedBridge::finish`]).
    fn move_clock(&mut self, expr: &str, origin: ShimOrigin) {
        let next_index = &mut self.next_index;
        for attacher in &mut self.attachers {
            attacher.move_clock(expr, origin, next_index);
        }
    }

    fn warn(&mut self, key: &str) {
        if !self.warnings.iter().any(|w| w == key) {
            self.warnings.push(key.to_string());
        }
    }

    /// Hand over what the channel covered, after the last counts and every clock move a page has not
    /// answered by `end_by` (taken as missed). Closes every connection. A page still open keeps its
    /// shim, which is why an application that outlives the session gets `release_pages` first. A
    /// document loaded after this starts without the shim: its registration dies with the connection
    /// (measured 2026-09-24, a reload after the session came back on the real clock).
    pub(crate) fn finish(mut self, origin: ShimOrigin, end_by: Instant) -> Outcome {
        let next_index = &mut self.next_index;
        for attacher in &mut self.attachers {
            attacher.settle(origin, next_index, end_by);
        }
        let missed = self.closed.iter().map(|c| c.moves_missed).sum::<usize>()
            + self.attachers.iter().map(Attacher::moves_missed).sum::<usize>();
        if missed > 0 {
            self.warn(KEY_CLOCK_MOVE_MISSED);
        }
        let mut outcome = Outcome {
            seen: Vec::new(),
            counts: BTreeMap::new(),
            failed: 0,
            overflow: 0,
            reached: self.reached,
            rate_changed: self.rate_changed,
            engines: self.engines,
            warnings: self.warnings,
            webview2: self.host,
            channel_on: self.channel_on,
            reach: self.reach,
            tree_found: self.tree.total(),
        };
        let live = self.attachers.into_iter().map(|a| {
            if a.native() > 0 {
                diag!("chrono core: {} context(s) on port {} were already on the session clock natively", a.native(), a.port());
            }
            a.into_outcome()
        });
        for part in self.closed.into_iter().chain(live) {
            outcome.seen.extend(part.seen);
            outcome.counts.extend(part.counts);
            outcome.failed += part.failed;
            outcome.overflow += part.overflow;
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(embedded: bool) -> TargetSpec {
        TargetSpec {
            path: "C:/apps/host.exe".into(),
            args: Vec::new(),
            cwd: None,
            embedded,
            elevated_embedded: false,
            console: Default::default(),
        }
    }

    /// The opt-out means no variable in the environment and nothing to look for. On, the machine is
    /// asked for a port - and what the launch is made of is judged on `compose`, below, with an
    /// environment of the test's choosing: this machine's own may already carry a debugging switch,
    /// which `engine_env` honours by adding nothing, and a test that read it would fail on exactly
    /// the tester's machine it is meant to describe.
    #[test]
    fn the_launch_reads_the_machine_only_when_the_channel_is_on() {
        let off = Launch::for_target(&target(false), false);
        assert!(off.env.is_empty());
        assert!(!off.enabled);
        assert!(!off.unavailable);

        let on = Launch::for_target(&target(true), false);
        assert!(on.enabled);
        assert!(!on.unavailable, "this machine hands out loopback ports");
        assert!(on.qt_port.is_some());
    }

    /// The core's own token is carried into the launch, channel on or off, because the closing words and
    /// the tree both depend on it. And an elevated application ignores the variables, so no registry
    /// value is claimed hidden by them - whatever this machine's registry holds.
    #[test]
    fn the_launch_carries_the_cores_elevation_and_claims_nothing_hidden_for_an_elevated_one() {
        for embedded in [false, true] {
            assert!(Launch::for_target(&target(embedded), true).reach.elevated_core);
            assert!(!Launch::for_target(&target(embedded), false).reach.elevated_core);
        }
        assert!(!Launch::for_target(&target(true), true).registry_hidden);
    }

    /// A registry value is claimed hidden by the variable only for an application that is not elevated,
    /// and for an elevated one the registry is not even asked.
    #[test]
    fn a_registry_value_is_hidden_by_the_variable_only_for_an_application_that_is_not_elevated() {
        use std::cell::Cell;
        let asked = Cell::new(0);
        let present = || {
            asked.set(asked.get() + 1);
            true
        };
        assert!(hidden_by_variable(false, present));
        assert_eq!(asked.get(), 1);
        assert!(!hidden_by_variable(true, present), "elevated: the variable is ignored, so nothing is hidden");
        assert_eq!(asked.get(), 1, "and the registry was not asked");
        assert!(!hidden_by_variable(false, || false), "no value, nothing hidden");
    }

    /// What the tester asked for and what was written are carried from the policy to the launch.
    #[test]
    fn the_launch_notes_what_the_policy_did() {
        let mut launch = Launch::for_target(&target(true), true);
        launch.note_policy(&PolicySession::none(true, Some("app.exe")));
        assert!(launch.reach.opted_in);
        assert_eq!(launch.reach.written_name.as_deref(), Some("app.exe"));
        launch.note_policy(&PolicySession::none(false, None));
        assert!(!launch.reach.opted_in);
        assert_eq!(launch.reach.written_name, None);
    }

    /// From a bare environment the two variables are there - the WebView2 one asking for an
    /// ephemeral port, the Qt one naming the port that was reserved for it. Without a port there is
    /// no variable at all, and the launch says the channel is unavailable. The registry finding rides
    /// through untouched either way.
    #[test]
    fn the_launch_is_composed_from_the_environment_and_the_reserved_port() {
        let on = Launch::compose(&[], Some(45_001), false);
        assert!(on.enabled && !on.unavailable);
        assert_eq!(on.qt_port, Some(45_001));
        let names: Vec<&str> = on.env.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS"), "{names:?}");
        assert!(names.contains(&"QTWEBENGINE_REMOTE_DEBUGGING"), "{names:?}");
        let qt = on.env.iter().find(|(n, _)| n == "QTWEBENGINE_REMOTE_DEBUGGING").map(|(_, v)| v.clone()).unwrap();
        assert_eq!(qt, "127.0.0.1:45001", "the Qt variable names the reserved port");

        let no_port = Launch::compose(&[], None, true);
        assert!(no_port.enabled && no_port.unavailable);
        assert!(no_port.env.is_empty(), "half a channel would find WebView2 and never Qt");
        assert!(no_port.registry_hidden);
    }

    /// Slice A's strong claim - the pages read the real clock - is refuted once the pages were
    /// reached, and the warning that replaces it says what happened instead. With no page reached
    /// the claim stands. The weak claim and everything else is left alone either way.
    #[test]
    fn the_strong_engine_warning_gives_way_once_the_pages_were_reached() {
        let mut reached = vec!["inheritance.children_uncovered".to_string(), KEY_WEB_ENGINE_UNCOVERED.to_string()];
        reconcile_engine_warnings(&mut reached, true);
        assert_eq!(reached, vec!["inheritance.children_uncovered", KEY_WEB_ENGINE_REACHED]);

        let mut unreached = vec![KEY_WEB_ENGINE_UNCOVERED.to_string()];
        reconcile_engine_warnings(&mut unreached, false);
        assert_eq!(unreached, vec![KEY_WEB_ENGINE_UNCOVERED]);

        let mut weak = vec!["embedded.web_engine_processes_uncovered".to_string()];
        reconcile_engine_warnings(&mut weak, true);
        assert_eq!(weak, vec!["embedded.web_engine_processes_uncovered"]);
    }

    fn outcome(reached: bool, seen: &[u32], overflow: usize, rate_changed: bool, warnings: &[&str]) -> Outcome {
        Outcome {
            seen: seen.to_vec(),
            counts: BTreeMap::new(),
            failed: 0,
            overflow,
            reached,
            rate_changed,
            engines: Vec::new(),
            warnings: warnings.iter().map(|w| w.to_string()).collect(),
            webview2: None,
            channel_on: true,
            reach: Reach::default(),
            tree_found: 0,
        }
    }

    /// Each warning only when it happened: the open port once an engine was reached, the ceiling
    /// once a page was refused past it, the running-timers caveat once the rate changed WITH pages
    /// to feel it, the zone once there are pages AND the zones differ. What the bridge noted along
    /// the way (an unreachable engine, say) comes first, in the order it was noted.
    #[test]
    fn the_session_warnings_say_only_what_happened() {
        assert!(outcome(false, &[], 0, false, &[]).session_warnings(true).is_empty());
        assert_eq!(outcome(true, &[], 0, true, &[]).session_warnings(true), vec![KEY_DEBUG_PORT_OPEN]);
        assert_eq!(
            outcome(true, &[1], 1, true, &[KEY_ENGINE_UNREACHABLE]).session_warnings(true),
            vec![
                KEY_ENGINE_UNREACHABLE,
                KEY_DEBUG_PORT_OPEN,
                "chromium.context_ceiling_reached",
                "chromium.rate_change_affects_running_timers",
                KEY_ZONE_IS_HOST,
            ]
        );
        assert_eq!(outcome(true, &[1], 0, false, &[]).session_warnings(false), vec![KEY_DEBUG_PORT_OPEN]);
    }

    fn host(elevated: Option<bool>) -> Option<WebView2Host> {
        Some(WebView2Host { pid: 42, elevated, image: Some("app.exe".into()) })
    }

    fn reach(opted_in: bool, written: Option<&str>) -> Reach {
        Reach { elevated_core: true, opted_in, written_name: written.map(str::to_string) }
    }

    /// Every combination of the facts the claim rests on. No host seen says nothing, reached or not. A
    /// host with an engine reached says nothing. A host with nothing reached and nothing else naming why
    /// says the claim - and the elevated line only when the token SAID elevated: one that could not be
    /// read is not one that is not elevated, and neither is it elevated.
    #[test]
    fn an_unreached_webview2_is_said_only_when_a_host_was_seen_nothing_was_reached_and_nothing_else_says_why() {
        let none = Reach::default();
        assert!(unreached_warnings(None, false, false, true, &none).is_empty());
        assert!(unreached_warnings(None, true, false, true, &none).is_empty());
        assert!(unreached_warnings(host(Some(true)).as_ref(), true, false, true, &none).is_empty());
        assert_eq!(unreached_warnings(host(Some(false)).as_ref(), false, false, true, &none), vec![KEY_WEBVIEW2_NOT_REACHED]);
        assert_eq!(unreached_warnings(host(None).as_ref(), false, false, true, &none), vec![KEY_WEBVIEW2_NOT_REACHED]);
        assert_eq!(
            unreached_warnings(host(Some(true)).as_ref(), false, false, true, &none),
            vec![KEY_WEBVIEW2_NOT_REACHED, KEY_ELEVATED_HOST]
        );
        assert!(unreached_warnings(host(Some(true)).as_ref(), false, true, true, &none).is_empty(), "another key already names why");
    }

    /// The advice to give up administrator rights is for a tester who has not taken the option that
    /// answers it, and for one who left the channel on: with the option taken it would be wrong, and to
    /// a tester who turned the pages off it would be advice to change something they chose.
    #[test]
    fn the_elevated_advice_is_said_only_to_a_tester_who_has_not_taken_the_option_and_left_the_channel_on() {
        let elevated = host(Some(true));
        let said = |channel_on: bool, reach: &Reach| unreached_warnings(elevated.as_ref(), false, false, channel_on, reach);
        assert_eq!(said(true, &reach(false, None)), vec![KEY_WEBVIEW2_NOT_REACHED, KEY_ELEVATED_HOST]);
        assert_eq!(said(true, &reach(true, None)), vec![KEY_WEBVIEW2_NOT_REACHED], "the option is taken: that advice is spent");
        assert_eq!(said(false, &reach(false, None)), vec![KEY_WEBVIEW2_NOT_REACHED], "the channel is off by choice");
        // A host that is not elevated has no use for it either way.
        assert_eq!(
            unreached_warnings(host(Some(false)).as_ref(), false, false, true, &reach(false, None)),
            vec![KEY_WEBVIEW2_NOT_REACHED]
        );
    }

    /// A value written under the name of the program the session started, with WebView2 loaded by a
    /// program of another name: said, because it names the program to start. Said only with both names
    /// known and different, and never on its own - without the unreached claim there is nothing to explain.
    #[test]
    fn a_value_written_for_another_program_than_the_one_that_loaded_webview2_is_named() {
        let loaded_by = |image: Option<&str>| {
            Some(WebView2Host { pid: 42, elevated: Some(true), image: image.map(str::to_string) })
        };
        let said = |h: Option<WebView2Host>, reached: bool, written: Option<&str>| {
            unreached_warnings(h.as_ref(), reached, false, true, &reach(true, written))
        };
        assert_eq!(
            said(loaded_by(Some("child.exe")), false, Some("launcher.exe")),
            vec![KEY_WEBVIEW2_NOT_REACHED, KEY_NAME_MISMATCH]
        );
        assert_eq!(said(loaded_by(Some("APP.EXE")), false, Some("app.exe")), vec![KEY_WEBVIEW2_NOT_REACHED], "case is not a difference");
        assert_eq!(said(loaded_by(None), false, Some("launcher.exe")), vec![KEY_WEBVIEW2_NOT_REACHED], "an unknown name claims nothing");
        assert_eq!(said(loaded_by(Some("child.exe")), false, None), vec![KEY_WEBVIEW2_NOT_REACHED], "nothing written, nothing to compare");
        assert!(said(loaded_by(Some("child.exe")), true, Some("launcher.exe")).is_empty(), "reached: nothing to explain");
    }

    /// The outcome applies that rule to what the bridge noted: an unreachable endpoint and a discovery
    /// that could not look each already explain a miss, a Qt port taken does not (it is about another
    /// engine), and the claim rides the session warnings in front of the open-port line.
    #[test]
    fn the_outcome_misses_its_engine_only_when_a_host_was_seen_and_no_key_already_says_why() {
        let with_host = |reached: bool, warnings: &[&str], seen: Option<WebView2Host>| {
            let mut o = outcome(reached, &[], 0, false, warnings);
            o.webview2 = seen;
            o
        };
        assert!(!outcome(false, &[], 0, false, &[]).engine_missed());
        assert!(with_host(false, &[], host(Some(true))).engine_missed());
        assert!(!with_host(true, &[], host(Some(true))).engine_missed());
        assert!(!with_host(false, &[KEY_ENGINE_UNREACHABLE], host(None)).engine_missed());
        assert!(!with_host(false, &[KEY_DISCOVERY_UNAVAILABLE], host(None)).engine_missed());
        assert!(with_host(false, &[KEY_QT_PORT_TAKEN], host(None)).engine_missed());
        assert_eq!(
            with_host(false, &[], host(Some(true))).session_warnings(false),
            vec![KEY_WEBVIEW2_NOT_REACHED, KEY_ELEVATED_HOST]
        );
    }

    /// A bridge with the channel on and no discovery to start: nothing is reserved and no thread runs.
    fn looking_bridge() -> EmbeddedBridge {
        let launch = Launch { enabled: true, unavailable: true, ..Launch::off() };
        EmbeddedBridge::start(&launch, vec![1], false)
    }

    /// The three answers of a test: which pid has the library, its token, its file. The question about the
    /// library is boxed because it counts how often it is asked, which only a closure can carry.
    type TestProbes<'a> = Probes<Box<dyn Fn(u32, &str) -> ModuleProbe + 'a>, fn(u32) -> Option<bool>, fn(u32) -> Option<String>>;

    fn probes(asked: &std::cell::Cell<u32>, loaded: u32) -> TestProbes<'_> {
        // The token and the file are plain functions of the pid, so they need no capture: the pid that
        // has the library is always 2 where they are looked at, and the tests that read them say so.
        Probes {
            has: Box::new(move |pid: u32, library: &str| {
                asked.set(asked.get() + 1);
                assert_eq!(library, WEBVIEW2_CLIENT_LIBRARY);
                if pid == loaded { ModuleProbe::Loaded } else { ModuleProbe::NotLoaded }
            }),
            elevated: |pid| Some(pid == 2),
            image: |pid| Some(format!("p{pid}.exe")),
        }
    }

    /// The family is asked in order, stopping at the first process that has the library and reading the
    /// token and the file of that one alone - and once one answered, never again. The channel being off
    /// changes nothing about that: a tester who left the pages alone is still owed the claim.
    #[test]
    fn the_family_is_asked_about_webview2_in_order_until_one_answers_whether_or_not_the_channel_is_on() {
        use std::cell::Cell;
        let asked = Cell::new(0u32);
        let t0 = Instant::now();

        let mut off = EmbeddedBridge::start(&Launch::off(), vec![1], false);
        off.look_with(&[1, 2], true, t0, probes(&asked, 2));
        assert_eq!(asked.get(), 2, "a channel that is off still asks");
        assert_eq!(off.host, Some(WebView2Host { pid: 2, elevated: Some(true), image: Some("p2.exe".into()) }));

        asked.set(0);
        let mut on = looking_bridge();
        on.look_with(&[1, 2, 3], false, t0, probes(&asked, 2));
        assert_eq!(asked.get(), 2, "the walk stops at the first process that has it");
        assert_eq!(on.host, Some(WebView2Host { pid: 2, elevated: Some(true), image: Some("p2.exe".into()) }));
        on.look_with(&[1, 2, 3], true, t0 + Duration::from_secs(9), probes(&asked, 2));
        assert_eq!(asked.get(), 2, "once found, even a forced look does not ask again");
    }

    /// A look that found nothing waits out the cadence before the next one, unless it is the closing
    /// look, which ignores it. A process that could not be looked into is not one that has the library.
    #[test]
    fn a_look_that_found_nothing_waits_out_the_cadence_unless_it_is_forced() {
        use std::cell::Cell;
        let asked = Cell::new(0u32);
        let t0 = Instant::now();
        let mut bridge = looking_bridge();
        bridge.look_with(&[1], false, t0, probes(&asked, 99));
        bridge.look_with(&[1], false, t0 + LOOK_EVERY / 2, probes(&asked, 99));
        assert_eq!(asked.get(), 1, "inside the cadence nothing is asked");
        bridge.look_with(&[1], false, t0 + LOOK_EVERY, probes(&asked, 99));
        assert_eq!(asked.get(), 2);
        bridge.look_with(&[1], true, t0 + LOOK_EVERY, probes(&asked, 99));
        assert_eq!(asked.get(), 3, "the closing look ignores the cadence");

        let unknown = Probes {
            has: |_: u32, _: &str| ModuleProbe::Unknown,
            elevated: |_: u32| Some(true),
            image: |_: u32| Some("x.exe".to_string()),
        };
        bridge.look_with(&[1], true, t0 + LOOK_EVERY, unknown);
        assert_eq!(bridge.host, None, "an unknown answer is not a sighting");
    }

    /// A bridge that has seen a host, for the tests of the tree below.
    fn bridge_with_host(elevated_core: bool) -> EmbeddedBridge {
        let launch = Launch { enabled: true, unavailable: true, reach: Reach::of(elevated_core), ..Launch::off() };
        let mut bridge = EmbeddedBridge::start(&launch, vec![1], false);
        bridge.host = host(Some(elevated_core));
        bridge
    }

    fn named(pid: u32, parent: u32) -> UncoveredChild {
        UncoveredChild { pid, parent_pid: parent, image: Some(format!("p{pid}.exe")), command_line: None }
    }

    /// The tree is followed only for a core that is itself elevated and only once a host was seen: an
    /// ordinary application is followed by the hook, and a session with no WebView2 in it owes the tree
    /// nothing. Its pids join the family discovery looks in.
    #[test]
    fn the_tree_is_followed_only_for_an_elevated_core_with_a_host_seen() {
        let t0 = Instant::now();
        let under = |_: u32| Ok(vec![(50, 42), (51, 50)]);

        let mut ordinary = bridge_with_host(false);
        assert!(ordinary.follow_tree_with(&[1], true, t0, under, named).is_empty());
        assert!(!ordinary.looked_in().contains(&50));

        let mut no_host = bridge_with_host(true);
        no_host.host = None;
        assert!(no_host.follow_tree_with(&[1], true, t0, under, named).is_empty());

        let mut elevated = bridge_with_host(true);
        let found = elevated.follow_tree_with(&[1], true, t0, under, named);
        assert_eq!(found.iter().map(|c| c.pid).collect::<Vec<_>>(), [50, 51]);
        let looked_in = elevated.looked_in();
        assert!(looked_in.contains(&50) && looked_in.contains(&51), "discovery looks at the tree now");
        assert_eq!(elevated.tree.total(), 2);
    }

    /// The tree's pids are in the family discovery looks in only while the tree shows them. A process that
    /// has left it may have its pid handed to a stranger, and a debugging port the stranger opens is not
    /// the host's - and a session that runs for hours must not carry every process that ever lived there.
    #[test]
    fn a_process_that_has_left_the_tree_is_no_longer_looked_in() {
        let t0 = Instant::now();
        let mut bridge = bridge_with_host(true);

        bridge.follow_tree_with(&[1], true, t0, |_| Ok(vec![(50, 42), (51, 50)]), named);
        assert!(bridge.looked_in().contains(&50), "in the tree, so looked in");

        bridge.follow_tree_with(&[1], true, t0, |_| Ok(vec![(51, 42)]), named);
        let mut looked_in = bridge.looked_in();
        looked_in.sort_unstable();
        assert_eq!(looked_in, [1, 51], "50 left the tree: the family is the known pid and what the tree shows");
        assert_eq!(bridge.family.len(), 1, "the family that only grows was not given the tree's pids");
        assert!(bridge.tree_pids().contains(&51) && !bridge.tree_pids().contains(&50));
    }
}
