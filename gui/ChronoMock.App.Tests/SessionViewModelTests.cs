using System.IO;
using System.Threading.Channels;
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
            ["report.target_exit"] = "target exited with code",
            ["report.cleanup"] = "not fully cleaned up",
            ["cleanup.chromium_profile_left"] = "temp profile left behind",
            ["report.requested"] = "requested: {0} (zone {1}, mode {2})",
            ["mode.x60"] = "×60",
        };
        return key => map.TryGetValue(key, out var value) ? value : key;
    }

    /// <summary>
    /// S-12. The README promises that a "does not work" verdict stops the target instead of handing back
    /// a session whose evidence would be about the real clock. The core now refuses and says so on the
    /// wire; the panel has to show that as a terminal state, not as a running session - refuse_start was
    /// declared in the event type but never read by anything.
    /// </summary>
    [Fact]
    public void A_refused_start_is_terminal_and_unreliable_not_a_running_session()
    {
        var vm = new SessionViewModel(new InMemorySessionHistoryStore());
        vm.Apply(State("2038-01-19T03:14:07", "2026-08-24T20:30:00", bias: 0, multiplier: 1));
        Assert.True(vm.IsRunning);

        vm.Apply(Verdict("fails", "coverage.time_channels_uncovered", refuseStart: true));

        Assert.False(vm.IsRunning);
        Assert.Equal(SessionStatusKind.Refused, vm.StatusKind);
        Assert.Equal("status.refused", vm.StatusKey);
        // The summary must still lead with the unreliable-evidence banner (chrono-mock 8.8).
        Assert.StartsWith("report.unreliable_banner", vm.BuildSummary(k => k), StringComparison.Ordinal);
    }

    /// <summary>The same verdict WITHOUT the refusal (the user ticked "run even if it does not work")
    /// keeps the session running - the override has to actually override.</summary>
    [Fact]
    public void A_failing_verdict_without_refusal_keeps_the_session_running()
    {
        var vm = new SessionViewModel(new InMemorySessionHistoryStore());
        vm.Apply(State("2038-01-19T03:14:07", "2026-08-24T20:30:00", bias: 0, multiplier: 1));
        vm.Apply(Verdict("fails", "coverage.time_channels_uncovered"));

        Assert.True(vm.IsRunning);
        Assert.NotEqual(SessionStatusKind.Refused, vm.StatusKind);
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

    private static ErrorEvent Error(string key) => new()
    {
        V = ProtocolJson.ProtocolVersion,
        Id = 11, // an in-flight command id (>= FirstInFlightCommandId): a jump/set_multiplier rejection
        Code = 1,
        Key = key,
        Origin = "core",
    };

    // A start/fatal error answers the START command (id 1) or is unsolicited (no id) - never an in-flight id.
    private static ErrorEvent StartError(string key, long? id = 1) => new()
    {
        V = ProtocolJson.ProtocolVersion,
        Id = id,
        Code = 3,
        Key = key,
        Origin = "core",
    };

    [Fact]
    public void In_flight_error_while_running_is_surfaced_without_ending_the_session()
    {
        var vm = new SessionViewModel();
        vm.Apply(State("2038-01-19T03:14:07", "2026-08-24T20:30:00", bias: 0, multiplier: 60)); // -> Running

        vm.Apply(Error("moment.invalid")); // e.g. a bad in-flight jump moment

        // The one command is rejected, but the session stays live and the error is shown (rule 6).
        Assert.Equal(SessionStatusKind.Running, vm.StatusKind);
        Assert.Equal("moment.invalid", vm.InFlightErrorKey);
        Assert.True(vm.HasInFlightError);
    }

    [Fact]
    public void Error_before_the_session_is_running_is_terminal()
    {
        var vm = new SessionViewModel(); // Idle - a start-time error (bad start moment, launch failed)

        vm.Apply(StartError("target.launch_failed"));

        Assert.Equal(SessionStatusKind.Error, vm.StatusKind);
        Assert.False(vm.HasInFlightError); // not surfaced as an in-flight notice - it is fatal
    }

    [Fact]
    public void A_failed_start_while_running_is_fatal_and_shows_its_reason_not_session_ended()
    {
        // The panel enters "running" optimistically right after `start`, before any heartbeat. When
        // injection fails, the core sends error{id:1} (answering the start command) then an `ended` that is
        // clean even on failure. The error must land as a FAILURE carrying its specific reason as the
        // headline, and the trailing `ended` must NOT overwrite it to "Session ended" (RELEASE-001).
        var vm = new SessionViewModel();
        vm.SetTarget(@"C:\apps\Ledger.exe");
        vm.Apply(State("2038-01-19T03:14:07", "2026-08-24T20:30:00", bias: 0, multiplier: 60)); // optimistic running
        Assert.Equal(SessionStatusKind.Running, vm.StatusKind);

        vm.Apply(StartError("target.inject_failed"));

        Assert.Equal(SessionStatusKind.Error, vm.StatusKind);   // a failure, not still running or "ended"
        Assert.Equal("target.inject_failed", vm.StatusKey);     // the specific reason IS the headline
        Assert.False(vm.HasInFlightError);                      // never demoted to a small in-flight notice

        vm.Apply(new EndedEvent { V = ProtocolJson.ProtocolVersion, Clean = true }); // clean even on failure
        Assert.Equal(SessionStatusKind.Error, vm.StatusKind);   // the honest failure stands
        Assert.Equal("target.inject_failed", vm.StatusKey);
    }

    [Fact]
    public void An_unsolicited_error_with_no_id_is_a_fatal_start_error_not_an_in_flight_notice()
    {
        // A protocol-level failure (no command, bad command, expected start) carries no id. Even while the
        // panel is optimistically running, it is a start/fatal error, never an in-flight command rejection.
        var vm = new SessionViewModel();
        vm.Apply(State("2038-01-19T03:14:07", "2026-08-24T20:30:00", bias: 0, multiplier: 60)); // running

        vm.Apply(StartError("protocol.expected_start", id: null));

        Assert.Equal(SessionStatusKind.Error, vm.StatusKind);
        Assert.Equal("protocol.expected_start", vm.StatusKey);
        Assert.False(vm.HasInFlightError);
    }

    [Fact]
    public async Task A_non_PE_target_reads_as_unsupported_not_core_missing()
    {
        // A non-executable target fails in SessionPlan.Build (before the core spawns), so StartAsync
        // classifies it as an unsupported executable, not a broken core install (RELEASE-007). No core is
        // launched, so this runs without one.
        var vm = new SessionViewModel();
        var path = Path.Combine(Path.GetTempPath(), $"chrono-{Guid.NewGuid():N}.txt");
        File.WriteAllText(path, "not a PE");
        try
        {
            vm.SetTarget(path);
            await vm.StartAsync();

            Assert.Equal(SessionStatusKind.Error, vm.StatusKind);
            Assert.Equal("status.target_unsupported", vm.StatusKey);
            Assert.True(vm.IsIdle); // returned to idle, nothing left running
        }
        finally
        {
            File.Delete(path);
        }
    }

    [Fact]
    public async Task A_missing_target_reads_as_unreadable_not_core_missing()
    {
        var vm = new SessionViewModel();
        vm.SetTarget(Path.Combine(Path.GetTempPath(), $"chrono-missing-{Guid.NewGuid():N}.exe"));

        await vm.StartAsync();

        Assert.Equal(SessionStatusKind.Error, vm.StatusKind);
        Assert.Equal("status.target_unreadable", vm.StatusKey);
    }

    [Fact]
    public void State_syncs_the_mode_dropdown_to_the_live_multiplier()
    {
        var vm = new SessionViewModel(); // default mode is x60

        vm.Apply(State("2038-01-19T03:14:07", "2026-08-24T20:30:00", bias: 0, multiplier: 1440));

        // The dropdown reflects the live speed when it matches a preset, so it never drifts from reality.
        Assert.Equal(1440, vm.SelectedMode.Multiplier);
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
    public void A_late_ended_or_error_after_a_vanish_keeps_the_did_not_take_effect_verdict()
    {
        // The core can emit `ended` (or `error`) right after `vanished` (the target's exit). The terminal
        // DidNotTakeEffect must survive it, or the honest "did not take effect" is lost from the summary and
        // the history record (M-9) - the guard used to protect only `state`, not `ended`/`error`.
        var vm = new SessionViewModel();
        vm.SetTarget(@"C:\apps\Ledger.exe");
        vm.Apply(new VanishedEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Pid = 1234,
            ReasonKey = "target.single_instance_suspected",
            LivedMs = 10,
        });
        Assert.Equal(SessionStatusKind.DidNotTakeEffect, vm.StatusKind);

        vm.Apply(new EndedEvent { V = ProtocolJson.ProtocolVersion, Clean = true });
        Assert.Equal(SessionStatusKind.DidNotTakeEffect, vm.StatusKind); // not overwritten to Ended

        Assert.Equal("undetermined", vm.BuildRecord().Verdict); // history stays honest (rule 4)
        Assert.StartsWith("!! UNRELIABLE EVIDENCE", vm.BuildSummary(T()), StringComparison.Ordinal);
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
        Assert.True(vm.Moment.IsValid);
    }

    [Theory]
    [InlineData("2038-01-19", "03:14:07", true)]
    [InlineData("2038-01-19", "", true)]        // date only - the time defaults to midnight
    [InlineData("2038-02-29", "00:00", false)]  // 2038 is not a leap year
    [InlineData("not a date", "", false)]
    [InlineData("", "", false)]
    public void Moment_validity_follows_the_fields(string date, string time, bool valid)
    {
        var vm = new SessionViewModel();
        vm.Moment.DateText = date;
        vm.Moment.TimeText = time;
        Assert.Equal(valid, vm.Moment.IsValid);
    }

    [Fact]
    public void An_invalid_moment_disables_start_even_with_a_target()
    {
        var vm = new SessionViewModel();
        vm.SetTarget(@"C:\apps\Ledger.exe");
        Assert.True(vm.CanStart);

        vm.Moment.DateText = "nonsense";

        Assert.False(vm.Moment.IsValid);
        Assert.False(vm.CanStart);
    }

    [Fact]
    public void Build_time_maps_the_inputs_to_the_wire()
    {
        var vm = new SessionViewModel();
        vm.Moment.DateText = "2040-06-15";
        vm.Moment.TimeText = "08:30";
        vm.SelectedZone = vm.Zones.First(z => z.BiasMinutes == 300); // UTC-05:00
        vm.SelectedMode = vm.Modes.First(m => m.Mode == "frozen");

        var time = vm.BuildTime();

        Assert.Equal("absolute", time.Moment.Kind);
        Assert.Equal("2040-06-15T08:30:00", time.Moment.Local); // canonicalised to the 'T' form
        Assert.Equal(300, time.Moment.TzBiasMin);
        Assert.Equal("frozen", time.Mode);
        Assert.Null(time.Multiplier);
        Assert.False(time.ScaleDuration); // off by default
        Assert.False(time.ScaleQpc); // off by default (ADR-2)
    }

    [Fact]
    public void Build_time_carries_the_scale_duration_toggle()
    {
        // The toggle (chrono-mock 11.1 pt 4) flows to the wire so a duration-based target - a countdown,
        // an animation - can be sped up with the multiplier, not just the wall clock.
        var vm = new SessionViewModel { ScaleDuration = true };
        Assert.True(vm.BuildTime().ScaleDuration);

        vm.ScaleDuration = false;
        Assert.False(vm.BuildTime().ScaleDuration);
    }

    [Fact]
    public void Build_time_carries_the_scale_qpc_toggle()
    {
        // The QPC toggle (A3, ADR-2 reversal) flows to the wire so a QPC-based timer (Python monotonic,
        // .NET Stopwatch, Java nanoTime) accelerates with the multiplier too. Separate from scale_duration.
        var vm = new SessionViewModel { ScaleQpc = true };
        Assert.True(vm.BuildTime().ScaleQpc);
        Assert.False(vm.BuildTime().ScaleDuration); // independent of scale_duration

        vm.ScaleQpc = false;
        Assert.False(vm.BuildTime().ScaleQpc);
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
    public void Request_stop_is_a_safe_no_op_when_no_session_runs()
    {
        var vm = new SessionViewModel();

        vm.RequestStop(); // idle, no client - must not throw

        Assert.False(vm.IsRunning);
    }

    [Fact]
    public void Custom_speed_surfaces_an_error_for_a_bad_value_and_clears_it_on_a_good_one()
    {
        var vm = new SessionViewModel();
        vm.Apply(State("2038-01-19T03:14:07", "2026-09-02T00:00:00", bias: 0, multiplier: 60)); // running

        vm.SetCustomSpeed("not a number");
        Assert.Equal("speed.invalid", vm.InFlightErrorKey); // a bad value is surfaced, not a silent no-op
        Assert.True(vm.HasInFlightError);

        vm.SetCustomSpeed("500"); // a fresh, valid attempt clears the error
        Assert.False(vm.HasInFlightError);
        Assert.True(vm.IsRunning); // and never ends the session
    }

    [Fact]
    public void Custom_speed_is_a_safe_no_op_when_no_session_runs()
    {
        var vm = new SessionViewModel();

        vm.SetCustomSpeed("abc"); // idle, no client - must not throw or set an error

        Assert.False(vm.HasInFlightError);
        Assert.False(vm.IsRunning);
    }

    [Fact]
    public async Task Watchdog_fires_when_the_core_stops_emitting()
    {
        // The core beats a `state` heartbeat every ~1 s. If it goes silent (hung with the pipe still open),
        // the idle watchdog must fire so the UI does not hang in "running" forever. Modelled with a channel
        // that emits once then never completes - a short timeout stands in for the real 15 s.
        var vm = new SessionViewModel();
        var channel = Channel.CreateUnbounded<ChronoEvent>();
        await channel.Writer.WriteAsync(State("2038-01-19T03:14:07", "2026-08-26T00:00:00", bias: 0, multiplier: 60));

        var fired = await vm.ConsumeEventsAsync(channel.Reader, TimeSpan.FromMilliseconds(150));

        Assert.True(fired); // the idle watchdog fired
        Assert.Equal("2038-01-19T03:14:07", vm.Fake.Wall); // the event before the silence was still applied
    }

    [Fact]
    public async Task Watchdog_does_not_fire_when_the_stream_completes()
    {
        // A clean end (the core exited, or Stop disposed the client) completes the channel - NOT a watchdog
        // fire, even with a generous timeout, because completion is observed immediately.
        var vm = new SessionViewModel();
        var channel = Channel.CreateUnbounded<ChronoEvent>();
        await channel.Writer.WriteAsync(State("2038-01-19T03:14:07", "2026-08-26T00:00:00", bias: 0, multiplier: 60));
        channel.Writer.Complete();

        var fired = await vm.ConsumeEventsAsync(channel.Reader, TimeSpan.FromSeconds(30));

        Assert.False(fired); // completed normally
        Assert.Equal("2038-01-19T03:14:07", vm.Fake.Wall);
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
    public void Ended_surfaces_the_target_exit_code_in_state_and_summary()
    {
        // A native session whose app exited on its own carries target_exit_code (informational, not a verdict).
        var vm = new SessionViewModel();
        vm.SetTarget(@"C:\apps\Ledger.exe");
        vm.Apply(State("2038-01-19T03:14:07", "2026-08-25T00:00:00", bias: 0, multiplier: 60));
        vm.Apply(new EndedEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Clean = true,
            TargetExitCode = 3,
            FakeEndWall = "2038-01-20T00:00:00",
        });

        Assert.True(vm.HasTargetExit);
        Assert.Equal(3, vm.TargetExitCode!.Value);
        Assert.Contains("target exited with code 3", vm.BuildSummary(T()), StringComparison.Ordinal);
    }

    [Fact]
    public void Ended_from_a_stop_or_cdp_has_no_exit_code()
    {
        // A Stop/end (and every CDP session) ends with target_exit_code null: show nothing, claim nothing.
        var vm = new SessionViewModel();
        vm.SetTarget(@"C:\apps\Ledger.exe");
        vm.Apply(State("2038-01-19T03:14:07", "2026-08-25T00:00:00", bias: 0, multiplier: 60));
        vm.Apply(new EndedEvent { V = ProtocolJson.ProtocolVersion, Clean = true });

        Assert.False(vm.HasTargetExit);
        Assert.Null(vm.TargetExitCode);
        Assert.DoesNotContain("target exited with code", vm.BuildSummary(T()), StringComparison.Ordinal);
    }

    [Fact]
    public void Ended_surfaces_cleanup_residue_in_state_and_summary()
    {
        // A CDP teardown that could not remove its temp profile reports residue - shown, never hidden (rule 6).
        var vm = new SessionViewModel();
        vm.SetTarget(@"C:\apps\Electron.exe");
        vm.Apply(State("2038-01-19T03:14:07", "2026-08-25T00:00:00", bias: 0, multiplier: 60));
        vm.Apply(new EndedEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Clean = false,
            ResidueKeys = ["cleanup.chromium_profile_left"],
            FakeEndWall = "2038-01-20T00:00:00",
        });

        Assert.True(vm.HasResidue);
        Assert.Equal(["cleanup.chromium_profile_left"], vm.ResidueKeys);
        var summary = vm.BuildSummary(T());
        Assert.Contains("not fully cleaned up (1):", summary, StringComparison.Ordinal);
        Assert.Contains("temp profile left behind", summary, StringComparison.Ordinal);
    }

    [Fact]
    public void A_clean_end_leaves_no_residue()
    {
        var vm = new SessionViewModel();
        vm.SetTarget(@"C:\apps\Ledger.exe");
        vm.Apply(State("2038-01-19T03:14:07", "2026-08-25T00:00:00", bias: 0, multiplier: 60));
        vm.Apply(new EndedEvent { V = ProtocolJson.ProtocolVersion, Clean = true });

        Assert.False(vm.HasResidue);
        Assert.Empty(vm.ResidueKeys);
        Assert.DoesNotContain("not fully cleaned up", vm.BuildSummary(T()), StringComparison.Ordinal);
    }

    [Fact]
    public void A_failed_session_captures_diagnostics_and_writes_a_log()
    {
        // The main RELEASE-012 case: an injection blocked (Defender/AV). The block is captured for the Copy
        // button and the same block is written to the log file.
        var log = new RecordingDiagnosticsLog();
        var vm = new SessionViewModel(new InMemorySessionHistoryStore(), log);
        vm.SetTarget(@"C:\apps\Foo.exe");
        vm.Apply(new ErrorEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Id = 1,
            Code = 1,
            Key = "target.inject_failed",
            Origin = "core",
        });

        vm.CaptureDiagnostics(new[] { "core stderr: chrono core: inject failed: access denied" });

        Assert.True(vm.HasDiagnostics);
        Assert.Contains("Chrono Mock diagnostics", vm.DiagnosticsText, StringComparison.Ordinal);
        Assert.Contains("target.inject_failed", vm.DiagnosticsText, StringComparison.Ordinal);
        Assert.Contains(@"C:\apps\Foo.exe", vm.DiagnosticsText, StringComparison.Ordinal);
        Assert.Contains("inject failed: access denied", vm.DiagnosticsText, StringComparison.Ordinal);
        Assert.Equal(vm.DiagnosticsText, log.LastSaved); // the same block was written to the log
        Assert.Equal(@"C:\logs\diagnostics-x.log", vm.DiagnosticsSavedPath);
        Assert.True(vm.HasDiagnosticsSaved);
    }

    [Fact]
    public void A_clean_works_session_captures_no_diagnostics()
    {
        var log = new RecordingDiagnosticsLog();
        var vm = new SessionViewModel(new InMemorySessionHistoryStore(), log);
        vm.SetTarget(@"C:\apps\Foo.exe");
        vm.Apply(Verdict("works", "verdict.works.covered"));
        vm.Apply(new EndedEvent { V = ProtocolJson.ProtocolVersion, Clean = true });

        vm.CaptureDiagnostics(new[] { "core stderr: some noise" });

        Assert.False(vm.HasDiagnostics);
        Assert.Equal(string.Empty, vm.DiagnosticsText);
        Assert.Null(log.LastSaved); // nothing written on a clean success - the happy path stays quiet
        Assert.False(vm.HasDiagnosticsSaved);
    }

    [Fact]
    public void A_vanished_session_captures_a_block_even_with_no_core_output()
    {
        var vm = new SessionViewModel();
        vm.SetTarget(@"C:\apps\Foo.exe");
        vm.Apply(new VanishedEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Pid = 1,
            ReasonKey = "target.single_instance_suspected",
            LivedMs = 5,
        });

        vm.CaptureDiagnostics(Array.Empty<string>());

        Assert.True(vm.HasDiagnostics); // a vanish is not a clean success, so a block is still captured
        Assert.Contains("DidNotTakeEffect", vm.DiagnosticsText, StringComparison.Ordinal);
        Assert.Contains("(no diagnostic output)", vm.DiagnosticsText, StringComparison.Ordinal);
    }

    [Fact]
    public void The_diagnostics_block_names_the_requested_moment_zone_and_mode()
    {
        // Defaults: moment 2038-01-19T03:14:07, zone UTC+02:00, mode x60.
        var vm = new SessionViewModel();
        vm.SetTarget(@"C:\apps\Foo.exe");

        var block = vm.BuildDiagnosticsBlock(new[] { "core stderr: hi" });

        Assert.Contains("2038-01-19T03:14:07", block, StringComparison.Ordinal);
        Assert.Contains("zone UTC+02:00", block, StringComparison.Ordinal);
        Assert.Contains("mode x60", block, StringComparison.Ordinal);
        Assert.Contains("core stderr: hi", block, StringComparison.Ordinal);
    }

    [Fact]
    public void A_read_only_medium_still_captures_diagnostics_without_a_saved_path()
    {
        // The log file could not be written (read-only), but the in-memory copy behind the button stands.
        var log = new RecordingDiagnosticsLog { PathToReturn = null };
        var vm = new SessionViewModel(new InMemorySessionHistoryStore(), log);
        vm.SetTarget(@"C:\apps\Foo.exe");
        vm.Apply(new ErrorEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Id = 1,
            Code = 1,
            Key = "core.hook_dll_missing",
            Origin = "core",
        });

        vm.CaptureDiagnostics(new[] { "core stderr: hook dll missing" });

        Assert.True(vm.HasDiagnostics);        // the button still works
        Assert.False(vm.HasDiagnosticsSaved);  // but no file path to show
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
        vm.Moment.DateText = "2040-06-15";
        vm.Moment.TimeText = "08:30";
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
        Assert.Equal("2040-06-15T08:30:00", vm.Moment.Canonical);
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

    [Fact]
    public void Remove_from_history_drops_it_from_the_panel_and_the_store()
    {
        var store = new InMemorySessionHistoryStore();
        store.Append(HistoryRecord("Alpha"));
        store.Append(HistoryRecord("Beta"));
        var vm = new SessionViewModel(store);
        Assert.Equal(2, vm.History.Count);

        vm.RemoveFromHistory(vm.History.First(r => r.TargetName == "Alpha.exe"));

        Assert.Equal("Beta.exe", Assert.Single(vm.History).TargetName);
        Assert.Single(store.Load());
    }

    [Fact]
    public void Clear_history_empties_the_panel_and_the_store()
    {
        var store = new InMemorySessionHistoryStore();
        store.Append(HistoryRecord("Alpha"));
        var vm = new SessionViewModel(store);
        Assert.True(vm.HasHistory);

        vm.ClearHistory();

        Assert.Empty(vm.History);
        Assert.Empty(store.Load());
        Assert.False(vm.HasHistory);
    }

    [Fact]
    public void Record_session_caps_the_panel_at_the_maximum()
    {
        var vm = new SessionViewModel();
        vm.SetTarget(@"C:\apps\Ledger.exe");

        for (int i = 0; i < SessionHistoryLimits.Max + 5; i++)
        {
            vm.RecordSession();
        }

        Assert.Equal(SessionHistoryLimits.Max, vm.History.Count);
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

    private static VerdictEvent Verdict(string verdict, string reasonKey, bool refuseStart = false) => new()
    {
        V = ProtocolJson.ProtocolVersion,
        Verdict = verdict,
        ReasonKey = reasonKey,
        RefuseStart = refuseStart,
    };

    private static CoverageEvent Coverage(int pid, string channel, long calls) => new()
    {
        V = ProtocolJson.ProtocolVersion,
        Pid = pid,
        Covered = [new CoveredChannel { Channel = channel, Calls = calls }],
    };

    // A fake diagnostics log: records the block it was asked to save and returns a canned path (or null to
    // simulate a read-only medium), so CaptureDiagnostics is tested without touching the file system.
    private sealed class RecordingDiagnosticsLog : IDiagnosticsLog
    {
        public string? LastSaved { get; private set; }
        public string? PathToReturn { get; init; } = @"C:\logs\diagnostics-x.log";

        public string? Save(string content)
        {
            LastSaved = content;
            return PathToReturn;
        }
    }
}
