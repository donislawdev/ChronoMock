using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using ChronoMock.App.Calc;

namespace ChronoMock.App.Tests;

/// <summary>
/// The reported failure: ask the calculator for business days with no calendar picked and the panel
/// printed the engine's own sentence, "chrono calc: step 1 needs a calendar - pass --calendar
/// (calc.needs_calendar)". Two things wrong with it on this screen. It names a command-line flag the
/// window has no way to pass, and it leaves the tester to work out that the fix is a picker three
/// controls away, which nothing marks.
/// <para>
/// <b>What these do not prove.</b> Nothing here runs the engine, so the step from a real failed
/// evaluation to <c>CalendarMissing</c> is not covered - the App tests deliberately launch no process,
/// and giving this one a dependency on a built Rust binary would buy less than it costs. That seam was
/// proved once on the running window instead. What is guarded here is the decision itself, the key it
/// stands on, and the mark it produces.
/// </para>
/// </summary>
public class CalculatorErrorTests
{
    /// <summary>The engine's message, verbatim, as of the run that reproduced this. Only the key inside it
    /// is load-bearing - see the mirror test below, which holds the engine to still emitting it.</summary>
    private const string EngineNeedsCalendarMessage =
        "chrono calc: step 1 needs a calendar - pass --calendar (calc.needs_calendar)";

    [Fact]
    public void The_missing_calendar_refusal_is_restated_as_the_control_that_fixes_it()
    {
        var text = WpfTestHost.Invoke(() => CalculatorViewModel.DescribeCalcError(EngineNeedsCalendarMessage));

        // The window's own words, not the engine's: no flag, no argv, and the field named so the mark on
        // it is not the only thing carrying the message.
        Assert.DoesNotContain("--calendar", text, StringComparison.Ordinal);
        Assert.DoesNotContain("chrono calc:", text, StringComparison.Ordinal);
        Assert.Contains("Calendar", text, StringComparison.Ordinal);
        Assert.True(CalculatorViewModel.IsNeedsCalendar(EngineNeedsCalendarMessage));
    }

    [Fact]
    public void Every_other_refusal_is_passed_through_word_for_word()
    {
        // The one key with a rewrite is the one whose message carries no data. The rest name a step
        // number or a year range that no key alone reproduces, so swallowing them for a tidier sentence
        // would cost the tester the part that says which step (rule 6).
        const string other = "chrono calc: step 3 overflows the representable range (calc.overflow)";

        var text = WpfTestHost.Invoke(() => CalculatorViewModel.DescribeCalcError(other));

        Assert.Equal(other, text);
        Assert.False(CalculatorViewModel.IsNeedsCalendar(other));
    }

    /// <summary>
    /// The GUI keys off a string produced in another language, in another crate, by a function whose
    /// prose is free to change. Same idea as <see cref="RustConstantMirrorTests"/> and pointed the same
    /// way: read the Rust source and fail loudly on the day one side moves alone. Without this, dropping
    /// the key from the engine's sentence would leave the picker quietly unmarked and every C# test here
    /// green, because they all feed the message in by hand.
    /// </summary>
    [Fact]
    public void The_key_the_panel_watches_for_is_still_the_key_the_engine_emits()
    {
        var path = Path.Combine(TestPaths.RepoRoot(), "crates", "cli", "src", "calc.rs");
        Assert.True(File.Exists(path), $"expected the calc surface at '{path}'");
        var source = File.ReadAllText(path);

        Assert.Contains(
            CalculatorViewModel.NeedsCalendarKey,
            source,
            StringComparison.Ordinal);
    }
}
