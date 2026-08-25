using ChronoMock.App;
using ChronoMock.Protocol;

namespace ChronoMock.App.Tests;

/// <summary>
/// The event-to-panel mapping (SessionViewModel.Apply). Pure and synchronous, so it is tested without a
/// core process or a UI thread: a state heartbeat fills both clocks with explicit zones, and a terminal
/// outcome cannot be resurrected by a late heartbeat.
/// </summary>
public class SessionViewModelTests
{
    private static StateEvent State(
        string fakeWall, string realWall, int bias, long multiplier,
        long elapsedRealMs = 0, long elapsedFakeMs = 0) => new()
        {
            V = ProtocolJson.ProtocolVersion,
            Fake = new Clock { Wall = fakeWall, ZoneBiasMin = bias },
            Real = new Clock { Wall = realWall, ZoneBiasMin = bias },
            Multiplier = multiplier,
            ElapsedRealMs = elapsedRealMs,
            ElapsedFakeMs = elapsedFakeMs,
        };

    // A fake translator: known format templates for the keys under test, echoing the key otherwise (which
    // is also the production fallback for a missing key). This keeps BuildSummary testable without WPF.
    private static Func<string, string> T()
    {
        var map = new Dictionary<string, string>(StringComparer.Ordinal)
        {
            ["report.unreliable_banner"] = "!! UNRELIABLE EVIDENCE - not proof",
            ["report.title"] = "Chrono Mock - session report",
            ["report.vanish_detail"] = "suspected single-instance app: {0} - lived {1} ms",
            ["report.session_reached"] = "fake clock reached {0}",
            ["report.elapsed"] = "real {0}s, fake {1}s",
            ["report.processes"] = "processes: {0}",
            ["report.requested"] = "requested: {0} (zone {1}, mode {2})",
            ["mode.x60"] = "×60",
        };
        return key => map.TryGetValue(key, out var value) ? value : key;
    }

    [Fact]
    public void State_event_fills_both_clocks_with_explicit_zones_and_rate()
    {
        var vm = new SessionViewModel();

        vm.Apply(State("2038-01-19T03:14:07", "2026-08-24T20:30:00", bias: -120, multiplier: 60));

        Assert.Equal("2038-01-19T03:14:07", vm.Fake.Wall);
        Assert.Equal("2038-01-19", vm.Fake.Date); // split onto two lines so a long ISO value never wraps
        Assert.Equal("03:14:07", vm.Fake.Time);
        Assert.Equal("UTC+02:00", vm.Fake.Zone);
        Assert.Equal("2026-08-24T20:30:00", vm.Real.Wall);
        Assert.Equal("2026-08-24", vm.Real.Date);
        Assert.Equal("20:30:00", vm.Real.Time);
        Assert.Equal("UTC+02:00", vm.Real.Zone);
        Assert.Equal("x60", vm.MultiplierText);
        Assert.Equal(SessionStatusKind.Running, vm.StatusKind);
    }

    [Fact]
    public void Vanished_marks_did_not_take_effect_and_is_not_resurrected_by_a_late_state()
    {
        var vm = new SessionViewModel();

        vm.Apply(new VanishedEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Pid = 1234,
            ReasonKey = "target.single_instance_suspected",
            LivedMs = 10,
        });
        Assert.Equal(SessionStatusKind.DidNotTakeEffect, vm.StatusKind);

