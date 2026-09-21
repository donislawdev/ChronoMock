using ChronoMock.App;
using ChronoMock.Protocol;

namespace ChronoMock.App.Tests;

/// <summary>
/// What the model keeps of the family verdict's process list, and what the copied summary says about it.
/// Pure over the view model - no window, no core.
/// </summary>
/// <remarks>
/// Its own class rather than more of <see cref="SessionViewModelTests"/>, which stands ON its coupling
/// ceiling (gui/CodeMetricsConfig.txt) - one more type in it, and this list's record is one, reddens the
/// metrics gate.
/// </remarks>
public class FamilyVerdictTests
{
    /// <summary>The identity translator: every key comes back as itself, so a summary can be read for the
    /// keys it used. The templates with holes are given real text, because a hole that is not filled is
    /// exactly what these tests read for.</summary>
    private static string T(string key) => key switch
    {
        "report.process_line" => "pid {0}: {1}, spawned by pid {2}",
        "report.process_line_role" => "pid {0}: {1} ({2}), spawned by pid {3}",
        "report.processes_more" => "and {0} more this report could not name",
        _ => key,
    };

    private static SessionVerdictEvent Verdict(IReadOnlyList<UncoveredChild> children, int total) => new()
    {
        V = ProtocolJson.ProtocolVersion,
        Verdict = "partial",
        ReasonKey = "session.family_partial_children",
        ProcessCount = 2,
        WarningKeys = ["inheritance.children_uncovered"],
        UncoveredChildren = children,
        UncoveredChildrenTotal = total,
    };

    private static UncoveredChild Child(uint pid, string? image, string? role = null)
        => new() { Pid = pid, ParentPid = 4242, Image = image, Role = role };

    [Fact]
    public void The_family_verdict_s_process_list_is_kept_with_its_true_total()
    {
        // Reversal probe: drop the two assignments under SessionVerdictEvent in Apply and this fails on Has.
        var vm = new SessionViewModel();
        vm.Apply(Verdict([Child(10, "engine.exe", "renderer"), Child(11, null)], total: 5));

        Assert.True(vm.HasUncoveredChildren);
        Assert.Equal(2, vm.UncoveredChildren.Count);
        Assert.Equal(5, vm.UncoveredChildrenTotal);
        Assert.Equal(3, vm.UncoveredChildrenUnnamed);
        Assert.True(vm.HasUnnamedUncoveredChildren);
    }

    [Fact]
    public void A_total_with_no_names_still_counts_as_processes_the_hook_never_got_into()
    {
        // Every child past the parent's ring is counted and never named - a family that fans out can arrive
        // as a total alone, and a total alone is still a fact the screen has to state (untouchable rule 4).
        var vm = new SessionViewModel();
        vm.Apply(Verdict([], total: 3));

        Assert.True(vm.HasUncoveredChildren);
        Assert.Empty(vm.UncoveredChildren);
        Assert.Equal(3, vm.UncoveredChildrenTotal);
        Assert.Equal(3, vm.UncoveredChildrenUnnamed);
    }

    [Fact]
    public void A_total_below_the_named_list_prints_the_list_s_length_and_nothing_left_unnamed()
    {
        var vm = new SessionViewModel();
        vm.Apply(Verdict([Child(10, "a.exe"), Child(11, "b.exe")], total: 1));

        Assert.Equal(2, vm.UncoveredChildrenTotal);
        Assert.Equal(0, vm.UncoveredChildrenUnnamed);
        Assert.False(vm.HasUnnamedUncoveredChildren);
    }

    [Fact]
    public void A_verdict_without_the_field_leaves_the_screen_without_the_block()
    {
        // An older core's session_verdict carries neither field: serde defaults on the wire, empty and zero
        // here, and nothing on screen claims a process ran without the hook.
        var vm = new SessionViewModel();
        vm.Apply(new SessionVerdictEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Verdict = "works",
            ReasonKey = "session.family_covered",
            ProcessCount = 2,
        });

        Assert.False(vm.HasUncoveredChildren);
        Assert.False(vm.HasUnnamedUncoveredChildren);
        Assert.DoesNotContain("report.processes_uncovered", vm.BuildSummary(T), StringComparison.Ordinal);
    }

    [Fact]
    public void A_new_session_forgets_the_last_one_s_processes()
    {
        // Every list on the model is cleared for the next session, and this one joined them: a second
        // session in the same window must not inherit the first one's real-clock processes (rule 4).
        var vm = SessionStates.Ended();
        PhaseStates.WithUncoveredProcesses(vm, total: 4);
        Assert.True(vm.HasUncoveredChildren);

        vm.BeginNewSession();

        Assert.False(vm.HasUncoveredChildren);
        Assert.Empty(vm.UncoveredChildren);
        Assert.Equal(0, vm.UncoveredChildrenTotal);
    }

    [Fact]
    public void The_summary_names_every_process_one_line_per_pid_the_way_the_cli_report_does()
    {
        // The screen groups them by executable, the clipboard keeps the pids and the parent, which is what
        // a ticket about one runaway child needs. Reversal probe: drop AppendUncoveredChildren from
        // BuildSummary and this fails on the heading.
        var vm = new SessionViewModel();
        vm.SetTarget(@"C:\apps\Host.exe");
        vm.Apply(new CoverageEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Pid = 4242,
            Covered = [new CoveredChannel { Channel = "GetSystemTimeAsFileTime", Calls = 3 }],
            InstalledLate = ["timeGetTime"],
            WarningKeys = ["wait.timeout_collapsed"],
        });
        vm.Apply(Verdict(
            [Child(10, "engine.exe", "renderer"), Child(11, "helper.exe"), Child(12, null), Child(13, string.Empty)],
            total: 6));

        var summary = vm.BuildSummary(T);

        Assert.Contains("report.processes_uncovered (6):\n", summary, StringComparison.Ordinal);
        Assert.Contains("    - pid 10: engine.exe (renderer), spawned by pid 4242\n", summary, StringComparison.Ordinal);
        Assert.Contains("    - pid 11: helper.exe, spawned by pid 4242\n", summary, StringComparison.Ordinal);
        Assert.Contains("    - pid 12: audit.process_unnamed, spawned by pid 4242\n", summary, StringComparison.Ordinal);
        // An empty name is no name: the path a runtime reports can end in a separator, the table already
        // reads it as the unnamed row, and the clipboard says the same words rather than printing a blank.
        Assert.Contains("    - pid 13: audit.process_unnamed, spawned by pid 4242\n", summary, StringComparison.Ordinal);
        Assert.Contains("    - and 2 more this report could not name\n", summary, StringComparison.Ordinal);
        // Where the CLI report puts it: after the channel lists, before the warnings that talk about it.
        Assert.True(
            summary.IndexOf("coverage.installed_late", StringComparison.Ordinal)
                < summary.IndexOf("report.processes_uncovered", StringComparison.Ordinal),
            "the process list comes before the channel lists");
        Assert.True(
            summary.IndexOf("report.processes_uncovered", StringComparison.Ordinal)
                < summary.IndexOf("coverage.warnings", StringComparison.Ordinal),
            "the process list comes after the warnings that are about it");
    }

    [Fact]
    public void The_summary_says_nothing_about_processes_when_none_ran_without_the_hook()
    {
        var vm = SessionStates.Ended();

        Assert.DoesNotContain("report.processes_uncovered", vm.BuildSummary(T), StringComparison.Ordinal);
    }
}
