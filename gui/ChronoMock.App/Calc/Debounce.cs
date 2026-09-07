namespace ChronoMock.App.Calc;

/// <summary>
/// One debounced action: a burst of edits spawns one run instead of one per edit, and the newest edit
/// wins. Every edit calls <see cref="RunAsync"/>, which waits out a quiet period and then runs the action
/// unless a newer edit arrived first, in which case the older run is cancelled before it spawns anything.
/// <para>
/// It exists as a type rather than as a pair of helpers because the bug it replaces was one of ownership,
/// and ownership needs somewhere to live. The previous shape kept the pending source in a field of the
/// view model, cancelled it from one method and disposed it in another - and a run that finished
/// NORMALLY disposed its source while the field still pointed at it. The next edit then cancelled a
/// disposed source, which threw <see cref="ObjectDisposedException"/> out of the collection-changed
/// handler, so the step was added, the date was never recomputed, and the dispatcher showed
/// "The CancellationTokenSource has been disposed." Reported from use, reproduced, and now guarded by
/// <c>CalculatorDebounceTests</c>.
/// </para>
/// <para>
/// The rule that fixes it: <b>exactly one of the two runs disposes a source.</b> A run that is superseded
/// leaves its source alone, because the run that replaced it took it out of the slot and disposes it
/// there. A run that finishes while still current takes itself out of the slot first. So nothing can
/// reach a source after it has been disposed - and nothing is leaked either, which was the reason the
/// disposal was added in the first place.
/// </para>
/// </summary>
internal sealed class Debounce
{
    private readonly TimeSpan _quiet;
    private CancellationTokenSource? _pending;

    public Debounce(TimeSpan quiet) => _quiet = quiet;

    /// <summary>
    /// Wait out the quiet period and then run <paramref name="action"/>, unless a newer call supersedes
    /// this one first. Fire and forget from the caller's side - the returned task completes when this run
    /// has either finished or been superseded, and it never faults on cancellation.
    /// </summary>
    public Task RunAsync(Func<Task> action)
    {
        ArgumentNullException.ThrowIfNull(action);

        // Deliberately NOT an async method, so that this half runs synchronously on the caller's stack. A
        // fault in the bookkeeping below has to reach the edit that caused it - that is how the disposed
        // source was found at all, as an error box raised out of the collection-changed handler. Inside an
        // async method the same throw would land in a task nobody awaits and be lost (rule 6), and the
        // test that reproduces the report would pass over a broken debouncer.
        var mine = new CancellationTokenSource();

        // The token is read HERE, before this run is installed and therefore before anything can supersede
        // and dispose the source. Reading it after would be correct on the dispatcher thread and only
        // there - and a token whose source is later disposed is still usable, because Supersede always
        // cancels before it disposes, so the delay below simply comes back cancelled.
        var token = mine.Token;
        Supersede(mine);
        return RunCoreAsync(mine, token, action);
    }

    private async Task RunCoreAsync(CancellationTokenSource mine, CancellationToken token, Func<Task> action)
    {
        try
        {
            await Task.Delay(_quiet, token);
            await action();
        }
        catch (OperationCanceledException)
        {
            // A newer edit owns the result - this run never spawned anything.
        }
        finally
        {
            Release(mine);
        }
    }

    /// <summary>Install this run as the pending one and put down the run it replaces, source and all.</summary>
    private void Supersede(CancellationTokenSource mine)
    {
        var previous = Interlocked.Exchange(ref _pending, mine);
        if (previous is null)
        {
            return;
        }

        // Cancel THEN dispose, in that order and both here: cancellation callbacks run synchronously, so
        // by the time Cancel returns, the delay that run was sitting in is already cancelled and nothing
        // will touch the source again.
        previous.Cancel();
        previous.Dispose();
    }

    /// <summary>
    /// Let go of this run's source - but only if this run is still the pending one. A superseded run
    /// finds a different source in the slot and leaves it there, because <see cref="Supersede"/> already
    /// disposed the one it owns.
    /// </summary>
    private void Release(CancellationTokenSource mine)
    {
        if (Interlocked.CompareExchange(ref _pending, null, mine) == mine)
        {
            mine.Dispose();
        }
    }
}
