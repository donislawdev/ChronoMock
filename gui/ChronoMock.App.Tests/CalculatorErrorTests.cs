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

    /// <summary>
    /// 🔴 This test used to assert the OPPOSITE, and its reason was sound: a refusal names a step number or
    /// a year range that no key alone reproduces, so swallowing the sentence for a tidier one would cost
    /// the tester the part that says which step (rule 6). What was wrong was the conclusion drawn from it -
    /// that the whole English sentence should therefore BE the message. In a Polish window it was the wrong
    /// language, carried the name of a process the user is not supposed to know runs, and ended in a raw
    /// contract key.
    ///
    /// Both halves are kept now: the translated key is the message, and the engine's sentence is a quieter
    /// line beneath it. Nothing is parsed out of the prose, which is what the key exists to avoid.
    /// </summary>
    [Fact]
    public void Every_refusal_is_translated_and_keeps_the_engine_sentence_as_detail()
    {
        const string other = "chrono calc: step 3 overflows the representable range (calc.overflow)";

        var text = WpfTestHost.Invoke(() => CalculatorViewModel.DescribeCalcError(other));
        var detail = WpfTestHost.Invoke(() => CalculatorViewModel.DetailForCalcError(other));

        // The message is the window's own words - no argv, no key.
        Assert.DoesNotContain("chrono calc:", text, StringComparison.Ordinal);
        Assert.DoesNotContain("calc.overflow", text, StringComparison.Ordinal);
        Assert.NotEqual(other, text);

        // The detail keeps what the key cannot carry: WHICH step.
        Assert.Contains("step 3", detail, StringComparison.Ordinal);
        Assert.DoesNotContain("chrono calc:", detail, StringComparison.Ordinal);
        Assert.DoesNotContain("calc.overflow", detail, StringComparison.Ordinal);

        Assert.False(CalculatorViewModel.IsNeedsCalendar(other));
    }

    /// <summary>A refusal whose sentence adds nothing beyond the translation shows one line, not the same
    /// thing twice - the duplicate-line shape rule 24 caught in the About window.</summary>
    [Fact]
    public void A_refusal_with_nothing_extra_to_say_shows_no_second_line()
    {
        // No key, so the message IS the sentence - a second copy of it underneath would be noise.
        const string keyless = "chrono calc: calendar 'pl' not found (looked in <exe>/calendars)";

        var text = WpfTestHost.Invoke(() => CalculatorViewModel.DescribeCalcError(keyless));
        var detail = WpfTestHost.Invoke(() => CalculatorViewModel.DetailForCalcError(keyless));

        Assert.Equal("calendar 'pl' not found (looked in <exe>/calendars)", text);
        Assert.Equal(string.Empty, detail);
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
