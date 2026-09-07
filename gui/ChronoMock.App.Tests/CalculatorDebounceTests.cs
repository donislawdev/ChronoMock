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


    [Fact]
    public async Task Typing_a_custom_format_mask_costs_one_evaluation_not_one_per_keystroke()
    {
        // The quiet period exists because every edit used to spawn a process immediately - typing a date
        // meant about ten launches, which is visible jank on a QA box with an antivirus scanner in the
        // loop. This asks whether the custom-format field got the same treatment as the rest of the
        // builder. Ten keystrokes, one evaluation.
        var (vm, launches) = NewViewModel();
        await vm.EnsureComputedAsync();
        var before = await QuietAsync(launches);

        foreach (var mask in new[] { "y", "yy", "yyy", "yyyy", "yyyy-", "yyyy-M", "yyyy-MM", "yyyy-MM-", "yyyy-MM-d", "yyyy-MM-dd" })
        {
            vm.CustomFormatMask = mask;
        }

        var after = await QuietAsync(launches);

        Assert.Equal(before + 1, after);
    }

    [Fact]
    public async Task Typing_a_preset_parameter_costs_one_evaluation_not_one_per_keystroke()
    {
        // The second undebounced path: a parameter input re-resolves the preset on every keystroke, and a
        // resolve that succeeds computes. Five keystrokes, one evaluation.
        var presets = Path.Combine(TestPaths.RepoRoot(), "presets");
        var (vm, launches) = NewViewModel(presets);
        await vm.EnsureComputedAsync();
        vm.ApplyPreset(PresetCatalog.Load(presets).Single(p => p.Id == "payment-due-business-days"));
        var before = await QuietAsync(launches);

        var input = vm.ParamInputs.Single();
        foreach (var amount in new[] { "1", "12", "123", "1234", "12345" })
        {
            input.Amount = amount;
        }

        var after = await QuietAsync(launches);

        Assert.Equal(before + 1, after);
    }

    /// <summary>
    /// Which recomputes are allowed to skip the quiet period, and why. Everything else goes through the
    /// debouncer.
    /// </summary>
    private static readonly (string Call, string Why)[] AllowedDirectCalls =
    [
        (
            "return RecomputeAsync();",
            "the first reveal of the screen. There is no burst to absorb and nothing on screen yet, so a " +
            "quarter of a second of empty result would be the only thing debouncing bought"
        ),
        (
            "_ = AnalyzeAsync();",
            "the reverse-analysis strip's own first reveal, on the same reasoning and in the same method"
        ),
    ];

    [Fact]
    public void Every_live_recompute_goes_through_the_debounce()
    {
        // Twice now a call site has been added, or left behind, outside the quiet period - the custom
        // format mask (five days older than the debounce) and the preset parameter inputs. Both cost one
        // process launch per keystroke, which is the exact jank the debounce was introduced to remove.
        // The behavioural tests above cover those two paths. This covers the next one.
        var source = File.ReadAllLines(
            Path.Combine(TestPaths.AppDirectory(), "Calc", "CalculatorViewModel.cs"));

        var direct = new List<string>();
        for (var i = 0; i < source.Length; i++)
        {
            var line = source[i];
            var code = line.Contains("//", StringComparison.Ordinal)
                ? line[..line.IndexOf("//", StringComparison.Ordinal)]
                : line;
            if (!code.Contains("RecomputeAsync()", StringComparison.Ordinal)
                && !code.Contains("AnalyzeAsync()", StringComparison.Ordinal))
            {
                continue;
            }

            if (code.Contains("RunAsync(RecomputeAsync)", StringComparison.Ordinal)
                || code.Contains("RunAsync(AnalyzeAsync)", StringComparison.Ordinal)
                || code.Contains("private async Task", StringComparison.Ordinal)
                || AllowedDirectCalls.Any(a => code.Contains(a.Call, StringComparison.Ordinal)))
            {
                continue;
            }

            direct.Add($"line {i + 1}: {code.Trim()}");
        }

        Assert.True(
            direct.Count == 0,
            "a recompute bypasses the quiet period. Either route it through the debouncer, or add it to " +
            "AllowedDirectCalls with the reason it may spawn a process per edit: " + string.Join("; ", direct));

        // The canary, and it earned its keep on the first run by firing at a threshold written from a
        // guess rather than a count. Exactly four call-shaped mentions stand in that file: the two method
        // declarations, and the two first-reveal calls registered above. The debounced call sites are not
        // among them - they pass the method as a delegate, without parentheses, which is the whole point.
        Assert.True(
            source.Count(l => l.Contains("RecomputeAsync()", StringComparison.Ordinal)
                              || l.Contains("AnalyzeAsync()", StringComparison.Ordinal)) >= 4,
            "the scan did not find the calls it is supposed to be reading - it is looking at the wrong file");
    }

    /// <summary>
    /// A view model over a client that cannot launch anything, plus the counter of how many evaluations it
    /// was asked for. The path is resolved once per evaluation and the launch then fails immediately, so
    /// no process is ever created and the recompute ends in the CalcException the view model handles.
    /// </summary>
    private static (CalculatorViewModel Vm, StrongBox<int> Launches) NewViewModel(string? presetsDir = null)
    {
        var launches = new StrongBox<int>(0);
        var client = new CalcClient(() =>
        {
            Interlocked.Increment(ref launches.Value);
            return Path.Combine(Path.GetTempPath(), "chrono-does-not-exist-here.exe");
        });
        return (new CalculatorViewModel(client, presetsDir), launches);
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
