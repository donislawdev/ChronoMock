using System.Globalization;
using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using ChronoMock.App.Calc;
using ChronoMock.Protocol;

namespace ChronoMock.App;

/// <summary>
/// A start moment expressed relative to now - the panel's answer to the command line's
/// <c>chrono run --at +30d</c>.
///
/// <para>
/// The arithmetic is NOT done here. A relative <c>--at</c> is one shift step on the base "now", evaluated
/// by the same engine the calculator drives (crates/cli run/moment.rs), and months, quarters and years fold
/// onto the civil date rather than adding a fixed number of ticks. Re-deriving that in C# would be a second
/// source of truth for the one thing this product exists to get right, so this class only shapes the
/// question and hands it to the engine, exactly as <see cref="ScenarioMoment"/> does for a scenario.
/// </para>
/// <para>
/// The core does NOT accept a relative moment on <c>start</c> - moment_from_spec answers
/// <c>moment.unsupported_kind</c> to anything but an absolute one - so the CLI driver resolves the delta
/// before it spawns the target, and so does this. The difference from the command line is WHEN - there at
/// launch, here when the tester presses the button. In exchange the moment field and its preview show the
/// exact instant the application will be given, which a delta resolved later could not promise.
/// </para>
/// </summary>
internal static class RelativeMoment
{
    /// <summary>The units this panel offers: the calculator's list minus business days. A session carries no
    /// calendar, so <c>bd</c> can only come back as "business days need a calendar" - offering it would be
    /// offering a failure (untouchable rule 4). Derived from the calculator's list rather than retyped, so a
    /// new unit reaches both pickers at once.</summary>
    internal static IReadOnlyList<UnitOption> Units { get; } =
        [.. StepViewModel.AllUnits.Where(u => u.Token != "bd")];

    /// <summary>The engine's shift token for a sign, an amount and a unit - <c>+30d</c> - or null when the
    /// amount is not a whole number above zero. The sign is a separate control, so a minus typed INTO the
    /// amount would otherwise arrive as <c>+-30d</c>: rejected here rather than sent, because the engine's
    /// refusal would name the token and not the field the tester can fix.</summary>
    internal static string? ShiftToken(string sign, string amount, string unitToken)
    {
        if (!uint.TryParse((amount ?? string.Empty).Trim(), NumberStyles.None, CultureInfo.InvariantCulture, out uint n)
            || n == 0)
        {
            return null;
        }

        return string.Create(CultureInfo.InvariantCulture, $"{sign}{n}{unitToken}");
    }

    /// <summary>The calc invocation for "now, shifted": the same builder the calculator and the scenario
    /// path use, so all three surfaces put the same grammar on the command line. The session zone travels
    /// with it - "now plus one day" lands on a different civil date read from another zone (rule 2).</summary>
    internal static IReadOnlyList<string> BuildArgs(string shiftToken, int zoneBiasMinutes) =>
        CalculatorViewModel.BuildCalcArgs(
            BaseKind.Now,
            string.Empty,
            [["--shift", shiftToken]],
            calendarId: null,
            customFormatMask: null,
            zoneOffset: ZoneLabel.OffsetFromBiasMinutes(zoneBiasMinutes));

    /// <summary>Ask the engine what "now, shifted" is. Returns the canonical ISO moment, or the translation
    /// key naming why there is none - never both, and never neither, so the field is either filled with a
    /// real answer or left alone with a reason on screen (rule 6).</summary>
    internal static async Task<RelativeResolution> ResolveAsync(
        CalcClient? engine, IReadOnlyList<string> args)
    {
        if (engine is null)
        {
            return new RelativeResolution(null, "scenario.engine_missing");
        }

        try
        {
            var result = await engine.EvaluateAsync(args);
            var iso = result.Moment?.Iso;
            return iso is null
                ? new RelativeResolution(null, "moment.relative_failed")
                : new RelativeResolution(iso, null);
        }
        catch (CalcException)
        {
            // The engine refused the step - an amount past the year range is the reachable case.
            return new RelativeResolution(null, "moment.relative_failed");
        }
        catch (Exception ex) when (ex is IOException or InvalidOperationException
                                       or UnauthorizedAccessException
                                       or System.ComponentModel.Win32Exception)
        {
            return new RelativeResolution(null, "scenario.engine_missing");
        }
    }
}

/// <summary>What a relative moment resolved to: exactly one of a canonical ISO moment or a translation key
/// saying why there is not one.</summary>
/// <param name="Iso">The computed moment, or null when it did not resolve.</param>
/// <param name="ErrorKey">Why it did not resolve, or null when it did.</param>
internal sealed record RelativeResolution(string? Iso, string? ErrorKey);
