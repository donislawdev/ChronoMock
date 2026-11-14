using ChronoMock.Protocol;

namespace ChronoMock.App.Tests;

/// <summary>
/// What a session said that the copied summary used to leave out (R5-D71, R5-D44): the errors the core sent,
/// how long the machine slept, and the browser a Chromium session drove. The CLI report has said all three
/// since the core learned them, and <c>WireGoldenTests.KnownUnread</c> and
/// <c>SummaryParityTests.KnownCliOnly</c> carried them as the window's gaps until these closed them.
/// </summary>
public class SessionReportFactsTests
{
    private static string Key(string key) => key;

    private static StateEvent Heartbeat => new()
    {
        V = ProtocolJson.ProtocolVersion,
        Fake = new Clock { Wall = "2038-01-19T03:14:08" },
        Real = new Clock { Wall = "2026-10-20T10:00:01" },
        Multiplier = 1,
        ElapsedRealMs = 1000,
        ElapsedFakeMs = 1000,
    };

    private static SessionVerdictEvent Verdict(long sleptMs = 0, string? browser = null, params string[] warnings) => new()
    {
        V = ProtocolJson.ProtocolVersion,
        Verdict = "works",
        ReasonKey = "session.family_works",
        WarningKeys = warnings,
        HostSleptMs = sleptMs,
        Browser = browser,
    };

    private static ErrorEvent Error(string key, long? id = null) => new()
    {
        V = ProtocolJson.ProtocolVersion,
        Id = id,
        Key = key,
        Origin = "core",
    };

    /// <summary>
    /// The sleep is on the warning list the moment the core notices it, not only once the verdict brings the
    /// same key at the end - a tester watching a session the machine slept through saw nothing until it ended.
    /// The verdict then settles the total and does not list the warning twice. Reversal probes: ignore the
    /// event and the first assertion fails, drop the <c>Except</c> and the key is listed twice.
    /// </summary>
    [Fact]
    public void The_sleep_is_on_the_warning_list_the_moment_the_core_notices_it()
    {
        var vm = new SessionViewModel();
        vm.Apply(Heartbeat);
        var raised = new List<string?>();
        vm.PropertyChanged += (_, e) => raised.Add(e.PropertyName);

        vm.Apply(new HostSleptEvent { V = ProtocolJson.ProtocolVersion, SleptMs = 3_600_000, TotalSleptMs = 3_600_000 });

        Assert.Contains("time.host_slept", vm.Warnings);
        Assert.Equal(("1:00:00", true), (vm.HostSlept, vm.HasHostSlept));
        Assert.Contains(nameof(SessionViewModel.Warnings), raised);
        Assert.Contains(nameof(SessionViewModel.HostSlept), raised);

        // The machine sleeps again: the time grows, the warning stays one line.
        vm.Apply(new HostSleptEvent { V = ProtocolJson.ProtocolVersion, SleptMs = 1_800_000, TotalSleptMs = 5_400_000 });
        Assert.Single(vm.Warnings, w => w == "time.host_slept");
        vm.Apply(Verdict(5_400_000, null, "time.host_slept"));

        Assert.Single(vm.Warnings, w => w == "time.host_slept");
        Assert.Equal("1:30:00", vm.HostSlept);
    }

    /// <summary>A session the machine did not sleep through has no slept line anywhere.</summary>
    [Fact]
    public void A_session_without_sleep_says_nothing_about_it()
    {
        var vm = new SessionViewModel();
        vm.Apply(Heartbeat);
        vm.Apply(Verdict());

        Assert.Equal((string.Empty, false), (vm.HostSlept, vm.HasHostSlept));
        Assert.DoesNotContain("report.slept", vm.BuildSummary(Key), StringComparison.Ordinal);
    }

