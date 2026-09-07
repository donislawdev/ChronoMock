using System.IO;
using System.Runtime.CompilerServices;
using ChronoMock.App.Calc;
using ChronoMock.Protocol;

namespace ChronoMock.App.Tests;

/// <summary>
/// The reported failure, reproduced through the view model: adding a step left the date unchanged and
/// the window showed "The CancellationTokenSource has been disposed."
/// <para>
/// Both were one fault. <c>AddStep</c> and <c>RemoveStep</c> reach the recompute through
/// <c>Steps.CollectionChanged</c>, and the debouncer threw before scheduling anything - so the step was
/// added to the list, no recompute was ever queued, and the exception left the collection-changed handler
/// for the dispatcher's error box. The date not changing was not a second bug, it was the same throw seen
/// from the other side.
/// </para>
/// <para>
/// These assert the RECOMPUTE HAPPENED rather than that nothing was thrown, and the difference is not
/// cosmetic. The first draft asserted "does not throw" and PASSED against a deliberately re-broken
/// debouncer, because at that point the fix had moved the throw into a task nobody awaits - a green
/// test over the exact defect it was written for. Two things came out of that: <c>Debounce.RunAsync</c>
/// does its bookkeeping synchronously again so a fault stays loud, and these tests ask the question the
/// user actually asked - after the second edit, did the date get recomputed. <see cref="CalcClient"/>
/// calls its path callback once per evaluation, which is what makes that countable without a process
/// ever starting.
/// </para>
/// <para>
/// <b>What these do not prove.</b> Nothing here notices a leaked <c>CancellationTokenSource</c> - a
/// debouncer that cleared its slot and never disposed anything behaves identically and passes. That gap
/// is deliberate: the cost of a leak is memory, and no assertion available here can see it.
/// </para>
/// </summary>
public class CalculatorDebounceTests
{
    /// <summary>The cap on one wait, twelve times the view model's 250 ms quiet period. It bounds the
    /// test rather than timing it - the answer comes from the count holding still, not from the clock.</summary>
    private static readonly TimeSpan SettleDeadline = TimeSpan.FromSeconds(3);

    /// <summary>How long the count must hold still before it counts as settled - longer than the view
    /// model's 250 ms quiet period, so a debounced run in flight is never mistaken for none.</summary>
    private static readonly TimeSpan StableFor = TimeSpan.FromMilliseconds(350);

    private static readonly TimeSpan PollInterval = TimeSpan.FromMilliseconds(25);

    [Fact]
    public async Task Adding_a_second_step_recomputes_the_date()
    {
        var (vm, launches) = NewViewModel();
        await vm.EnsureComputedAsync();
        await QuietAsync(launches); // the first reveal computes and analyses - let that land first

        vm.AddStep();
        var afterFirst = await QuietAsync(launches);

        vm.AddStep();
        var afterSecond = await QuietAsync(launches);

        Assert.Equal(2, vm.Steps.Count);
        Assert.True(
            afterSecond > afterFirst,
            $"the second step queued no recompute - launches stayed at {afterFirst}, so the date the user " +
            "sees is the one from before the step was added");
    }

    [Fact]
    public async Task Removing_a_step_recomputes_the_date()
    {
        var (vm, launches) = NewViewModel();
        await vm.EnsureComputedAsync();
        await QuietAsync(launches); // the first reveal computes and analyses - let that land first

        vm.AddStep();
        var afterAdd = await QuietAsync(launches);

        vm.RemoveStep(vm.Steps[0]);
        var afterRemove = await QuietAsync(launches);

        Assert.Empty(vm.Steps);
        Assert.True(afterRemove > afterAdd, $"removing a step queued no recompute - launches stayed at {afterAdd}");
    }

    [Fact]
    public async Task Editing_the_reverse_analysis_field_twice_analyses_twice()
    {
        // The same debouncer drives the analysis strip, so the same fault was reachable by typing into it,
        // pausing, and typing again. Found by reading the fix rather than reported - it is the second of
        // the two call sites and nothing about it made it safer.
        var (vm, launches) = NewViewModel();
        await vm.EnsureComputedAsync();
        await QuietAsync(launches);

        vm.AnalyzeText = "2008-04-08";
        var afterFirst = await QuietAsync(launches);

        vm.AnalyzeText = "2009-05-09";
        var afterSecond = await QuietAsync(launches);

        Assert.True(afterSecond > afterFirst, $"the second edit queued no analysis - launches stayed at {afterFirst}");
    }

    /// <summary>
    /// A view model over a client that cannot launch anything, plus the counter of how many evaluations it
    /// was asked for. The path is resolved once per evaluation and the launch then fails immediately, so
    /// no process is ever created and the recompute ends in the CalcException the view model handles.
    /// </summary>
    private static (CalculatorViewModel Vm, StrongBox<int> Launches) NewViewModel()
    {
        var launches = new StrongBox<int>(0);
        var client = new CalcClient(() =>
        {
            Interlocked.Increment(ref launches.Value);
            return Path.Combine(Path.GetTempPath(), "chrono-does-not-exist-here.exe");
        });
        return (new CalculatorViewModel(client), launches);
    }

    /// <summary>
    /// Wait until the launch count has stopped moving, and return it. A quiescence check rather than a
    /// sleep: the caller compares the count before and after an edit, so a machine too slow to settle
    /// inside the deadline reports the same number twice and the assertion fails - loudly, and in the
    /// direction that says "no recompute", never in the direction that says "fine".
    /// </summary>
    private static async Task<int> QuietAsync(StrongBox<int> launches)
    {
        var deadline = DateTime.UtcNow + SettleDeadline;
        var last = -1;
        var stableSince = DateTime.UtcNow;
        while (DateTime.UtcNow < deadline && DateTime.UtcNow - stableSince < StableFor)
        {
            var now = Volatile.Read(ref launches.Value);
            if (now != last)
            {
                last = now;
                stableSince = DateTime.UtcNow;
            }

            await Task.Delay(PollInterval, TestContext.Current.CancellationToken);
        }

        return Volatile.Read(ref launches.Value);
    }
}
