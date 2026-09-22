using ChronoMock.App;
using ChronoMock.Protocol;

namespace ChronoMock.App.Tests;

/// <summary>
/// How the panel folds a <c>coverage</c> event by the unit it speaks for (<c>coverage.kind</c>): a process
/// keeps the parent's counts, a context (a page inside the application) accumulates its own rows after
/// the parent's, and a kind this build does not know does neither - its number is not a pid, so it
/// must not become the parent, and its counts have no row to go in - while what it says about the
/// session stays. Pure over the view model - no window, no core.
/// </summary>
/// <remarks>
/// Its own class rather than more of <see cref="SessionViewModelTests"/>, which stands ON its coupling
/// ceiling (gui/CodeMetricsConfig.txt).
/// </remarks>
public class CoverageKindTests
{
    private static CoverageEvent Coverage(uint pid, string kind, string channel, long calls, params string[] warnings) => new()
    {
        V = ProtocolJson.ProtocolVersion,
        Pid = pid,
        Kind = kind,
        Covered = [new CoveredChannel { Channel = channel, Calls = calls }],
        WarningKeys = warnings,
    };

    /// <summary>
    /// Reversal probe: drop the <c>UnitProcess</c> guard from the native branch in
    /// <c>SessionViewModel.Apply</c> and the unknown kind becomes the parent - this reddens on the counts.
    /// </summary>
    [Fact]
    public void A_kind_this_build_does_not_know_is_neither_the_parent_nor_a_row()
    {
        var vm = new SessionViewModel();

        // A unit of an unknown kind arrives FIRST, with a warning and a count.
        vm.Apply(Coverage(8, "thread", "SomeChannel", 3, "some.warning"));
        // Then the real parent.
        vm.Apply(Coverage(4242, CoverageEvent.UnitProcess, "GetSystemTimeAsFileTime", 12));
        // Then a page inside the application.
        vm.Apply(Coverage(1, CoverageEvent.UnitContext, "page Date.now", 5));

        Assert.True(vm.CoverageKnown);
        Assert.Contains("some.warning", vm.Warnings);
        Assert.DoesNotContain(vm.Covered, ch => ch.Channel == "SomeChannel");
        Assert.Equal(["GetSystemTimeAsFileTime", "page Date.now"], vm.Covered.Select(ch => ch.Channel).ToArray());

        // The parent's later snapshot replaces the parent's rows and keeps the page's - and the
        // unknown unit still has no say in either.
        vm.Apply(Coverage(4242, CoverageEvent.UnitProcess, "GetSystemTimeAsFileTime", 40));
        Assert.Equal([40L, 5L], vm.Covered.Select(ch => ch.Calls).ToArray());
    }
}
