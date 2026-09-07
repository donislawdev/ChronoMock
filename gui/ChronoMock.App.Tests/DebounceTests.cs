using ChronoMock.App.Calc;

namespace ChronoMock.App.Tests;

/// <summary>
/// The debouncer's own contract, decided by the code rather than by how fast the machine is: a zero
/// quiet period settles a run on the awaited task, and superseding one cancels it at once, so every
/// assertion here follows an await rather than a sleep.
/// <para>
/// The first test is the defect this type was written for, and it is the one that must be watched
/// failing: the previous shape disposed a finished run's source while the pending slot still pointed at
/// it, so the NEXT edit cancelled a disposed source and threw
/// "The CancellationTokenSource has been disposed." out of the calculator's collection-changed handler.
/// It reproduced on the second click, every time, once the first had settled.
/// </para>
/// </summary>
public class DebounceTests
{
    /// <summary>
    /// Long enough that a run does not settle between two statements, short enough to await at the end -
    /// and awaiting it is not optional. A run left pending holds the test host open for its whole quiet
    /// period: 30 s of them cost the suite 30 s (measured), and Timeout.InfiniteTimeSpan hung it outright.
    /// </summary>
    private static readonly TimeSpan ShortQuiet = TimeSpan.FromMilliseconds(200);

    [Fact]
    public async Task A_run_that_already_finished_does_not_break_the_next_one()
    {
        var debounce = new Debounce(TimeSpan.Zero);
        await debounce.RunAsync(() => Task.CompletedTask);

        var thrown = await Record.ExceptionAsync(() => debounce.RunAsync(() => Task.CompletedTask));

        Assert.Null(thrown);
    }

    [Fact]
    public async Task Nor_the_one_after_that()
    {
        // Three in a row, because the reported failure appeared on the second click and the fix has to
        // hold for every click after it - a slot that is cleared once and then filled wrongly would pass
        // the test above and fail here.
        var debounce = new Debounce(TimeSpan.Zero);
        await debounce.RunAsync(() => Task.CompletedTask);
        await debounce.RunAsync(() => Task.CompletedTask);

        var thrown = await Record.ExceptionAsync(() => debounce.RunAsync(() => Task.CompletedTask));

        Assert.Null(thrown);
    }

    [Fact]
    public async Task Every_settled_call_runs_its_action_exactly_once()
    {
        var runs = 0;
        var debounce = new Debounce(TimeSpan.Zero);

        await debounce.RunAsync(() => { runs++; return Task.CompletedTask; });
        await debounce.RunAsync(() => { runs++; return Task.CompletedTask; });

        Assert.Equal(2, runs);
    }

    [Fact]
    public async Task A_superseded_run_never_executes_its_action()
    {
        // The whole point of the type. Without this, "does not throw" could be satisfied by a debouncer
        // that had quietly stopped debouncing, which is a process launch per keystroke.
        var runs = 0;
        var debounce = new Debounce(ShortQuiet);

        var first = debounce.RunAsync(() => { runs++; return Task.CompletedTask; });
        var second = debounce.RunAsync(() => { runs++; return Task.CompletedTask; });

        await first;
        Assert.Equal(0, runs);
        Assert.True(first.IsCompletedSuccessfully, "a superseded run must complete quietly, not fault");

        await second;
        Assert.Equal(1, runs); // and the newest one is the one that ran
    }

    [Fact]
    public async Task A_run_superseded_before_it_settled_does_not_break_the_next_one()
    {
        // The other order: cancelled first, then a fresh call. Supersede disposes the source it takes out
        // of the slot, so this is where a double dispose would show up.
        var debounce = new Debounce(ShortQuiet);
        var superseded = debounce.RunAsync(() => Task.CompletedTask);
        var replacement = debounce.RunAsync(() => Task.CompletedTask);
        await superseded;

        // This third call supersedes `replacement`, so awaiting it below costs nothing.
        var thrown = await Record.ExceptionAsync(() => debounce.RunAsync(() => Task.CompletedTask));

        await replacement;
        Assert.Null(thrown);
    }

    [Fact]
    public async Task A_failing_action_surfaces_rather_than_being_swallowed()
    {
        // Only cancellation is absorbed. An action that throws for its own reasons has to reach the
        // caller, or a broken recompute would look exactly like a debounced one (rule 6).
        var debounce = new Debounce(TimeSpan.Zero);

        await Assert.ThrowsAsync<InvalidOperationException>(
            () => debounce.RunAsync(() => throw new InvalidOperationException("boom")));
    }
}
