//! What a session does about the machine registry for an application that runs as administrator
//! (docs/09 section 12.19), and what it says about it.
//!
//! The registry work itself is `chrono_mech::policy_value`. This is the session's side of it: whether
//! to do anything at all (the tester asked, the core is elevated, the target is an `.exe`), what to
//! tell the tester about what was and was not done, and the one guarantee that matters - a value this
//! session wrote is taken away before the verdict goes out, and by `Drop` on every other way out.
//!
//! The registry is behind [`Registry`], so every decision here is tested with a registry of the test's
//! making and never with the machine's.

use std::path::Path;

use chrono_mech::{PolicyRemoval, PolicySet, PolicyValue, Recovery};
use chrono_proto::{Event, TargetSpec};

use crate::events::ended_after_launch;
use crate::output::diag;

/// This session set a WebView2 value for the application and took it away again. The whole account
/// of what was done to the machine, said at the end because that is when it is true.
pub(crate) const KEY_REMOVED: &str = "embedded.policy_value_removed";
/// The value could not be taken away. The one line here that says something is STILL set.
pub(crate) const KEY_LEFT: &str = "embedded.policy_value_left";
/// A value an earlier session left behind (it ended abruptly) was removed before this one started.
pub(crate) const KEY_RECOVERED: &str = "embedded.policy_value_recovered";
/// A value an earlier session left behind is still there: this session could not, or was not asked to,
/// remove it.
pub(crate) const KEY_STALE: &str = "embedded.policy_value_stale";
/// A value that is not this session's already stands under the application's name, and was left.
pub(crate) const KEY_FOREIGN: &str = "embedded.policy_value_foreign";
/// The tester asked for the value and it could not be written. The reason is on stderr.
pub(crate) const KEY_NOT_WRITTEN: &str = "embedded.policy_not_written";
/// The value was written for the program the session started, and WebView2 was loaded by another one.
pub(crate) const KEY_NAME_MISMATCH: &str = "embedded.policy_name_mismatch";

/// A value this session wrote, as far as the session needs to know it: something that can be taken
/// away. The real one is `chrono_mech::PolicyValue`, and a test has its own.
pub(crate) trait Removable {
    fn remove(&mut self) -> PolicyRemoval;
}

impl Removable for PolicyValue {
    fn remove(&mut self) -> PolicyRemoval {
        PolicyValue::remove(self)
    }
}

/// What trying to write came to, in the session's terms.
pub(crate) enum Attempt {
    /// Written under this name, and answerable for until it is removed.
    Written(String, Box<dyn Removable>),
    StarHasPort,
    Foreign,
    Failed(String),
}

/// The registry, as far as the session asks of it. Production is [`Machine`].
pub(crate) trait Registry {
    fn recover_stale(&self) -> Recovery;
    fn stale_value_present(&self) -> bool;
    fn set_for_session(&self, exe_name: &str) -> Attempt;
}

/// The machine's registry, through the mechanism layer.
struct Machine;

impl Registry for Machine {
    fn recover_stale(&self) -> Recovery {
        chrono_mech::recover_stale()
    }

    fn stale_value_present(&self) -> bool {
        chrono_mech::stale_value_present()
    }

    fn set_for_session(&self, exe_name: &str) -> Attempt {
        match chrono_mech::set_for_session(exe_name) {
            PolicySet::Written(value) => Attempt::Written(value.name().to_string(), Box::new(value)),
            PolicySet::StarHasPort => Attempt::StarHasPort,
            PolicySet::Foreign => Attempt::Foreign,
            PolicySet::Failed(why) => Attempt::Failed(why),
        }
    }
}

/// The session's dealings with the registry, from before the launch to after the verdict.
pub(crate) struct PolicySession {
    /// The tester asked for the value (and the channel is on to use it).
    opted_in: bool,
    /// The value this session wrote and has not yet removed.
    written: Option<Box<dyn Removable>>,
    /// The name it was written under, kept after the value is gone: the closing look at which program
    /// loaded WebView2 compares against it.
    name: Option<String>,
    /// What is known before the launch and said with the first coverage.
    start_keys: Vec<String>,
    /// What the removal came to, once it has been done.
    end_keys: Option<Vec<String>>,
}

