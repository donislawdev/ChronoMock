using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using ChronoMock.App;
using ChronoMock.Protocol;

namespace ChronoMock.App.Tests;

/// <summary>
/// What the result phase leads with, and what hangs under it, for each way a session can end. Pure over the
/// view model - no window, no core - because the headline is a decision and a decision is tested as one.
/// </summary>
/// <remarks>
/// Its own class rather than more of <see cref="SessionViewModelTests"/>, which stands one type under its
/// coupling ceiling (gui/CodeMetricsConfig.txt).
/// </remarks>
public class ResultPhaseModelTests
{
    [Fact]
    public void A_session_that_worked_leads_with_its_verdict_and_the_core_s_reason_for_it()
    {
        var vm = PhaseStates.ResultWorks();

        Assert.Equal("verdict.works", vm.ResultHeadlineKey);
        Assert.Equal(VerdictKind.Works, vm.ResultKind);
        // Unlike the footer chip, the result explains a clean works too.
        Assert.True(vm.ResultHasReason);
        Assert.False(vm.VerdictHasReason);
        Assert.Equal("session.family_covered", vm.VerdictReasonKey);
        Assert.False(vm.ResultHasMeaning);
        Assert.True(vm.ResultHasEnding);
        Assert.True(vm.HasTiming);
        Assert.False(vm.HasVanishReason);
    }

    [Fact]
    public void A_partial_session_explains_itself_twice_under_the_headline_and_not_in_the_audit()
    {
        var vm = PhaseStates.ResultPartial();

        Assert.Equal("verdict.partial", vm.ResultHeadlineKey);
        Assert.True(vm.ResultHasReason);
        Assert.True(vm.ResultHasMeaning);
        // The audit block gives the reasoning up once the session is over - it stands under the headline.
        Assert.False(vm.AuditExplainsVerdict);
        Assert.False(vm.AuditExplainsMeaning);
    }

    [Fact]
    public void The_audit_explains_the_verdict_only_while_the_session_runs()
    {
        var vm = SessionStates.Running();
        vm.Apply(new VerdictEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Verdict = "partial",
            ReasonKey = "coverage.time_channels_partial",
            RefuseStart = false,
        });

        Assert.True(vm.AuditExplainsVerdict);
        Assert.True(vm.AuditExplainsMeaning);

        vm.Apply(new EndedEvent { V = ProtocolJson.ProtocolVersion, Clean = true });

