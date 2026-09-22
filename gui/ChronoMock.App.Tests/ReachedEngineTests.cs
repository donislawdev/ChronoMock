using ChronoMock.App;
using ChronoMock.Protocol;

namespace ChronoMock.App.Tests;

/// <summary>
/// What the model keeps of the engines the session reached, and what the copied summary says about them.
/// Pure over the view model - no window, no core.
/// </summary>
/// <remarks>
/// Its own class rather than more of <see cref="SessionViewModelTests"/>, which stands one below its
/// coupling ceiling (gui/CodeMetricsConfig.txt) - the engine record is a type, and one more type in that
/// class reddens the metrics gate. Same reason <see cref="FamilyVerdictTests"/> exists.
/// </remarks>
public class ReachedEngineTests
{
    /// <summary>The identity translator, with the one template that has holes given real text - a hole
    /// that is not filled is exactly what the summary test reads for.</summary>
    private static string T(string key) => key switch
    {
        "report.engine_line" => "{0} on port {1} (pid {2})",
        _ => key,
    };

    private static SessionVerdictEvent Verdict(IReadOnlyList<ReachedEngine> engines) => new()
    {
        V = ProtocolJson.ProtocolVersion,
        Verdict = "partial",
        ReasonKey = "session.family_partial_children",
        ProcessCount = 2,
        WarningKeys = ["embedded.web_engine_reached", "embedded.debug_port_open"],
        ContextCount = 2,
        Engines = engines,
    };

    private static ReachedEngine Engine(uint pid, int port, string browser = "Engine/153.0")
        => new() { Pid = pid, Port = port, Browser = browser };

    [Fact]
    public void The_verdict_s_engines_reach_the_model()
    {
        // Reversal probe: drop `Engines = sv.Engines` from Apply and this fails - the wire has carried the
        // list since slice C and the screen showed none of it.
        var vm = new SessionViewModel();

        vm.Apply(Verdict([Engine(4300, 61868)]));

        Assert.True(vm.HasEngines);
        var engine = Assert.Single(vm.Engines);
        Assert.Equal(61868u, (uint)engine.Port);
        Assert.Equal(4300u, engine.Pid);
    }

    [Fact]
    public void A_session_that_reached_no_engine_says_so_and_the_summary_stays_silent()
    {
        var vm = new SessionViewModel();

        vm.Apply(new SessionVerdictEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Verdict = "works",
            ReasonKey = "session.family_covered",
            ProcessCount = 2,
        });