impl PolicySession {
    /// Do what the tester asked about the registry, before the application starts: find what an
    /// earlier session left, and - only when asked, elevated, and for an `.exe` - write the value.
    /// `elevated` is the core's own token, read once by the caller.
    pub(crate) fn start(target: &TargetSpec, elevated: bool) -> PolicySession {
        PolicySession::start_with(&Machine, target, elevated)
    }

    fn start_with(registry: &dyn Registry, target: &TargetSpec, elevated: bool) -> PolicySession {
        let opted_in = target.embedded && target.elevated_embedded;
        let mut session =
            PolicySession { opted_in, written: None, name: None, start_keys: Vec::new(), end_keys: None };
        // The channel off means nothing about WebView2 is asked of the machine, a leftover included.
        if !target.embedded {
            return session;
        }
        session.look_for_leftovers(registry, opted_in && elevated);
        if opted_in {
            session.write(registry, target, elevated);
        }
        session
    }

    /// An earlier session that ended abruptly may have left its value behind. A session that may write
    /// the registry removes it and says so, one that may not only says it is there.
    fn look_for_leftovers(&mut self, registry: &dyn Registry, may_remove: bool) {
        if may_remove {
            let recovery = registry.recover_stale();
            if recovery.removed > 0 {
                self.start_keys.push(KEY_RECOVERED.to_string());
            }
            if recovery.failed > 0 {
                diag!("chrono core: {} value(s) left by an earlier session could not be removed", recovery.failed);
                self.start_keys.push(KEY_STALE.to_string());
            }
        } else if registry.stale_value_present() {
            self.start_keys.push(KEY_STALE.to_string());
        }
    }

    fn write(&mut self, registry: &dyn Registry, target: &TargetSpec, elevated: bool) {
        match unwritable(target, elevated) {
            Some(why) => self.not_written(&why),
            None => {
                // `unwritable` has checked the name, so there is one.
                let name = value_name_for(&target.path).unwrap_or_default();
                match registry.set_for_session(&name) {
                    Attempt::Written(name, guard) => {
                        self.name = Some(name);
                        self.written = Some(guard);
                    }
                    Attempt::StarHasPort => diag!(
                        "chrono core: the machine-wide WebView2 value '*' already opens a debugging port, so none was written for {name}"
                    ),
                    Attempt::Foreign => {
                        diag!("chrono core: a WebView2 value named {name} is already in the machine registry and was left as it is");
                        self.start_keys.push(KEY_FOREIGN.to_string());
                    }
                    Attempt::Failed(why) => self.not_written(&why),
                }
            }
        }
    }

    fn not_written(&mut self, why: &str) {
        diag!("chrono core: no WebView2 value was written: {why}");
        self.start_keys.push(KEY_NOT_WRITTEN.to_string());
    }

    /// A session that did what a test says it did and nothing else: asked or not, and a name it claims
    /// to have written. For the tests of what the bridge says about it.
    #[cfg(test)]
    pub(crate) fn none(opted_in: bool, name: Option<&str>) -> PolicySession {
        PolicySession {
            opted_in,
            written: None,
            name: name.map(str::to_string),
            start_keys: Vec::new(),
            end_keys: None,
        }
    }

    /// Whether the tester asked for the value.
    pub(crate) fn opted_in(&self) -> bool {
        self.opted_in
    }

    /// The name the value was written under, while this session has written one.
    pub(crate) fn written_name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// What was known before the launch, for the first coverage.
    pub(crate) fn start_keys(&self) -> &[String] {
        &self.start_keys
    }

    /// Take the value away and say how that went: `removed` when it is gone, `left` when it is not.
    /// Safe to call again - the account of the first call is what it returns.
    pub(crate) fn finish(&mut self) -> Vec<String> {
        if let Some(done) = &self.end_keys {
            return done.clone();
        }
        let mut keys = Vec::new();
        if let Some(mut guard) = self.written.take() {
            match guard.remove() {
                PolicyRemoval::Removed => keys.push(KEY_REMOVED.to_string()),
                PolicyRemoval::Left(why) => {
                    diag!("chrono core: the WebView2 value could not be removed: {why}");
                    keys.push(KEY_LEFT.to_string());
                }
            }
        }
        self.end_keys = Some(keys.clone());
        keys
    }

