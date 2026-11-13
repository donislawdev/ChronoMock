using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using ChronoMock.App;
using ChronoMock.App.Calc;
using ChronoMock.Protocol;

namespace ChronoMock.App.Tests;

/// <summary>
/// The rebuilt phases in the states that decide whether they work, shared by the sheet that draws them
/// and the layout guards that hold them to account.
/// </summary>
/// <remarks>
/// 🔴 ONE SOURCE FOR BOTH. The sheet built these models inline, and the guards that now read the phases
/// need exactly the same states. Two copies drift the first time one of them gains an option, and a guard
/// reading a state the sheet no longer draws is a guard nobody can check against a picture.
///
/// The session states themselves stay in <see cref="SessionStates"/>, which the shipped panel's renders
/// share. Only the application is added here, for the reason <see cref="WithTarget"/> gives.
/// </remarks>
internal static class PhaseStates
{
    /// <summary>The setup phase on first contact: nothing chosen, the scenario catalogue loaded.</summary>
    public static SessionViewModel SetupStartup()
        => TestCatalogues.WithShippedScenarios(scenarios => new(new InMemorySessionHistoryStore(), scenarios: scenarios));

    /// <summary>The scenario list as it is before the engine has answered - the panel the moment the window
    /// opens. Nothing is asked for, so the list is still reading (R4/18).</summary>
    public static SessionViewModel SetupScenariosReading() => new(new InMemorySessionHistoryStore());

    /// <summary>The scenario list the engine could not give - a missing engine, said with its path.</summary>
    public static SessionViewModel SetupScenariosFailed()
    {
        var model = new SessionViewModel(
            new InMemorySessionHistoryStore(),
            scenarios: new ScenarioPicker(new PresetLibrary(FakePresetSource.Failing(
                "cannot launch 'C:\\Chrono Mock\\core\\x64\\chrono.exe': The system cannot find the file specified. (calc.launch_failed)"))));
        Assert.True(model.EnsureScenariosAsync().IsCompletedSuccessfully);
        return model;
    }

    /// <summary>A catalogue with nothing to offer this panel - said, where the section used to vanish.</summary>
    public static SessionViewModel SetupScenariosEmpty()
    {
        var model = new SessionViewModel(new InMemorySessionHistoryStore());
        Assert.True(model.EnsureScenariosAsync().IsCompletedSuccessfully);
        return model;
    }

    /// <summary>The shipped scenarios with preset files the engine refused beside them (D2 of R4/18).</summary>
    public static SessionViewModel SetupScenariosWithFilesLeftOut()
    {
        var model = new SessionViewModel(
            new InMemorySessionHistoryStore(), scenarios: TestCatalogues.Picker(TestCatalogues.ShippedWithFilesLeftOut()));
        Assert.True(model.EnsureScenariosAsync().IsCompletedSuccessfully);
        return model;
    }

    /// <summary>The scenario catalogue filtered down to nothing.</summary>
    public static SessionViewModel SetupSearchingForNothing()
    {
        var model = SetupStartup();
        model.ScenarioPicker.Filter = "nothing is called this";
        return model;
    }

    /// <summary>Every option in the merged speed section turned on, the launch fields filled in.</summary>
    public static SessionViewModel SetupWithEveryOption()
    {
        var model = SetupStartup();
        model.ScaleDuration = true;
        model.ScaleQpc = true;
        model.ForceStart = true;
        model.GeckoSandboxOff = true;
        model.TargetArgs = "--seed 7 --headless";
        model.WorkingFolder = @"C:\apps\data";
        return model;
    }

    /// <summary>A window that runs as administrator, with the option that writes the machine registry
    /// ticked: the one state in which the option is live, its cost note shown in place of the line that says
    /// what it needs (docs/09 section 12.19).</summary>
    public static SessionViewModel SetupWithElevatedOption()
    {
        var model = ElevatedSetup();
        model.ReachElevatedEmbedded = true;
        return model;
    }

    /// <summary>An elevated window with the channel the option rides on turned off: the option is greyed
    /// and unticked with it, so the form never claims a session that Start does not run.</summary>
    public static SessionViewModel SetupElevatedWithoutChannel()
    {
        var model = ElevatedSetup();
        model.ReachElevatedEmbedded = true;
        model.ReachEmbedded = false;
        return model;
    }

