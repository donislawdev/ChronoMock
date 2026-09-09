using ChronoMock.App.Calc;

namespace ChronoMock.App.Tests;

/// <summary>
/// Engine failure to interface text. The calculator runs <c>chrono calc</c> as a child process and gets
/// its refusals as an English stderr sentence - correct for a command line, wrong for a window (rule 15).
/// One case out of nine used to be translated and the other eight went on the panel raw, prefix and
/// contract key included.
/// </summary>
public sealed class CalcErrorTextTests
{
    /// <summary>A resolver in the production shape: it answers the keys it knows and echoes the rest, which
    /// is exactly what <see cref="TranslationKeyConverter"/> does for a key with no string.</summary>
    private static Func<string, string> Translator(params string[] known)
    {
        var set = new HashSet<string>(known, StringComparer.Ordinal);
        return key => set.Contains(key) ? $"[{key}]" : key;
    }

    [Theory]
    [InlineData("chrono calc: step 1 needs a calendar - pass --calendar (calc.needs_calendar)", "calc.err.needs_calendar")]
    [InlineData("chrono calc: step 3 (zone) is not built yet (calc.step_unsupported)", "calc.err.step_unsupported")]
    [InlineData("chrono calc: step 1 overflows the representable range (calc.overflow)", "calc.err.overflow")]
    [InlineData("chrono calc: unrecognised date format '31.02.2026' (calc.analyze_unrecognized)", "calc.err.analyze_unrecognized")]
    [InlineData("calc did not finish within 10 s and was stopped (calc.timeout)", "calc.err.timeout")]
    public void An_engine_key_becomes_its_translation(string stderr, string expectedKey)
        => Assert.Equal($"[{expectedKey}]", CalcErrorText.Describe(stderr, Translator(expectedKey)));

    /// <summary>
    /// The key is read off the trailing parenthesis and nothing else. Reading the PROSE instead is the
    /// mistake the missing-calendar check has always avoided, because rewording a sentence must not quietly
    /// unmap it.
    /// </summary>
    [Fact]
    public void The_key_is_the_trailing_parenthesis_not_the_prose()
    {
        Assert.Equal("calc.overflow", CalcErrorText.KeyOf("chrono calc: something (calc.overflow)"));
        Assert.Null(CalcErrorText.KeyOf("chrono calc: mentions calc.overflow in passing"));
        Assert.Null(CalcErrorText.KeyOf("chrono calc: an aside (which is prose, not a key)"));
        Assert.Null(CalcErrorText.KeyOf("chrono: calendar 'pl' not found (looked in <exe>/calendars)"));
    }

    /// <summary>
    ///🔴 Not every calculator failure carries a key - a bad moment, an unresolvable preset and a missing
    /// calendar file come out of shared code that predates them. Those stay English, which is honest. What
    /// they must NOT keep is the command-line prefix: a window has no processes in it, and "chrono calc:"
    /// tells the reader about an implementation detail instead of about their mistake.
    /// </summary>
    [Fact]
    public void A_failure_with_no_key_keeps_its_sentence_and_loses_the_command_line_prefix()
    {
        Assert.Equal(
            "calendar 'pl' not found (looked in <exe>/calendars)",
            CalcErrorText.Describe("chrono calc: calendar 'pl' not found (looked in <exe>/calendars)", Translator()));

        Assert.Equal(
            "second 60 in '2026-01-01T00:00:60' is a leap second - this model has none (use :59)",
            CalcErrorText.Describe(
                "chrono: second 60 in '2026-01-01T00:00:60' is a leap second - this model has none (use :59)",
                Translator()));
    }

    /// <summary>
    /// An engine key with no translation must not put "calc.err.something" on the panel - raw keys are the
    /// jargon this whole mapping exists to remove. It falls back to the sentence, minus the prefix and minus
    /// the key, which is still a readable English answer rather than a contract identifier.
    /// </summary>
    [Fact]
    public void An_untranslated_key_falls_back_to_the_sentence_never_to_the_key()
    {
        var text = CalcErrorText.Describe(
            "chrono calc: step 1 does something new (calc.brand_new_refusal)", Translator());

        Assert.Equal("step 1 does something new", text);
        Assert.DoesNotContain("calc.", text, StringComparison.Ordinal);
    }

    [Fact]
    public void An_empty_or_null_message_is_handled_rather_than_thrown_on()
    {
        Assert.Equal(string.Empty, CalcErrorText.Describe(string.Empty, Translator()));
        Assert.Equal(string.Empty, CalcErrorText.Describe(null!, Translator()));
    }
}