    /// `ended` for a start that never became a running session: refused, vanished, or not prepared. The
    /// value this session wrote is taken away first, and one that could not be is said as residue, because
    /// the machine still holds it and nothing else in these answers could tell the tester. The account of
    /// a value that was removed is a plain fact and not residue.
    pub(crate) fn ended_before_running(&mut self) -> Event {
        let left = self.finish().into_iter().filter(|key| key == KEY_LEFT).collect();
        ended_after_launch(left)
    }
}

impl Drop for PolicySession {
    /// Every way out of the session that did not come through `finish` - an early refusal, a launch
    /// that failed, a panic - still takes the value away.
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

/// Why the value cannot be written for this target, or `None` when nothing stands in the way.
fn unwritable(target: &TargetSpec, elevated: bool) -> Option<String> {
    if !elevated {
        return Some("Chrono Mock is not running as administrator".into());
    }
    if value_name_for(&target.path).is_none() {
        return Some(format!(
            "the value is named after the .exe the application is started from, and {} is not one (a script is started through the command interpreter)",
            target.path
        ));
    }
    None
}

fn is_exe(path: &str) -> bool {
    Path::new(path).extension().is_some_and(|e| e.eq_ignore_ascii_case("exe"))
}

fn file_name(path: &str) -> Option<String> {
    Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned())
}

