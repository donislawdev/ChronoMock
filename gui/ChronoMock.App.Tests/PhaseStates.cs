using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using ChronoMock.App;
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
        => new(new InMemorySessionHistoryStore(), presetsDir: Path.Combine(TestPaths.RepoRoot(), "presets"));

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
        model.TargetArgs = "--seed 7 --headless";
        model.WorkingFolder = @"C:\apps\data";
        return model;
    }

    /// <summary>An application chosen and a date that does not exist.</summary>
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

    /// <summary>The core refusing to start, with the diagnostics that refusal leaves behind.</summary>
    public static SessionViewModel ResultRefused()
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
        });
        model.CaptureDiagnostics(["core stderr: chrono core: verdict fails, refusing to hand back the session"]);
        return model;
    }

    /// <summary>The target gone before the hook could take: no verdict, a reason and a lifetime instead.</summary>
    /// <remarks>Built as the core sends it - coverage from the guard window, then the vanish, then an end the
    /// model ignores - rather than on the panel's fixture, which puts a heartbeat first. A heartbeat cannot
    /// precede a vanish (the session is never entered), and with one the first render of this state showed
    /// an hour of fake time passing on an application that lived 180 ms.</remarks>
    public static SessionViewModel ResultVanished()
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
            ReasonKey = "target.single_instance_suspected",
            LivedMs = 180,
        });
        model.Apply(new EndedEvent { V = ProtocolJson.ProtocolVersion, Clean = true });
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

    /// <summary>The worked session with one earlier session chosen, so the row actions have something to act on.</summary>
    public static SessionViewModel ResultWithHistoryChosen()
    {
        var model = ResultWorks();
        model.SelectedRecord = model.History[1];
        return model;
    }

    /// <summary>Running with the audit reported, on a history of three, one heartbeat past the start.</summary>
    private static SessionViewModel ResultRunning()
    {
        var model = WithTarget(new SessionViewModel(SeededHistory()));
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