        Assert.False(vm.AuditExplainsVerdict);
        Assert.False(vm.AuditExplainsMeaning);
        Assert.True(vm.ResultHasReason);
    }

    [Fact]
    public void A_target_that_vanished_leads_with_did_not_take_effect_whatever_the_verdict_said()
    {
        var vm = PhaseStates.ResultVanished();
        // The first blink can carry a verdict of its own - the summary already overrides it, and so does this.
        vm.Apply(new VerdictEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Verdict = "works",
            ReasonKey = "coverage.time_channels_covered",
            RefuseStart = false,
        });

        Assert.Equal("result.headline_did_not_take_effect", vm.ResultHeadlineKey);
        Assert.Equal(VerdictKind.Fails, vm.ResultKind);
        Assert.False(vm.ResultHasReason);
        Assert.False(vm.ResultHasMeaning);
        // The headline is the status, so the status line gives way to how long the target lived.
        Assert.False(vm.ResultHasEnding);
        Assert.True(vm.HasVanishReason);
        Assert.Equal("target.single_instance_suspected", vm.VanishReasonKey);
        Assert.Equal(180, vm.LivedMs);
        Assert.False(vm.HasTiming);
        // The core reported what it saw in the guard window, so the audit is there and no sentence denies it.
        Assert.True(vm.CoverageKnown);
        Assert.False(vm.AuditNeverArrived);
        Assert.False(vm.AuditNeverStarted);
    }

    [Fact]
    public void A_start_that_failed_before_any_heartbeat_leads_with_did_not_start()
    {
        var vm = PhaseStates.ResultNotStarted();

        Assert.Equal("result.headline_did_not_start", vm.ResultHeadlineKey);
        Assert.Equal(VerdictKind.Fails, vm.ResultKind);
        Assert.True(vm.ResultHasEnding); // the status line is the error itself
        Assert.False(vm.HasTiming);
        Assert.True(vm.AuditNeverStarted);
        Assert.False(vm.AuditNeverArrived);
        Assert.True(vm.HasDiagnostics);
    }

    [Fact]
    public void An_error_after_the_first_heartbeat_keeps_the_verdict_and_says_the_audit_never_came()
    {
        var vm = SessionStates.Running();
        vm.Apply(new VerdictEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Verdict = "works",
            ReasonKey = "coverage.time_channels_covered",
            RefuseStart = false,
        });
        // Unsolicited (no id), so not an in-flight rejection: a fatal error that ends the session.
        vm.Apply(new ErrorEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Id = null,
            Code = 3,
            Key = "session.control_failed",
            Origin = "core",
        });

        Assert.Equal(SessionStatusKind.Error, vm.StatusKind);
        Assert.Equal("verdict.works", vm.ResultHeadlineKey);
        Assert.Equal(VerdictKind.Works, vm.ResultKind);
        Assert.True(vm.HasTiming);
        Assert.False(vm.AuditNeverStarted);
        Assert.True(vm.AuditNeverArrived);
    }

    [Fact]
    public void A_refusal_leads_with_fails_and_keeps_the_audit_the_core_sent_first()
    {
        var vm = PhaseStates.ResultRefused();

        Assert.Equal("verdict.fails", vm.ResultHeadlineKey);
        Assert.Equal(VerdictKind.Fails, vm.ResultKind);
        Assert.True(vm.ResultHasReason);
        Assert.True(vm.ResultHasMeaning);
        Assert.True(vm.ResultHasEnding);
        Assert.False(vm.HasTiming);
        Assert.True(vm.CoverageKnown);
        Assert.False(vm.AuditNeverArrived);
        Assert.False(vm.AuditNeverStarted);
        Assert.True(vm.HasDiagnosticsSaved);
    }

    [Fact]
    public void A_session_over_without_a_verdict_leads_with_no_verdict()
    {
        var vm = SessionStates.Running();
        vm.Apply(new EndedEvent { V = ProtocolJson.ProtocolVersion, Clean = true });

        Assert.Equal("result.headline_no_verdict", vm.ResultHeadlineKey);
        Assert.Equal(VerdictKind.Undetermined, vm.ResultKind);
        Assert.False(vm.ResultHasReason);
    }

    [Fact]
    public void The_fake_end_fact_prefers_the_end_timing_over_the_last_heartbeat()
    {
        var vm = PhaseStates.ResultWorks();

        // The last heartbeat said 04:15:06, the end report 04:15:07 - the report wins, the format matches the
        // contract sentence, and the zone is spelled out.
        Assert.Contains("04:15:07", vm.FakeEndPreview, StringComparison.Ordinal);
        Assert.Contains("(UTC+02:00)", vm.FakeEndPreview, StringComparison.Ordinal);
        Assert.DoesNotContain("04:15:06", vm.FakeEndPreview, StringComparison.Ordinal);
        Assert.Contains("2038", vm.FakeEndPreview, StringComparison.Ordinal);
    }

    [Fact]
    public void The_fake_end_fact_falls_back_to_the_heartbeat_when_no_end_timing_came()
    {
        var vm = SessionStates.Running(); // one heartbeat at 03:14:07, no end report
        vm.Apply(new EndedEvent { V = ProtocolJson.ProtocolVersion, Clean = true });

        Assert.Contains("03:14:07", vm.FakeEndPreview, StringComparison.Ordinal);
    }

    [Fact]
    public void The_facts_are_absent_until_a_heartbeat_arrives()
    {
        var vm = new SessionViewModel();
        Assert.False(vm.HasTiming);
        Assert.Equal(string.Empty, vm.FakeEndPreview); // the "-" placeholder is not a moment

        vm.Apply(new StateEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Fake = new Clock { Wall = "2038-01-19T03:14:07", ZoneBiasMin = 0 },
            Real = new Clock { Wall = "2026-09-09T20:30:00", ZoneBiasMin = 0 },
            Multiplier = 1,
            ElapsedFakeMs = 0,
            ElapsedRealMs = 0,
        });

        Assert.True(vm.HasTiming);
    }

    [Fact]
    public async Task The_started_at_fact_reads_the_start_snapshot_not_the_form()
    {
        // The snapshot is taken before the target is read, so a start on a missing file still captures it.
        var vm = new SessionViewModel();
        vm.SetTarget(Path.Combine(Path.GetTempPath(), $"chrono-missing-{Guid.NewGuid():N}.exe"));
        vm.Moment.DateText = "2030-06-15";
        vm.Moment.TimeText = "10:00:00";

        await vm.StartAsync();
        Assert.Equal(SessionStatusKind.Error, vm.StatusKind);

        // The form is unlocked again and gets edited - the fact must not follow it.
        vm.Moment.DateText = "2031-01-01";

        Assert.Contains("15 June 2030", vm.StartedAtPreview, StringComparison.Ordinal);
        Assert.DoesNotContain("2031", vm.StartedAtPreview, StringComparison.Ordinal);
        Assert.Contains("2031", vm.MomentPreview, StringComparison.Ordinal);
    }

    [Fact]
    public void Choosing_a_recorded_session_enables_the_actions_that_need_one()
    {
        var vm = PhaseStates.ResultWorks();
        Assert.False(vm.HasSelectedRecord);

        vm.SelectedRecord = vm.History[0];
        Assert.True(vm.HasSelectedRecord);

        vm.SelectedRecord = null;
        Assert.False(vm.HasSelectedRecord);
    }

    [Fact]
    public void A_recorded_session_states_both_of_its_moments_in_full_with_their_zones()
    {
        var record = new SessionRecord
        {
            TargetPath = @"C:\apps\sample-app.exe",
            MomentLocal = "2028-02-29T12:00:00",
            TzBiasMin = 300,
            Mode = "multiplier",
            Multiplier = 60,
            Verdict = "partial",
            EndedAtUtc = "2026-09-12T15:40:02Z",
        };

        Assert.Equal("UTC-05:00", record.ZoneText);
        Assert.Contains("29 February 2028, 12:00:00 (UTC-05:00)", record.MomentText, StringComparison.Ordinal);
        Assert.Contains("12 September 2026, 15:40:02 (UTC)", record.EndedText, StringComparison.Ordinal);
    }

    [Fact]
    public void The_copy_outcome_is_absent_until_a_copy_is_attempted()
    {
        var vm = PhaseStates.ResultWorks();
        Assert.False(vm.HasCopyFeedback);

        vm.NoteCopy(ok: false);

        Assert.True(vm.HasCopyFeedback);
        Assert.Equal("copy.failed", vm.CopyFeedbackKey);
    }
}