/// The name a value would be written under for this start path, or `None` when none would be: the path
/// is not an `.exe`, or has no file name. The one rule a dry run and a run share about the name.
pub(crate) fn value_name_for(path: &str) -> Option<String> {
    if is_exe(path) { file_name(path) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    /// A registry of the test's making: what it is asked, in order, and what each question answers.
    struct Fake {
        leftovers: Cell<u32>,
        recovery: Recovery,
        outcome: RefCell<Option<Attempt>>,
        asked: RefCell<Vec<String>>,
    }

    impl Fake {
        fn answering(outcome: Attempt) -> Fake {
            Fake { leftovers: Cell::new(0), recovery: Recovery::default(), outcome: RefCell::new(Some(outcome)), asked: RefCell::default() }
        }

        fn empty() -> Fake {
            Fake::answering(Attempt::Foreign)
        }
    }

    impl Registry for Fake {
        fn recover_stale(&self) -> Recovery {
            self.asked.borrow_mut().push("recover".into());
            self.recovery
        }

        fn stale_value_present(&self) -> bool {
            self.asked.borrow_mut().push("stale?".into());
            self.leftovers.get() > 0
        }

        fn set_for_session(&self, exe_name: &str) -> Attempt {
            self.asked.borrow_mut().push(format!("set {exe_name}"));
            self.outcome.borrow_mut().take().expect("asked to write once")
        }
    }

    /// A value whose removal the test scripts, counting the calls.
    struct Scripted {
        result: PolicyRemoval,
        calls: Rc<Cell<u32>>,
    }

    impl Removable for Scripted {
        fn remove(&mut self) -> PolicyRemoval {
            self.calls.set(self.calls.get() + 1);
            self.result.clone()
        }
    }

    fn written(result: PolicyRemoval) -> (Attempt, Rc<Cell<u32>>) {
        let calls = Rc::new(Cell::new(0));
        (Attempt::Written("app.exe".into(), Box::new(Scripted { result, calls: calls.clone() })), calls)
    }

    fn target(path: &str, embedded: bool, elevated_embedded: bool) -> TargetSpec {
        TargetSpec {
            path: path.into(),
            args: Vec::new(),
            cwd: None,
            embedded,
            elevated_embedded,
            keep_background_timers: false,
            gecko_sandbox: true,
            console: Default::default(),
        }
    }

    /// Without the opt-in nothing is written and nothing is removed: the registry is asked only
    /// whether something was left behind, which is a read.
    #[test]
    fn without_the_opt_in_the_registry_is_only_read() {
        let registry = Fake::empty();
        let session = PolicySession::start_with(&registry, &target("C:/apps/app.exe", true, false), true);
        assert!(!session.opted_in());
        assert_eq!(*registry.asked.borrow(), ["stale?"]);
        assert!(session.start_keys().is_empty());
        assert_eq!(session.written_name(), None);
    }

    /// With the channel off nothing is asked of the machine at all, whatever else was said.
    #[test]
    fn with_the_channel_off_the_registry_is_not_asked() {
        let registry = Fake::empty();
        let session = PolicySession::start_with(&registry, &target("C:/apps/app.exe", false, true), true);
        assert!(!session.opted_in(), "the option is for the channel, and the channel is off");
        assert!(registry.asked.borrow().is_empty());
    }

    /// The happy path: recovery first, then the write under the file name, then the removal says so.
    #[test]
    fn a_written_value_is_removed_and_said() {
        let (attempt, calls) = written(PolicyRemoval::Removed);
        let registry = Fake::answering(attempt);
        let mut session = PolicySession::start_with(&registry, &target("C:/apps/app.exe", true, true), true);
        assert!(session.opted_in());
        assert_eq!(*registry.asked.borrow(), ["recover", "set app.exe"], "leftovers first, then this session's own");
        assert_eq!(session.written_name(), Some("app.exe"));
        assert!(session.start_keys().is_empty(), "nothing to say before the launch when all went well");

        assert_eq!(session.finish(), [KEY_REMOVED]);
        assert_eq!(calls.get(), 1);
        assert_eq!(session.finish(), [KEY_REMOVED], "asking again says the same and removes nothing twice");
        assert_eq!(calls.get(), 1);
        drop(session);
        assert_eq!(calls.get(), 1, "and dropping it afterwards does not either");
        assert_eq!(registry.asked.borrow().len(), 2);
    }

    /// A removal that fails is the one thing said loudly, and the guard does not try again behind
    /// the tester's back.
    #[test]
    fn a_value_that_could_not_be_removed_is_left_said_and_not_retried() {
        let (attempt, calls) = written(PolicyRemoval::Left("error 5".into()));
        let mut session =
            PolicySession::start_with(&Fake::answering(attempt), &target("C:/apps/app.exe", true, true), true);
        assert_eq!(session.finish(), [KEY_LEFT]);
        drop(session);
        assert_eq!(calls.get(), 1);
    }

    /// A start that never ran ends with the registry's account: a value that could not be removed makes it
    /// unclean and is named in it, a value that was removed and a start that wrote nothing end clean.
    #[test]
    fn a_start_that_never_ran_ends_unclean_only_when_a_value_is_left() {
        fn ended(of: Event) -> (bool, Vec<String>) {
            match of {
                Event::Ended { clean, residue_keys, .. } => (clean, residue_keys),
                other => panic!("not an ended event: {other:?}"),
            }
        }
        let target = target("C:/apps/app.exe", true, true);

        let (left, _) = written(PolicyRemoval::Left("error 5".into()));
        let mut session = PolicySession::start_with(&Fake::answering(left), &target, true);
        assert_eq!(ended(session.ended_before_running()), (false, vec![KEY_LEFT.to_string()]));

        let (gone, _) = written(PolicyRemoval::Removed);
        let mut session = PolicySession::start_with(&Fake::answering(gone), &target, true);
        assert_eq!(ended(session.ended_before_running()), (true, Vec::new()), "removed is not residue");

        let mut nothing = PolicySession::none(false, None);
        assert_eq!(ended(nothing.ended_before_running()), (true, Vec::new()));
    }

    /// Every way out that does not come through `finish` still takes the value away.
    #[test]
    fn dropping_the_session_removes_the_value() {
        let (attempt, calls) = written(PolicyRemoval::Removed);
        let session =
            PolicySession::start_with(&Fake::answering(attempt), &target("C:/apps/app.exe", true, true), true);
        drop(session);
        assert_eq!(calls.get(), 1);
    }

    /// The four ways a write does not happen, each with the key it is owed and none of them touching
    /// the registry beyond the question that failed.
    #[test]
    fn a_write_that_does_not_happen_says_why() {
        let cases: [(&str, Attempt, bool, &str, &[&str]); 5] = [
            ("foreign", Attempt::Foreign, true, "C:/apps/app.exe", &["recover", "set app.exe"]),
            ("failed", Attempt::Failed("error 5".into()), true, "C:/apps/app.exe", &["recover", "set app.exe"]),
            ("not elevated", Attempt::Foreign, false, "C:/apps/app.exe", &["stale?"]),
            ("a script", Attempt::Foreign, true, "C:/apps/run.bat", &["recover"]),
            ("no exe extension", Attempt::Foreign, true, "C:/apps/app", &["recover"]),
        ];
        let expected_keys = [KEY_FOREIGN, KEY_NOT_WRITTEN, KEY_NOT_WRITTEN, KEY_NOT_WRITTEN, KEY_NOT_WRITTEN];
        for ((label, attempt, elevated, path, asked), key) in cases.into_iter().zip(expected_keys) {
            let registry = Fake::answering(attempt);
            let session = PolicySession::start_with(&registry, &target(path, true, true), elevated);
            assert_eq!(session.start_keys(), [key], "{label}");
            assert_eq!(*registry.asked.borrow(), asked, "{label}");
            assert_eq!(session.written_name(), None, "{label}");
        }
    }

    /// A `*` value that already carries a port needs no value of ours, and says nothing: the port is
    /// the report. It is on stderr so the tester who asked is not left guessing.
    #[test]
    fn a_star_value_with_its_own_port_writes_nothing_and_says_no_key() {
        let session = PolicySession::start_with(
            &Fake::answering(Attempt::StarHasPort),
            &target("C:/apps/app.exe", true, true),
            true,
        );
        assert!(session.start_keys().is_empty());
        assert_eq!(session.written_name(), None);
    }

    /// Leftovers: a session that may write removes them and says so, and says a failure to remove.
    /// One that may not only says they are there.
    #[test]
    fn leftovers_are_removed_by_a_session_that_may_write_and_only_reported_by_one_that_may_not() {
        let mut registry = Fake::empty();
        registry.recovery = Recovery { removed: 2, failed: 0 };
        let session = PolicySession::start_with(&registry, &target("C:/apps/run.bat", true, true), true);
        assert_eq!(&session.start_keys()[..1], [KEY_RECOVERED]);

        let mut registry = Fake::empty();
        registry.recovery = Recovery { removed: 1, failed: 1 };
        let session = PolicySession::start_with(&registry, &target("C:/apps/run.bat", true, true), true);
        assert_eq!(&session.start_keys()[..2], [KEY_RECOVERED, KEY_STALE]);

        let registry = Fake::empty();
        registry.leftovers.set(1);
        let session = PolicySession::start_with(&registry, &target("C:/apps/app.exe", true, false), true);
        assert_eq!(session.start_keys(), [KEY_STALE], "no opt-in: said, never removed");
        assert!(!registry.asked.borrow().contains(&"recover".to_string()));

        let registry = Fake::empty();
        registry.leftovers.set(1);
        let session = PolicySession::start_with(&registry, &target("C:/apps/app.exe", true, true), false);
        assert_eq!(session.start_keys()[0], KEY_STALE, "opted in but not elevated cannot remove, only say");
    }

    #[test]
    fn only_an_exe_with_a_file_name_can_name_a_value() {
        assert!(is_exe("C:/apps/app.exe"));
        assert!(is_exe("C:\\apps\\APP.EXE"));
        assert!(!is_exe("C:/apps/app.exe.bat"));
        assert!(!is_exe("C:/apps/app.cmd"));
        assert!(!is_exe("C:/apps/app"));
        assert_eq!(file_name("C:/apps/app.exe").as_deref(), Some("app.exe"));
        assert_eq!(file_name("app.exe").as_deref(), Some("app.exe"));
        assert_eq!(file_name("C:/apps/").as_deref(), Some("apps"), "a trailing separator names the folder, which is no exe");
        assert!(unwritable(&target("C:/apps/", true, true), true).is_some());
        assert_eq!(value_name_for("C:/apps/App.exe").as_deref(), Some("App.exe"), "the name keeps its case, as started");
        assert_eq!(value_name_for("C:/apps/run.bat"), None);
        assert_eq!(value_name_for("C:/apps/"), None);
    }
}
