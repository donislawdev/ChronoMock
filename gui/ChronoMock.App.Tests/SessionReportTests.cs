using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using ChronoMock.App;
using ChronoMock.Protocol;

namespace ChronoMock.App.Tests;

/// <summary>
/// The reports a session leaves behind, composed from its facts and its start setup alone - no view model,
/// no window, no core (R5-D62). The view model's own tests drive the same functions through Apply, so these
/// hold what only the seam makes reachable: a report from facts written by hand, and a clock the caller owns.
/// </summary>
public class SessionReportTests
{
    /// <summary>Keys resolve to themselves, so a test can find a line by the key that produced it.</summary>
    private static string Key(string key) => key;

    private static readonly DateTime Noon = new(2031, 5, 6, 12, 0, 0, DateTimeKind.Utc);

    /// <summary>Every field set to something no other field holds, so a field read from the wrong place
    /// shows up as the wrong value rather than as a coincidence.</summary>
    private static SessionSetup Distinct() => new(
        TargetPath: @"C:\apps\ledger.exe",
        Moment: "2031-02-03T04:05:06",
        Zone: TimeInputs.Zones.First(z => z.BiasMinutes != 0),
        Mode: TimeInputs.Modes.First(m => m.Multiplier is > 1),
        TargetArgs: "--profile two",
        WorkingFolder: @"C:\work",
        ScaleDuration: true,
        ScaleQpc: false,
        KeepBackgroundTimers: true,
        GeckoSandboxOff: true,
        Force: false,
        ReachEmbedded: false,
        ReachElevated: true);

    [Fact]
    public void The_record_carries_every_field_of_the_setup_it_was_started_with()
    {
        var setup = Distinct();
        var result = new SessionResult { VerdictKnown = true, VerdictKind = VerdictKind.Partial };

        var record = SessionReport.Record(result, setup, Noon);

        Assert.Equal(setup.TargetPath, record.TargetPath);
        Assert.Equal(setup.Moment, record.MomentLocal);
        Assert.Equal(setup.Zone.BiasMinutes, record.TzBiasMin);
        Assert.Equal(setup.Mode.Mode, record.Mode);
        Assert.Equal(setup.Mode.Multiplier, record.Multiplier);
        Assert.Equal(setup.TargetArgs, record.TargetArgs);
        Assert.Equal(setup.WorkingFolder, record.WorkingFolder);
        Assert.Equal(
            (true, false, true, true, false, false, true),
            (record.ScaleDuration, record.ScaleQpc, record.KeepBackgroundTimers, record.GeckoSandboxOff, record.Force,
                record.Embedded, record.ElevatedEmbedded));
        Assert.Equal("partial", record.Verdict);
        Assert.Equal("2031-05-06T12:00:00Z", record.EndedAtUtc);
    }

    [Fact]
    public void The_diagnostics_block_is_stamped_with_the_clock_it_is_given()
    {
        var block = SessionReport.Diagnostics(new SessionResult(), Distinct(), ["core said this"], 0, null, Noon);

        Assert.Contains("  when:      2031-05-06T12:00:00Z\n", block, StringComparison.Ordinal);
        Assert.Contains(@"  target:    C:\apps\ledger.exe", block, StringComparison.Ordinal);
        Assert.Contains("    core said this\n", block, StringComparison.Ordinal);
    }

    /// <summary>
    /// R5/6b, D2. The block names the window, the system and the mode, and the exception the window caught
    /// whole: its type and message on the error line, every further line of it indented under that line, so a
    /// stack read in a ticket stays one block.
    /// </summary>
    [Fact]
    public void The_diagnostics_block_names_the_window_the_system_and_the_whole_exception()
    {
        Exception fault;
        try
        {
            throw new InvalidOperationException("outer", new IOException("inner"));
        }
        catch (InvalidOperationException e)
        {
            fault = e;
        }

        var result = new SessionResult { CorePath = @"C:\cores\chrono.exe", IsCdp = true, Browser = "Chrome/152.0", Fault = fault };
        var block = SessionReport.Diagnostics(result, Distinct(), [], 0, null, Noon);
        var lines = block.Split('\n');

        Assert.Contains(lines, l => l.StartsWith("  gui:       " + AppVersion.Current + " (", StringComparison.Ordinal));
        Assert.Contains(lines, l => l.StartsWith("  os:        ", StringComparison.Ordinal) && l.Length > "  os:        ".Length);
        Assert.Contains("  mode:      chromium\n  browser:   Chrome/152.0\n", block, StringComparison.Ordinal);
        Assert.Contains("  error:     System.InvalidOperationException: outer\n", block, StringComparison.Ordinal);
        Assert.Contains(lines, l => l.StartsWith("              ---> System.IO.IOException: inner", StringComparison.Ordinal));
        var errorAt = Array.FindIndex(lines, l => l.StartsWith("  error:", StringComparison.Ordinal));
        var outputAt = Array.FindIndex(lines, l => l == "  core output:");
        Assert.All(lines[(errorAt + 1)..outputAt], l => Assert.StartsWith("             ", l, StringComparison.Ordinal));
    }

    /// <summary>R5/6b, D2. The core line says what became of the core the session asked for, from a session that
    /// never chose one to one that shook hands, and a block without a caught exception has no error line.</summary>
    [Theory]
    [InlineData("", false, "", "  core:      (none - the session stopped before choosing one)\n  mode:      (no plan)\n")]
    [InlineData(@"C:\c\chrono.exe", false, "", "  core:      did not start, C:\\c\\chrono.exe\n  mode:      native\n")]
    [InlineData(@"C:\c\chrono.exe", true, "", "  core:      no handshake, C:\\c\\chrono.exe\n")]
    [InlineData(@"C:\c\chrono.exe", true, "0.4.0", "  core:      0.4.0, x86, C:\\c\\chrono.exe\n")]
    public void The_core_line_says_what_became_of_the_core(string path, bool started, string version, string expected)
    {
        var result = new SessionResult { CorePath = path, CoreStarted = started, CoreVersion = version, CoreBitness = "x86" };

        var block = SessionReport.Diagnostics(result, Distinct(), [], 0, null, Noon);

        Assert.Contains(expected, block, StringComparison.Ordinal);
        Assert.DoesNotContain("  error:", block, StringComparison.Ordinal);
    }

    [Fact]
    public void A_summary_names_the_start_setup_and_the_facts_it_is_given()
    {
        var setup = Distinct();
        var result = new SessionResult
        {
            StatusKind = SessionStatusKind.Ended,
            EndedSeen = true,
            VerdictKnown = true,
            VerdictKind = VerdictKind.Works,
            VerdictLabelKey = "verdict.works",
            Warnings = ["coverage.session_clock_never_read"],
        };

        var summary = SessionReport.Summary(result, setup, "-", Key);

        // A works verdict whose warnings undo it as evidence still leads with the banner (R5-D3).
        Assert.StartsWith("report.unreliable_banner", summary, StringComparison.Ordinal);
        Assert.Contains("  report.target: ledger.exe\n", summary, StringComparison.Ordinal);
        Assert.Contains("  report.verdict: verdict.works\n", summary, StringComparison.Ordinal);
        Assert.Contains("    - coverage.session_clock_never_read\n", summary, StringComparison.Ordinal);
    }
}