        Assert.False(vm.HasEngines);
        Assert.Empty(vm.Engines);
        Assert.DoesNotContain("report.engines_reached", vm.BuildSummary(T), StringComparison.Ordinal);
    }

    [Fact]
    public void A_new_session_forgets_the_last_one_s_engines()
    {
        // A second session in the same window must not inherit the first one's open port: the engine is
        // gone and the port with it, and a screen still naming it would be describing a session that is
        // not this one (untouchable rule 4).
        var vm = SessionStates.Ended();
        vm.Apply(Verdict([Engine(4300, 61868)]));
        Assert.True(vm.HasEngines);

        vm.BeginNewSession();

        Assert.False(vm.HasEngines);
        Assert.Empty(vm.Engines);
    }

    [Fact]
    public void The_summary_names_the_pid_beside_the_port_the_way_the_cli_report_does()
    {
        // The screen leaves the pid out - nothing there to match it against - and the clipboard keeps it,
        // because a ticket about one engine wants it. Reversal probe: drop AppendEngines from BuildSummary
        // and this fails on the heading.
        var vm = new SessionViewModel();
        vm.SetTarget(@"C:\apps\Host.exe");
        vm.Apply(Verdict([Engine(4300, 61868), Engine(9100, 5123, "python/3.14")]));

        var summary = vm.BuildSummary(T);

        Assert.Contains("report.engines_reached:\n", summary, StringComparison.Ordinal);
        Assert.Contains("    - Engine/153.0 on port 61868 (pid 4300)\n", summary, StringComparison.Ordinal);
        Assert.Contains("    - python/3.14 on port 5123 (pid 9100)\n", summary, StringComparison.Ordinal);
    }

    [Fact]
    public void The_summary_gives_a_nameless_engine_the_word_rather_than_a_blank()
    {
        var vm = new SessionViewModel();
        vm.SetTarget(@"C:\apps\Host.exe");
        vm.Apply(Verdict([Engine(4300, 61868, string.Empty)]));

        Assert.Contains(
            "    - audit.engine_unnamed on port 61868 (pid 4300)\n", vm.BuildSummary(T), StringComparison.Ordinal);
    }

    [Fact]
    public void The_screen_and_the_copied_summary_name_the_same_engines()
    {
        // 🔴 THE GUARD FOR A RULE WRITTEN TWICE. The table folds one row per endpoint and the summary
        // prints one line per endpoint, and the two folds are separate code: sharing them would put the
        // converter's type into the view model, which is on its coupling ceiling. So this counts both
        // sides of one duplicated wire and requires them to agree.
        //
        // Reversal probe: drop the IsFirstMention check from AppendEngines and this fails 1 against 2 -
        // which is exactly the state this PR was in when review caught it.
        var vm = new SessionViewModel();
        vm.SetTarget(@"C:\apps\Host.exe");
        vm.Apply(Verdict([Engine(4300, 61868), Engine(4300, 61868), Engine(9100, 5123, "python/3.14")]));

        var rows = EngineRowsConverter.Fold(vm.Engines);
        var lines = vm.BuildSummary(T)
            .Split('\n')
            // " on port " and not "port": the warning key embedded.debug_port_open is also a line
            // beginning with a dash and containing the word, and counting it made this read 3 against 2
            // while the code under test was right.
            .Count(l => l.StartsWith("    - ", StringComparison.Ordinal) && l.Contains(" on port ", StringComparison.Ordinal));

        Assert.Equal(2, rows.Count);
        Assert.Equal(rows.Count, lines);
    }

    [Fact]
    public void An_engine_whose_name_is_only_whitespace_is_treated_as_nameless_on_both_sides()
    {
        // The core sanitises what an engine calls itself but does not trim it, and the text comes from
        // an engine inside somebody else's application - so three spaces is a name the wire can carry.
        // Untrimmed, it drew a blank cell on the screen and printed a blank in the summary.
        var vm = new SessionViewModel();
        vm.SetTarget(@"C:\apps\Host.exe");
        vm.Apply(Verdict([Engine(4300, 61868, "   ")]));

        var row = Assert.Single(EngineRowsConverter.Fold(vm.Engines));
        Assert.True(row.IsUnnamed);
        Assert.Equal(string.Empty, row.Name);
        Assert.Contains(
            "    - audit.engine_unnamed on port 61868 (pid 4300)\n", vm.BuildSummary(T), StringComparison.Ordinal);
    }

    [Fact]
    public void The_engines_stand_between_the_processes_and_the_warnings_as_they_do_in_the_cli_report()
    {
        // Order is the claim here: the two tables answer one question between them, and the warning about
        // an open debugging port comes after the list that names the port.
        var vm = new SessionViewModel();
        vm.SetTarget(@"C:\apps\Host.exe");
        vm.Apply(new SessionVerdictEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Verdict = "partial",
            ReasonKey = "session.family_partial_children",
            ProcessCount = 2,
            WarningKeys = ["embedded.debug_port_open"],
            UncoveredChildren = [new UncoveredChild { Pid = 5180, ParentPid = 4300, Image = "engine.exe", Role = "renderer" }],
            UncoveredChildrenTotal = 1,
            Engines = [Engine(4300, 61868)],
        });

        var summary = vm.BuildSummary(T);

        Assert.True(
            summary.IndexOf("report.processes_uncovered", StringComparison.Ordinal)
            < summary.IndexOf("report.engines_reached", StringComparison.Ordinal),
            "the processes come before the engines");
        Assert.True(
            summary.IndexOf("report.engines_reached", StringComparison.Ordinal)
            < summary.IndexOf("embedded.debug_port_open", StringComparison.Ordinal),
            "the engines come before the warning that names their port");
    }
}