        // A heartbeat arriving after a terminal outcome must not flip the panel back to "running".
        vm.Apply(State("x", "y", bias: 0, multiplier: 1));
        Assert.Equal(SessionStatusKind.DidNotTakeEffect, vm.StatusKind);
    }

    [Fact]
    public void Ended_marks_the_session_ended()
    {
        var vm = new SessionViewModel();

        vm.Apply(new EndedEvent { V = ProtocolJson.ProtocolVersion, Clean = true });

        Assert.Equal(SessionStatusKind.Ended, vm.StatusKind);
        Assert.Equal("status.ended", vm.StatusKey);
    }

    [Fact]
    public void Starts_idle_with_no_target_chosen()
    {
        var vm = new SessionViewModel();

        Assert.Equal(SessionStatusKind.Idle, vm.StatusKind);
        Assert.False(vm.HasTarget);   // nothing chosen yet
        Assert.False(vm.CanStart);    // Start stays disabled until a target is chosen (chrono-mock 7.1, zasady/13 11)
        Assert.False(vm.VerdictKnown);
        Assert.Equal("clock.fake", vm.Fake.RoleKey);
        Assert.Equal("clock.real", vm.Real.RoleKey);
    }

    [Fact]
    public void Choosing_a_target_enables_start_and_shows_its_file_name()
    {
        var vm = new SessionViewModel();
        Assert.False(vm.CanStart);

        vm.SetTarget(@"C:\apps\Ledger.exe");

        Assert.True(vm.HasTarget);
        Assert.Equal("Ledger.exe", vm.TargetName);
        Assert.True(vm.CanStart);
    }

    [Fact]
    public void Defaults_the_zone_and_mode_to_the_shipped_values()
    {
        var vm = new SessionViewModel();

        Assert.Equal(-120, vm.SelectedZone.BiasMinutes); // UTC+02:00
        Assert.Equal("multiplier", vm.SelectedMode.Mode);
        Assert.Equal(60, vm.SelectedMode.Multiplier);
        Assert.True(vm.MomentValid);
    }

    [Theory]
    [InlineData("2038-01-19T03:14:07", true)]
    [InlineData("2038-01-19 03:14:07", true)]   // a space separator is accepted too
    [InlineData("2038-02-29T00:00:00", false)]  // 2038 is not a leap year
    [InlineData("not a date", false)]
    [InlineData("", false)]
    public void Moment_validity_follows_the_text(string text, bool valid)
        => Assert.Equal(valid, new SessionViewModel { MomentText = text }.MomentValid);

    [Fact]
    public void An_invalid_moment_disables_start_even_with_a_target()
    {
        var vm = new SessionViewModel();
        vm.SetTarget(@"C:\apps\Ledger.exe");
        Assert.True(vm.CanStart);

        vm.MomentText = "nonsense";

        Assert.False(vm.MomentValid);
        Assert.False(vm.CanStart);
    }

    [Fact]
    public void Build_time_maps_the_inputs_to_the_wire()
    {
        var vm = new SessionViewModel { MomentText = "2040-06-15 08:30:00" };
        vm.SelectedZone = vm.Zones.First(z => z.BiasMinutes == 300); // UTC-05:00
        vm.SelectedMode = vm.Modes.First(m => m.Mode == "frozen");

        var time = vm.BuildTime();

        Assert.Equal("absolute", time.Moment.Kind);
        Assert.Equal("2040-06-15T08:30:00", time.Moment.Local); // canonicalised to the 'T' form
        Assert.Equal(300, time.Moment.TzBiasMin);
        Assert.Equal("frozen", time.Mode);
        Assert.Null(time.Multiplier);
        Assert.False(time.ScaleDuration);
    }

    [Fact]
    public void Works_verdict_shows_the_label_without_a_caveat()
    {
        var vm = new SessionViewModel();

        vm.Apply(Verdict("works", "verdict.works.covered"));

        Assert.True(vm.VerdictKnown);
        Assert.Equal(VerdictKind.Works, vm.VerdictKind);
        Assert.Equal("verdict.works", vm.VerdictLabelKey);
        Assert.False(vm.VerdictHasReason);  // a clean works needs no reason or meaning
        Assert.False(vm.VerdictHasMeaning);
    }

    [Theory]
    [InlineData("partial", VerdictKind.Partial, "verdict.partial", "verdict.partial.meaning")]
    [InlineData("fails", VerdictKind.Fails, "verdict.fails", "verdict.fails.meaning")]
    [InlineData("undetermined", VerdictKind.Undetermined, "verdict.undetermined", "verdict.undetermined.meaning")]
    public void Non_works_verdict_shows_reason_and_meaning(
        string wire, VerdictKind kind, string labelKey, string meaningKey)
    {
        var vm = new SessionViewModel();

        vm.Apply(Verdict(wire, "verdict.some.reason"));

        Assert.Equal(kind, vm.VerdictKind);
        Assert.Equal(labelKey, vm.VerdictLabelKey);
        Assert.True(vm.VerdictHasReason);
        Assert.Equal("verdict.some.reason", vm.VerdictReasonKey);
        Assert.True(vm.VerdictHasMeaning);
        Assert.Equal(meaningKey, vm.VerdictMeaningKey);
    }

    [Fact]
    public void An_unrecognised_verdict_is_undetermined_never_works()
    {
        var vm = new SessionViewModel();

        vm.Apply(Verdict("something_new", "verdict.some.reason"));

        Assert.Equal(VerdictKind.Undetermined, vm.VerdictKind);
    }

    [Fact]
    public void Session_verdict_overrides_the_per_process_verdict_and_reports_the_family_size()
    {
        var vm = new SessionViewModel();
        vm.Apply(Verdict("works", "verdict.works.covered")); // per-process, at start
        Assert.Equal(VerdictKind.Works, vm.VerdictKind);

        vm.Apply(new SessionVerdictEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Verdict = "partial",
            ReasonKey = "verdict.partial.family",
            ProcessCount = 2,
        });

        Assert.Equal(VerdictKind.Partial, vm.VerdictKind); // the family aggregate wins
        Assert.Equal(2, vm.ProcessCount);
        Assert.True(vm.IsFamily);
    }

    [Fact]
    public void Coverage_event_fills_the_lists_with_counts()
    {
        var vm = new SessionViewModel();

        vm.Apply(new CoverageEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Pid = 100,
            Covered = [new CoveredChannel { Channel = "GetSystemTimeAsFileTime", Calls = 842 }],
            Observed = [new CoveredChannel { Channel = "QueryPerformanceCounter", Calls = 5 }],
            Uncovered = ["KUSER_SHARED_DATA"],
            WarningKeys = ["source.network_at_start"],
        });

        Assert.True(vm.CoverageKnown);
        Assert.True(vm.HasCovered);
        Assert.Contains("GetSystemTimeAsFileTime", vm.Covered[0], StringComparison.Ordinal);
        Assert.Contains("842", vm.Covered[0], StringComparison.Ordinal);
        Assert.True(vm.HasObserved);
        Assert.Contains("QueryPerformanceCounter", vm.Observed[0], StringComparison.Ordinal);
        Assert.Equal("KUSER_SHARED_DATA", Assert.Single(vm.Uncovered));
        Assert.Equal("source.network_at_start", Assert.Single(vm.Warnings));
    }

    [Fact]
    public void A_child_coverage_never_replaces_or_sums_the_parent_coverage()
    {
        var vm = new SessionViewModel();

        vm.Apply(Coverage(pid: 100, "GetSystemTimeAsFileTime", 842)); // parent, first
        vm.Apply(Coverage(pid: 200, "GetSystemTimeAsFileTime", 5));   // a child - must be ignored here

        // Still the parent's single channel and its own count (untouchable rule 4: never sum processes).
        Assert.Equal("GetSystemTimeAsFileTime  ×842", Assert.Single(vm.Covered));
    }

    [Fact]
    public void Is_running_follows_the_status()
    {
        var vm = new SessionViewModel();
        Assert.False(vm.IsRunning);

        vm.Apply(State("2038-01-19T03:14:07", "2026-08-25T00:00:00", bias: 0, multiplier: 60));
        Assert.True(vm.IsRunning);

        vm.Apply(new EndedEvent { V = ProtocolJson.ProtocolVersion, Clean = true });
        Assert.False(vm.IsRunning);
    }

    [Fact]
    public void Send_multiplier_is_a_safe_no_op_when_no_session_runs()
    {
        var vm = new SessionViewModel();

        vm.SendMultiplier(0); // idle, no client - must not throw

        Assert.False(vm.IsRunning);
    }

    [Fact]
    public void Send_jump_is_a_safe_no_op_when_no_session_runs()
    {
        var vm = new SessionViewModel();

        vm.SendJump("+1d"); // idle, no client - must not throw

        Assert.False(vm.IsRunning);
    }

    [Fact]
    public void A_works_session_summary_has_no_unreliable_banner_and_echoes_the_target()
    {
        var vm = new SessionViewModel();
        vm.SetTarget(@"C:\apps\Ledger.exe");
        vm.Apply(State("2038-01-19T03:14:07", "2026-08-25T00:00:00", bias: -120, multiplier: 60));
        vm.Apply(Verdict("works", "verdict.works.covered"));

        var summary = vm.BuildSummary(T());

        Assert.DoesNotContain("UNRELIABLE", summary, StringComparison.Ordinal);
        Assert.Contains("Ledger.exe", summary, StringComparison.Ordinal);
    }

    [Theory]
    [InlineData("partial")]
    [InlineData("fails")]
    [InlineData("undetermined")]
    public void A_non_works_session_summary_leads_with_the_unreliable_banner(string wire)
    {
        var vm = new SessionViewModel();
        vm.SetTarget(@"C:\apps\Ledger.exe");
        vm.Apply(Verdict(wire, "verdict.some.reason"));

        Assert.StartsWith("!! UNRELIABLE EVIDENCE", vm.BuildSummary(T()), StringComparison.Ordinal);
    }

    [Fact]
    public void A_vanished_session_summary_is_unreliable_and_names_the_non_effect()
    {
        var vm = new SessionViewModel();
        vm.SetTarget(@"C:\apps\Ledger.exe");
        vm.Apply(new VanishedEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Pid = 1234,
            ReasonKey = "target.single_instance_suspected",
            LivedMs = 1500,
        });

        var summary = vm.BuildSummary(T());

        Assert.StartsWith("!! UNRELIABLE EVIDENCE", summary, StringComparison.Ordinal);
        Assert.Contains("report.did_not_take_effect", summary, StringComparison.Ordinal); // key echoed by T()
        Assert.Contains("target.single_instance_suspected", summary, StringComparison.Ordinal);
        Assert.Contains("1500", summary, StringComparison.Ordinal); // lived ms
    }

    [Fact]
    public void The_summary_lists_uncovered_channels()
    {
        var vm = new SessionViewModel();
        vm.SetTarget(@"C:\apps\Ledger.exe");
        vm.Apply(new CoverageEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Pid = 100,
            Covered = [new CoveredChannel { Channel = "GetSystemTimeAsFileTime", Calls = 842 }],
            Uncovered = ["KUSER_SHARED_DATA"],
        });

        var summary = vm.BuildSummary(T());

        Assert.Contains("GetSystemTimeAsFileTime", summary, StringComparison.Ordinal);
        Assert.Contains("KUSER_SHARED_DATA", summary, StringComparison.Ordinal);
    }

    [Fact]
    public void The_summary_timing_prefers_the_authoritative_end_wall_and_elapsed()
    {
        var vm = new SessionViewModel();
        vm.SetTarget(@"C:\apps\Ledger.exe");
        vm.Apply(State("2038-01-19T03:14:07", "2026-08-25T00:00:00", bias: 0, multiplier: 60,
            elapsedRealMs: 1000, elapsedFakeMs: 60000));
        vm.Apply(new EndedEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Clean = true,
            FakeEndWall = "2038-01-20T00:00:00",
            ElapsedRealMs = 1500,
            ElapsedFakeMs = 90000,
        });

        var summary = vm.BuildSummary(T());

        Assert.Contains("fake clock reached 2038-01-20T00:00:00", summary, StringComparison.Ordinal);
        Assert.Contains("real 1.5s, fake 90.0s", summary, StringComparison.Ordinal);
    }

    [Fact]
    public void The_summary_timing_falls_back_to_the_last_heartbeat_when_ended_has_no_wall()
    {
        var vm = new SessionViewModel();
        vm.SetTarget(@"C:\apps\Ledger.exe");
        vm.Apply(State("2038-01-19T03:14:07", "2026-08-25T00:00:00", bias: 0, multiplier: 60,
            elapsedRealMs: 1000, elapsedFakeMs: 60000));
        vm.Apply(new EndedEvent { V = ProtocolJson.ProtocolVersion, Clean = true }); // no FakeEndWall

        var summary = vm.BuildSummary(T());

        Assert.Contains("fake clock reached 2038-01-19T03:14:07", summary, StringComparison.Ordinal);
        Assert.Contains("real 1.0s, fake 60.0s", summary, StringComparison.Ordinal);
    }

    [Fact]
    public void The_summary_echoes_the_requested_moment_zone_and_mode()
    {
        // Defaults: moment 2038-01-19T03:14:07, zone UTC+02:00, mode ×60.
        var vm = new SessionViewModel();
        vm.SetTarget(@"C:\apps\Ledger.exe");
        vm.Apply(Verdict("works", "verdict.works.covered"));

        var summary = vm.BuildSummary(T());

        Assert.Contains("2038-01-19T03:14:07", summary, StringComparison.Ordinal);
        Assert.Contains("UTC+02:00", summary, StringComparison.Ordinal);
        Assert.Contains("×60", summary, StringComparison.Ordinal);
    }

    [Fact]
    public void Can_copy_summary_only_after_a_session_has_started()
    {
        var vm = new SessionViewModel();
        Assert.False(vm.CanCopySummary); // idle - nothing to copy yet

        vm.Apply(State("2038-01-19T03:14:07", "2026-08-25T00:00:00", bias: 0, multiplier: 60));
        Assert.True(vm.CanCopySummary); // running

        vm.Apply(new EndedEvent { V = ProtocolJson.ProtocolVersion, Clean = true });
        Assert.True(vm.CanCopySummary); // ended - still copyable
    }

    [Fact]
    public void Copy_feedback_reports_success_and_failure()
    {
        var vm = new SessionViewModel();
        Assert.Equal(string.Empty, vm.CopyFeedbackKey);

        vm.NoteCopy(ok: true);
        Assert.Equal("copy.done", vm.CopyFeedbackKey);

        vm.NoteCopy(ok: false);
        Assert.Equal("copy.failed", vm.CopyFeedbackKey);
    }

    [Fact]
    public void Build_record_captures_the_setup_and_the_verdict()
    {
        var vm = new SessionViewModel();
        vm.SetTarget(@"C:\apps\Ledger.exe");
        vm.MomentText = "2040-06-15 08:30:00";
        vm.SelectedZone = vm.Zones.First(z => z.BiasMinutes == 300);
        vm.SelectedMode = vm.Modes.First(m => m.Mode == "frozen");
        vm.Apply(Verdict("works", "verdict.works.covered"));

        var record = vm.BuildRecord();

        Assert.Equal(@"C:\apps\Ledger.exe", record.TargetPath);
        Assert.Equal("2040-06-15T08:30:00", record.MomentLocal); // canonicalised to the 'T' form
        Assert.Equal(300, record.TzBiasMin);
        Assert.Equal("frozen", record.Mode);
        Assert.Null(record.Multiplier);
        Assert.Equal("works", record.Verdict);
        Assert.EndsWith("Z", record.EndedAtUtc, StringComparison.Ordinal); // UTC end time
    }

    [Fact]
    public void Build_record_is_undetermined_for_a_vanished_session()
    {
        var vm = new SessionViewModel();
        vm.SetTarget(@"C:\apps\Ledger.exe");
        vm.Apply(new VanishedEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Pid = 1,
            ReasonKey = "target.single_instance_suspected",
            LivedMs = 1,
        });

        Assert.Equal("undetermined", vm.BuildRecord().Verdict); // could not be audited - never a faked verdict
    }

    [Fact]
    public void Load_from_history_fills_the_setup_and_does_not_start()
    {
        var vm = new SessionViewModel();

        vm.LoadFromHistory(HistoryRecord("Ledger", moment: "2040-06-15T08:30:00", bias: 300, mode: "frozen", multiplier: null));

        Assert.Equal(@"C:\apps\Ledger.exe", vm.TargetPath);
        Assert.Equal("2040-06-15T08:30:00", vm.MomentText);
        Assert.Equal(300, vm.SelectedZone.BiasMinutes);
        Assert.Equal("frozen", vm.SelectedMode.Mode);
        Assert.Equal(SessionStatusKind.Idle, vm.StatusKind); // rule 7: fills the form, never starts a session
    }

    [Fact]
    public void Load_from_history_is_ignored_while_a_session_runs()
    {
        var vm = new SessionViewModel();
        vm.SetTarget(@"C:\apps\Ledger.exe");
        vm.Apply(State("2038-01-19T03:14:07", "2026-08-25T00:00:00", bias: 0, multiplier: 60)); // running

        vm.LoadFromHistory(HistoryRecord("Other", moment: "2000-01-01T00:00:00", bias: 0, mode: "flow", multiplier: null));

        Assert.Equal(@"C:\apps\Ledger.exe", vm.TargetPath); // unchanged - a run is in progress
    }

    [Fact]
    public void Constructor_loads_existing_history_newest_first()
    {
        var store = new InMemorySessionHistoryStore();
        store.Append(HistoryRecord("Alpha")); // oldest
        store.Append(HistoryRecord("Beta"));  // newest

        var vm = new SessionViewModel(store);

        Assert.Equal("Beta.exe", vm.History[0].TargetName); // newest first
        Assert.Equal("Alpha.exe", vm.History[1].TargetName);
        Assert.True(vm.HasHistory);
    }

    [Fact]
    public void Record_session_prepends_to_the_panel_and_persists()
    {
        var store = new InMemorySessionHistoryStore();
        var vm = new SessionViewModel(store);
        vm.SetTarget(@"C:\apps\Ledger.exe");
        vm.Apply(Verdict("works", "verdict.works.covered"));

        vm.RecordSession();

        Assert.Equal("Ledger.exe", vm.History[0].TargetName); // prepended for the panel
        Assert.Equal("Ledger.exe", Assert.Single(store.Load()).TargetName); // persisted to the store
    }

    private static SessionRecord HistoryRecord(
        string name, string moment = "2038-01-19T03:14:07", int bias = -120,
        string mode = "multiplier", long? multiplier = 60) => new()
        {
            TargetPath = $@"C:\apps\{name}.exe",
            MomentLocal = moment,
            TzBiasMin = bias,
            Mode = mode,
            Multiplier = multiplier,
            Verdict = "works",
            EndedAtUtc = "2026-08-25T09:00:00Z",
        };

    private static VerdictEvent Verdict(string verdict, string reasonKey) => new()
    {
        V = ProtocolJson.ProtocolVersion,
        Verdict = verdict,
        ReasonKey = reasonKey,
    };

    private static CoverageEvent Coverage(int pid, string channel, long calls) => new()
    {
        V = ProtocolJson.ProtocolVersion,
        Pid = pid,
        Covered = [new CoveredChannel { Channel = channel, Calls = calls }],
    };
}