    /// <summary>An elevated window as it opens: the option is available and not ticked, the channel it rides
    /// on is on. The control for anything that must say nothing about the option until it is asked for.</summary>
    public static SessionViewModel SetupElevatedOptionOff() => ElevatedSetup();

    private static SessionViewModel ElevatedSetup()
        => TestCatalogues.WithShippedScenarios(scenarios => new(new InMemorySessionHistoryStore(), scenarios: scenarios, canReachElevated: true));

    /// <summary>An application chosen and a date that does not exist.</summary>
    /// <summary>A chosen scenario that gave no date (R4-Z2): its sentence under the list, and the footer
    /// refusing Start with the reason rather than starting with the date from before the choice. The engine
    /// refuses at once, so the state is reached the way a click reaches it, with nothing left pending.</summary>
    public static SessionViewModel SetupWithFailedScenario()
    {
        var engine = new FakeCalcEngine(_ => throw new CalcException(
            "chrono calc: step 1 overflows the representable range (calc.overflow)", 1));
        var model = WithTarget(TestCatalogues.WithShippedScenarios(scenarios => new SessionViewModel(
            new InMemorySessionHistoryStore(), calcClient: engine, scenarios: scenarios)));
        model.SelectedScenario = model.ScenarioPicker.Visible[0];
        return model;
    }

    public static SessionViewModel SetupWithBadDate()
    {
        var model = WithTarget(SetupStartup());
        model.Moment.DateText = "2038-02-31";
        return model;
    }

    /// <summary>The setup form as "Set up again" leaves it: filled from a past session whose zone this version
    /// no longer offers, so the footer carries the note naming the field that did not load. Reached the way
    /// the user reaches it - through the command from the result screen - so the render shows what the
    /// press actually produces, not a form dressed up to look like it.</summary>
    public static SessionViewModel SetupAfterRepeatWithMissingZone()
    {
        var model = ResultWorks();
        model.SelectedRecord = model.History[1] with { TzBiasMin = 999 };
        model.Commands.Repeat.Execute(null);
        return model;
    }

    /// <summary>An application chosen and the options that show as chips in the folded header.</summary>
    public static SessionViewModel SetupConfigured()
    {
        var model = WithTarget(SetupStartup());
        model.ScaleDuration = true;
        model.ScaleQpc = true;
        model.ForceStart = true;
        return model;
    }

    /// <summary>
    /// Gives a model the application every phase render assumes.
    /// </summary>
    /// <remarks>
    /// 🔴 A SESSION HAS AN APPLICATION, and SessionStates does not set one - it was written for the panel,
    /// where the form supplies it. Without this the session phase's first line is bound to a model with no
    /// target and renders as nothing, so the render would be missing the one thing that says which session
    /// it is. Set here rather than in SessionStates, because the panel renders use those fixtures and their
    /// baselines would move.
    /// </remarks>
    public static SessionViewModel WithTarget(SessionViewModel model)
    {
        model.SetTarget(Path.Combine(TestPaths.RepoRoot(), "target.exe"));
        return model;
    }

    // ---- The result phase ----
    //
    // Built here rather than on SessionStates, because a finished session has three things the panel's
    // fixtures never needed: a history to fold, a heartbeat later than the start so "reached" differs from
    // "started at", and a diagnostics log that returns a path. The panel's own fixtures stay untouched, so
    // their baselines stay untouched.