    /// <summary>
    /// Every error the core sent reaches the summary: one it refused while the session ran (which the screen
    /// showed only while it ran), one the session ended on, and one that arrived after the end. Reversal
    /// probe: stop recording errors and the list is gone.
    /// </summary>
    [Fact]
    public void Every_error_the_core_sends_is_listed_in_the_summary()
    {
        var vm = new SessionViewModel();
        vm.Apply(Heartbeat);
        vm.Apply(Error("time.bad_multiplier", id: 12));
        vm.Apply(Error("hook.inject_failed"));
        vm.Apply(new EndedEvent { V = ProtocolJson.ProtocolVersion, Clean = true });
        vm.Apply(Error("cdp.profile_locked"));

        var summary = vm.BuildSummary(Key);

        Assert.Contains(
            "  report.errors (3):\n    - time.bad_multiplier\n    - hook.inject_failed\n    - cdp.profile_locked\n",
            summary,
            StringComparison.Ordinal);
    }

    /// <summary>R5-D44: the browser a Chromium session drove is named under the application, and a session
    /// that drove none has no line for it.</summary>
    [Fact]
    public void The_browser_a_chromium_session_drove_is_named_under_the_application()
    {
        var vm = new SessionViewModel();
        vm.SetTarget(@"C:\apps\app.exe");
        vm.Apply(Verdict(0, "Chrome/152.0.7977.149"));

        Assert.Contains(
            "  report.target: app.exe\n  report.browser: Chrome/152.0.7977.149\n",
            vm.BuildSummary(Key),
            StringComparison.Ordinal);

        var native = new SessionViewModel();
        native.Apply(Verdict());
        Assert.DoesNotContain("report.browser", native.BuildSummary(Key), StringComparison.Ordinal);
    }

    /// <summary>A new session starts without the last one's errors, sleep and browser - a summary of it would
    /// otherwise describe a session that is not this one (untouchable rule 4).</summary>
    [Fact]
    public void A_new_session_forgets_what_the_last_one_reported()
    {
        var vm = new SessionViewModel();
        vm.Apply(Heartbeat);
        vm.Apply(new HostSleptEvent { V = ProtocolJson.ProtocolVersion, SleptMs = 60_000, TotalSleptMs = 60_000 });
        vm.Apply(Error("time.bad_multiplier", id: 12));
        vm.Apply(Verdict(60_000, "Chrome/152.0.7977.149", "time.host_slept"));
        vm.Apply(new EndedEvent { V = ProtocolJson.ProtocolVersion, Clean = true });
        Assert.True(vm.HasHostSlept); // the last session reported all three - not vacuous

        vm.BeginNewSession();

        var summary = vm.BuildSummary(Key);
        Assert.False(vm.HasHostSlept);
        Assert.DoesNotContain("report.errors", summary, StringComparison.Ordinal);
        Assert.DoesNotContain("report.browser", summary, StringComparison.Ordinal);
        Assert.DoesNotContain("report.slept", summary, StringComparison.Ordinal);
    }

    /// <summary>The slept time goes in the session's block beside the other durations, and under its own
    /// heading when the session never reported its timing, rather than as a line hanging under nothing.</summary>
    [Fact]
    public void The_slept_time_is_said_in_the_session_block()
    {
        var setup = new SessionSetup(
            @"C:\apps\app.exe", "2031-02-03T04:05:06", TimeInputs.Zones.First(), TimeInputs.Modes.First(),
            string.Empty, string.Empty, false, false, false, false, false, false, false);
        string Slept(string key) => key == "report.slept" ? "slept {0}" : key;

        var timed = SessionReport.Summary(
            new SessionResult { HasTiming = true, HostSleptMs = 5_400_000 }, setup, "-", Slept);
        var untimed = SessionReport.Summary(new SessionResult { HostSleptMs = 61_000 }, setup, "-", Slept);

        Assert.Contains("    report.elapsed\n    slept 1:30:00\n", timed, StringComparison.Ordinal);
        Assert.Contains("  report.session: slept 0:01:01\n", untimed, StringComparison.Ordinal);
    }
}