    /// <summary>A session that worked: two processes, the application closed itself, three earlier sessions on record.</summary>
    public static SessionViewModel ResultWorks()
    {
        var model = ResultRunning();
        model.Apply(new SessionVerdictEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Verdict = "works",
            ReasonKey = "session.family_covered",
            ProcessCount = 2,
            WarningKeys = ["coverage.pid_registry_full"],
        });
        model.Apply(Ended());
        return model;
    }

    /// <summary>
    /// A session whose record moved a history this version cannot read aside (R5-D73), with the history
    /// section open: the note naming where it went, over the one row the history now starts with. The path is
    /// as long as a real one under a long account name, which is what the note has to wrap.
    /// </summary>
    public static SessionViewModel ResultWithHistorySetAside()
    {
        var model = ResultRunning(new SessionViewModel(new SetAsideHistoryStore(
            @"C:\Users\a.tester.with.a.long.account.name\AppData\Local\ChronoMock\history\sessions.json.unreadable-20261020-180000")));
        model.Apply(new SessionVerdictEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Verdict = "works",
            ReasonKey = "session.family_covered",
            ProcessCount = 1,
        });
        model.Apply(Ended());
        model.RecordSessionAsync().GetAwaiter().GetResult(); // the store answers at once - no thread to wait for
        // The record is stamped with the real clock, and a render that moves with it is no baseline.
        model.History[0] = model.History[0] with { EndedAtUtc = "2026-09-09T20:31:01Z" };
        return model;
    }

    /// <summary>
    /// A clear that found the history unreadable and moved it aside (R5-D73, review round on #133): no rows
    /// left, and the section stays to say where the old history went.
    /// </summary>
    public static SessionViewModel ResultWithHistoryClearedAside()
    {
        var model = ResultRunning(new SessionViewModel(new SetAsideHistoryStore(
            @"C:\Users\a.tester\AppData\Local\ChronoMock\history\sessions.json.unreadable-20261020-190000")));
        model.Apply(new SessionVerdictEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Verdict = "works",
            ReasonKey = "session.family_covered",
            ProcessCount = 1,
        });
        model.Apply(Ended());
        model.ClearHistoryAsync().GetAwaiter().GetResult(); // the store answers at once - no thread to wait for
        return model;
    }

    /// <summary>
    /// A works session the machine slept through for eight hours (R5-D71): the warning on the audit, and the
    /// fact row that says how long the session clock stood still.
    /// </summary>
    public static SessionViewModel ResultWorksAfterSleep()
    {
        var model = ResultRunning();
        model.Apply(new HostSleptEvent { V = ProtocolJson.ProtocolVersion, SleptMs = 28_800_000, TotalSleptMs = 28_800_000 });
        model.Apply(new SessionVerdictEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Verdict = "works",
            ReasonKey = "session.family_covered",
            ProcessCount = 1,
            WarningKeys = ["time.host_slept"],
            HostSleptMs = 28_800_000,
        });
        model.Apply(Ended());
        return model;
    }

    /// <summary>
    /// A session that outlived the program it launched: a launcher, or a script running <c>start app.exe</c>,
    /// ended and left the application running, and the session went on for it (ADR-16). The warning is what
    /// tells the tester why the session lasted longer than the program they chose.
    /// </summary>
    public static SessionViewModel ResultWorksAfterHandOff()
    {
        var model = ResultRunning();
        model.Apply(new SessionVerdictEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Verdict = "works",
            ReasonKey = "session.family_covered",
            ProcessCount = 2,
            WarningKeys = ["session.followed_family"],
            Followed = [new FollowedProcess { Pid = 5150, Image = "app.exe" }, new FollowedProcess { Pid = 5151 }],
        });
        model.Apply(Ended());
        return model;
    }

    /// <summary>
    /// A session whose application closed without showing a window while a copy of it ran outside the
    /// session (ADR-4, 2026-11-11): a single-instance application that most likely passed its work to that
    /// copy after the guard window. Partial, with the copy named under the exit code and the warning that
    /// says what to do.
    /// </summary>
    public static SessionViewModel ResultPartialHandedToCopy(string image = "app.exe")
    {
        var model = ResultRunning();
        model.Apply(new SessionVerdictEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Verdict = "partial",
            ReasonKey = "session.family_handed_to_copy",
            ProcessCount = 1,
            WarningKeys = ["session.handed_to_copy"],
            CopiesOutside = [new FollowedProcess { Pid = 4600, Image = image }],
        });
        model.Apply(Ended());
        return model;
    }

    /// <summary>
    /// A session that works as a mechanism and proves nothing about time (R5-D3): a Go program, whose wall
    /// clock no hook reaches, and no process read a clock the session replaces. Both warnings at once, because
    /// the two are raised independently and a Go program that reads only the shared page gets both. The
    /// headline stays Works and both sentences stand under its reason.
    /// </summary>
    public static SessionViewModel ResultWorksButNotEvidence() => NotEvidence("works", "session.family_covered");

    /// <summary>The same two warnings under a partial verdict, whose meaning line and open audit already say
    /// what went wrong - the caveats under the headline are for a works alone.</summary>
    public static SessionViewModel ResultPartialButNotEvidence() => NotEvidence("partial", "session.family_partial");

    private static SessionViewModel NotEvidence(string verdict, string reasonKey)
    {
        var model = ResultRunning();
        model.Apply(new SessionVerdictEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Verdict = verdict,
            ReasonKey = reasonKey,
            ProcessCount = 1,
            WarningKeys = ["coverage.session_clock_never_read", "runtime.go_wall_clock_unreachable"],
        });
        model.Apply(Ended());
        return model;
    }

    /// <summary>A session that only partly worked: the reason and the meaning both have to show under the word.</summary>
    public static SessionViewModel ResultPartial()
    {
        var model = ResultRunning();
        model.Apply(new SessionVerdictEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Verdict = "partial",
            ReasonKey = "session.family_partial",
            ProcessCount = 1,
            WarningKeys = [],
        });
        model.Apply(Ended());
        return model;
    }

    /// <summary>
    /// A session whose family spawned processes the hook never got into - the shape an application with
    /// an embedded web engine produces (measured on two of them, 2026-09-21): the engine's runtime in
    /// every role it runs, one of them the renderer, a second runtime with a renderer of its own, two
    /// children gone before they could be named, and a total above the named list.
    /// </summary>
    public static SessionViewModel ResultPartialWithUncoveredProcesses()
    {
        var model = ResultRunning();
        model.Apply(new SessionVerdictEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Verdict = "partial",
            ReasonKey = "session.family_partial_children",
            ProcessCount = 3,
            WarningKeys = ["inheritance.children_uncovered", "embedded.web_engine_uncovered"],
            UncoveredChildren =
            [
                new UncoveredChild { Pid = 5120, ParentPid = 4300, Image = "msedgewebview2.exe", Role = "crashpad-handler" },
                new UncoveredChild { Pid = 5136, ParentPid = 4300, Image = "msedgewebview2.exe", Role = "gpu-process" },
                new UncoveredChild { Pid = 5150, ParentPid = 4300, Image = "msedgewebview2.exe", Role = "utility" },
                new UncoveredChild { Pid = 5164, ParentPid = 4300, Image = "msedgewebview2.exe", Role = "utility" },
                new UncoveredChild { Pid = 5180, ParentPid = 4300, Image = "msedgewebview2.exe", Role = "renderer" },
                new UncoveredChild { Pid = 6012, ParentPid = 4242, Image = "QtWebEngineProcess.exe", Role = "renderer" },
                new UncoveredChild { Pid = 6100, ParentPid = 4242 },
                new UncoveredChild { Pid = 6104, ParentPid = 4242 },
            ],
            UncoveredChildrenTotal = 11,
        });
        model.Apply(Ended());
        return model;
    }

    /// <summary>
    /// A session whose application has an embedded web engine and whose pages the session reached through
    /// the engine's debugging port (docs/09 section 12): the engine's helper processes still ran on the
    /// real clock and stand in the process table, the renderer among them, but its pages have rows of their
    /// own under the parent's - context rows, not process rows - and the warning says they were reached
    /// rather than that they read the real clock. The shape the bench measured on two hosts, 2026-09-22.
    /// </summary>
    public static SessionViewModel ResultPartialWithEmbeddedPages() => EmbeddedPages([], []);

    /// <summary>The same session after an application that runs as administrator, whose registry value
    /// was written and taken away again (docs/09 section 12.19): the account of what was done to the machine
    /// stands with the other warnings.</summary>
    public static SessionViewModel ResultElevatedRemoved() => EmbeddedPages([], ["embedded.policy_value_removed"]);

    /// <summary>The same, with the value that could NOT be taken away. The core puts that one FIRST in the
    /// warnings, because the list is a short window onto a longer one and it is the one line that says
    /// something is still set on this machine.</summary>
    public static SessionViewModel ResultElevatedLeft() => EmbeddedPages(["embedded.policy_value_left"], []);

    /// <summary>The same session with what the audit could not see in its pages (R5-F9, R5-F10): pages gone
    /// before it read them, and the engine's own stamps and animations at real speed while the timers ran
    /// faster - the three longest of the page warnings, drawn together.</summary>
    public static SessionViewModel ResultEngineTimeUnseen() =>
        EmbeddedPages([], ["chromium.contexts_unread", "chromium.engine_stamps_real", "chromium.animations_real"]);

    private static SessionViewModel EmbeddedPages(string[] firstWarningKeys, string[] lastWarningKeys)
    {
        var model = ResultRunning();
        // The parent's final snapshot arrives BEFORE the page rows, as the core emits them - and the
        // page rows must survive it whichever order they come in.
        model.Apply(new CoverageEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Pid = 4242,
            Covered =
            [
                new CoveredChannel { Channel = "GetSystemTimeAsFileTime", Calls = 130_020 },
                new CoveredChannel { Channel = "GetLocalTime", Calls = 3_602 },
                new CoveredChannel { Channel = "GetTickCount64", Calls = 45_118 },
            ],
            Uncovered = ["QueryPerformanceCounter"],
            Unobserved = ["NtQuerySystemTime"],
            InstalledLate = ["timeGetTime"],
            WarningKeys = [],
        });
        model.Apply(new CoverageEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Pid = 1,
            Kind = CoverageEvent.UnitContext,
            Covered =
            [
                new CoveredChannel { Channel = "page Date.now", Calls = 612 },
                new CoveredChannel { Channel = "page performance.now", Calls = 588 },
                new CoveredChannel { Channel = "page setInterval", Calls = 2 },
            ],
            WarningKeys = [],
        });
        model.Apply(new CoverageEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Pid = 2,
            Kind = CoverageEvent.UnitContext,
            Covered = [new CoveredChannel { Channel = "worker Date.now", Calls = 41 }],
            WarningKeys = [],
        });
        model.Apply(new SessionVerdictEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Verdict = "partial",
            ReasonKey = "session.family_partial_children",
            ProcessCount = 3,
            WarningKeys =
            [
                ..firstWarningKeys,
                "inheritance.children_uncovered", "embedded.web_engine_reached", "embedded.debug_port_open",
                "chromium.rate_change_affects_running_timers",
                ..lastWarningKeys,
            ],
            UncoveredChildren =
            [
                new UncoveredChild { Pid = 5120, ParentPid = 4300, Image = "msedgewebview2.exe", Role = "crashpad-handler" },
                new UncoveredChild { Pid = 5136, ParentPid = 4300, Image = "msedgewebview2.exe", Role = "gpu-process" },
                new UncoveredChild { Pid = 5150, ParentPid = 4300, Image = "msedgewebview2.exe", Role = "utility" },
                new UncoveredChild { Pid = 5180, ParentPid = 4300, Image = "msedgewebview2.exe", Role = "renderer" },
            ],
            UncoveredChildrenTotal = 4,
            ContextCount = 2,
            Engines = [new ReachedEngine { Pid = 4300, Port = 61868, Browser = "Engine/153.0", ContextCount = 2 }],
        });
        model.Apply(Ended());
        return model;
    }

    /// <summary>
    /// An engine reached whose pages the session did not put on its clock (R5, P15): the engine row stands,
    /// there are no page rows, the renderer is among the processes on the real clock, and the note under
    /// the engines table says that no page was put on the session clock that way. It used to say they were.
    /// </summary>
    public static SessionViewModel ResultPartialWithEngineButNoPages()
    {
        var model = ResultRunning();
        model.Apply(new CoverageEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Pid = 4242,
            Covered =
            [
                new CoveredChannel { Channel = "GetSystemTimeAsFileTime", Calls = 130_020 },
                new CoveredChannel { Channel = "GetTickCount64", Calls = 45_118 },
            ],
            WarningKeys = [],
        });
        model.Apply(new SessionVerdictEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Verdict = "partial",
            ReasonKey = "session.family_partial_children",
            ProcessCount = 2,
            WarningKeys = ["inheritance.children_uncovered", "embedded.web_engine_uncovered", "embedded.debug_port_open"],
            UncoveredChildren = [new UncoveredChild { Pid = 5180, ParentPid = 4300, Image = "msedgewebview2.exe", Role = "renderer" }],
            UncoveredChildrenTotal = 1,
            ContextCount = 0,
            Engines = [new ReachedEngine { Pid = 4300, Port = 61868, Browser = "Engine/153.0", ContextCount = 0 }],
        });
        model.Apply(Ended());
        return model;
    }

    /// <summary>
    /// The family verdict naming <paramref name="total"/> processes the hook never got into, applied to
    /// a model in whatever state it is - for a guard that reads a count off the screen and wants that
    /// count to be one no other list shares - or, with an <paramref name="image"/>, for a guard that
    /// reads how the table treats one particular name.
    /// </summary>
    public static SessionViewModel WithUncoveredProcesses(SessionViewModel model, int total, string image = "helper.exe")
    {
        model.Apply(new SessionVerdictEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Verdict = "partial",
            ReasonKey = "session.family_partial_children",
            ProcessCount = 2,
            WarningKeys = ["inheritance.children_uncovered"],
            UncoveredChildren = Enumerable.Range(0, total)
                .Select(i => new UncoveredChild { Pid = (uint)(7000 + i), ParentPid = 4242, Image = image })
                .ToList(),
            UncoveredChildrenTotal = total,
        });
        return model;
    }

    /// <summary>
    /// The family verdict naming one web engine the session reached, applied to a model in whatever state
    /// it is - for a guard that reads how the engine table treats one particular name, such as one far
    /// longer than its column. An engine names itself in its own version endpoint, so its length is not
    /// ours to promise.
    /// </summary>
    public static SessionViewModel WithEngine(SessionViewModel model, string browser, int port = 61868)
    {
        model.Apply(new SessionVerdictEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Verdict = "partial",
            ReasonKey = "session.family_partial_children",
            ProcessCount = 2,
            WarningKeys = ["embedded.web_engine_reached", "embedded.debug_port_open"],
            ContextCount = 1,
            Engines = [new ReachedEngine { Pid = 4300, Port = port, Browser = browser, ContextCount = 1 }],
        });
        return model;
    }

    /// <summary>The core refusing to start, with the diagnostics that refusal leaves behind.</summary>
    public static SessionViewModel ResultRefused() => ResultRefused([], incomplete: false);

    /// <summary>A refusal that could not end all of the application (R4-S5): the status says it was ended,
    /// and the list under it names what still runs on the real clock - one named, one the list could not
    /// name.</summary>
    public static SessionViewModel ResultRefusedLeftRunning()
        => ResultRefused([new FollowedProcess { Pid = 5150, Image = "helper.exe" }, new FollowedProcess { Pid = 5151 }], incomplete: false);

    /// <summary>A refusal that could not look for all of the application (R4-S5): the caveat under the status,
    /// and the one process it found and could not end under that.</summary>
    public static SessionViewModel ResultRefusedIncomplete()
        => ResultRefused([new FollowedProcess { Pid = 5150, Image = "helper.exe" }], incomplete: true);

    private static SessionViewModel ResultRefused(IReadOnlyList<FollowedProcess> leftRunning, bool incomplete)
    {
        var model = WithTarget(new SessionViewModel(SeededHistory(), new SavedDiagnosticsLog()));
        // 🔴 THE AUDIT COMES BEFORE THE VERDICT (crates/cli/src/core.rs, the start sequence): the core reports
        // what it saw in the guard window and then says no. Without it the first render of this state said
        // the session ended before anything could be checked - the opposite of what a refusal is.
        model.Apply(new CoverageEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Pid = 4242,
            Covered = [],
            Uncovered = ["GetSystemTimeAsFileTime", "GetLocalTime"],
            Unobserved = [],
            InstalledLate = [],
            WarningKeys = [],
        });
        model.Apply(new VerdictEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Verdict = "fails",
            ReasonKey = "coverage.time_channels_uncovered",
            RefuseStart = true,
            LeftRunning = leftRunning,
            FamilySearchIncomplete = incomplete,
        });
        model.CaptureDiagnostics(["core stderr: chrono core: verdict fails, refusing to hand back the session"]);
        return model;
    }

    /// <summary>The target gone before the hook could take: no verdict, a reason and a lifetime instead.</summary>
    /// <remarks>Built as the core sends it - coverage from the guard window, then the vanish, then an end the
    /// model ignores - rather than on the panel's fixture, which puts a heartbeat first. A heartbeat cannot
    /// precede a vanish (the session is never entered), and with one the first render of this state showed
    /// an hour of fake time passing on an application that lived 180 ms.</remarks>
    public static SessionViewModel ResultVanished() => Vanished("target.single_instance_suspected", 180);

    /// <summary>
    /// The target gone after starting a program the hook could not enter, usually one of the other bitness,
    /// which runs on the real clock - a hand-off, not a single-instance application (ADR-16). Lived 14 ms, as
    /// measured on a 64-bit launcher starting a 32-bit program.
    /// </summary>
    public static SessionViewModel ResultVanishedHandedOff() => Vanished("target.handed_off_uncovered", 14);

    private static SessionViewModel Vanished(string reasonKey, long livedMs)
    {
        var model = WithTarget(new SessionViewModel(SeededHistory()));
        model.Apply(new CoverageEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Pid = 4242,
            Covered = [new CoveredChannel { Channel = "GetSystemTimeAsFileTime", Calls = 2 }],
            Uncovered = [],
            Unobserved = [],
            InstalledLate = [],
            WarningKeys = [],
        });
        model.Apply(new VanishedEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Pid = 4242,
            ReasonKey = reasonKey,
            LivedMs = livedMs,
        });
        model.Apply(new EndedEvent { V = ProtocolJson.ProtocolVersion, Clean = true });
        return model;
    }

    /// <summary>
    /// A core that stopped after the parent's verdict (R4-W3): no session verdict, no ended, and the stream ended
    /// on its own. The verdict from the start is a works, which is what the panel used to lead with.
    /// </summary>
    public static SessionViewModel ResultCutShort()
    {
        var model = ResultRunning(new SessionViewModel(SeededHistory(), new SavedDiagnosticsLog()));
        model.Apply(new VerdictEvent { V = ProtocolJson.ProtocolVersion, Verdict = "works", ReasonKey = "coverage.time_channels_covered" });
        model.OnStreamEnded(watchdogFired: false, stopRequested: false);
        model.CaptureDiagnostics(["core stderr: thread 'main' panicked at an unexpected state"], coreExit: 101);
        return model;
    }

    /// <summary>A start that never got off the ground, with its diagnostics.</summary>
    public static SessionViewModel ResultNotStarted()
    {
        var model = WithTarget(new SessionViewModel(SeededHistory(), new SavedDiagnosticsLog()));
        model.Apply(new ErrorEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Id = 1,
            Code = 3,
            Key = "target.inject_failed",
            Origin = "core",
        });
        model.CaptureDiagnostics(["core stderr: chrono core: inject failed: access denied"]);
        return model;
    }

    /// <summary>
    /// A start the application's own loading ended (R4-S6), with the longest of those texts: a library
    /// of the other bitness. The headline is the whole explanation, so it has to fit and wrap.
    /// </summary>
    public static SessionViewModel ResultLoaderFailed()
    {
        var model = WithTarget(new SessionViewModel(SeededHistory(), new SavedDiagnosticsLog()));
        model.Apply(new ErrorEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Id = 1,
            Code = 2,
            Key = "target.loader_bad_image",
            Origin = "mechanism",
        });
        model.CaptureDiagnostics(["core stderr: chrono core: the target ended while Windows was loading it, with code 0xC000007B"]);
        return model;
    }

    /// <summary>The worked session with one earlier session chosen, so the row actions have something to act on.</summary>
    public static SessionViewModel ResultWithHistoryChosen()
    {
        var model = ResultWorks();
        model.SelectedRecord = model.History[1];
        return model;
    }

    /// <summary>Running with the audit reported, on a history of three, one heartbeat past the start.</summary>
    private static SessionViewModel ResultRunning(SessionViewModel? start = null)
    {
        var model = WithTarget(start ?? new SessionViewModel(SeededHistory()));
        // The zone the heartbeats carry. No session was started here, so the started-at fact falls back to
        // the form, and a form left on UTC made the first render start a session at +00:00 and reach +02:00.
        model.SelectedZone = TimeInputs.Zones.Single(z => z.BiasMinutes == -120);
        model.Apply(new StateEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Fake = new Clock { Wall = "2038-01-19T03:14:07", ZoneBiasMin = -120 },
            Real = new Clock { Wall = "2026-09-09T20:30:00", ZoneBiasMin = -120 },
            Multiplier = 60,
            ElapsedFakeMs = 0,
            ElapsedRealMs = 0,
        });
        model.Apply(new CoverageEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Pid = 4242,
            Covered =
            [
                new CoveredChannel { Channel = "GetSystemTimeAsFileTime", Calls = 128_004 },
                new CoveredChannel { Channel = "GetLocalTime", Calls = 3_517 },
                new CoveredChannel { Channel = "GetTickCount64", Calls = 44_910 },
            ],
            Uncovered = ["QueryPerformanceCounter"],
            Unobserved = ["NtQuerySystemTime"],
            InstalledLate = ["timeGetTime"],
            WarningKeys = ["wait.timeout_collapsed", "coverage.channel_installed_late"],
        });
        // 🔴 A later heartbeat, so the fake clock has moved since the start. Without it "started at" and
        // "reached" print the same moment beside an hour of elapsed time, and the render would be arguing
        // with itself.
        model.Apply(new StateEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Fake = new Clock { Wall = "2038-01-19T04:15:06", ZoneBiasMin = -120 },
            Real = new Clock { Wall = "2026-09-09T20:31:00", ZoneBiasMin = -120 },
            Multiplier = 60,
            ElapsedFakeMs = 3_659_000,
            ElapsedRealMs = 60_990,
        });
        return model;
    }

    /// <summary>The application closed itself a second after the last heartbeat, and the end timing says so.</summary>
    private static EndedEvent Ended() => new()
    {
        V = ProtocolJson.ProtocolVersion,
        Clean = true,
        TargetExitCode = 0,
        ElapsedRealMs = 61_000,
        ElapsedFakeMs = 3_660_000,
        FakeEndWall = "2038-01-19T04:15:07",
    };

    /// <summary>Three earlier sessions, oldest appended first so the newest is at the top of the well.</summary>
    private static InMemorySessionHistoryStore SeededHistory()
    {
        var store = new InMemorySessionHistoryStore();
        store.Append(new SessionRecord
        {
            TargetPath = @"C:\apps\sample-app.exe",
            MomentLocal = "2027-12-31T23:59:59",
            TzBiasMin = 0,
            Mode = "multiplier",
            Multiplier = 1440,
            Verdict = "fails",
            EndedAtUtc = "2026-09-11T09:12:44Z",
        });
        store.Append(new SessionRecord
        {
            TargetPath = @"C:\apps\sample-app.exe",
            MomentLocal = "2028-02-29T12:00:00",
            TzBiasMin = 300,
            Mode = "multiplier",
            Multiplier = 60,
            Verdict = "partial",
            EndedAtUtc = "2026-09-12T15:40:02Z",
        });
        store.Append(new SessionRecord
        {
            TargetPath = Path.Combine(TestPaths.RepoRoot(), "target.exe"),
            MomentLocal = "2038-01-19T03:14:07",
            TzBiasMin = -120,
            Mode = "multiplier",
            Multiplier = 60,
            Verdict = "works",
            EndedAtUtc = "2026-09-13T18:05:31Z",
        });
        return store;
    }

    /// <summary>A diagnostics log that always reports a file, so the saved-path line is on the picture.</summary>
    private sealed class SavedDiagnosticsLog : IDiagnosticsLog
    {
        public string? Save(string content)
            => @"C:\Users\qa\AppData\Local\ChronoMock\diagnostics\diagnostics-2026-09-13-203100.log";
    }
}
