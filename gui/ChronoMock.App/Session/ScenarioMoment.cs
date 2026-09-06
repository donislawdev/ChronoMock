using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using ChronoMock.App.Calc;
using ChronoMock.Protocol;

namespace ChronoMock.App;

/// <summary>
/// Turning one scenario into one moment, and nothing else.
/// <para>
/// This is the calculator half of the substitution panel, and it was living inside
/// <see cref="SessionViewModel"/> - which meant the view model that drives a session also knew about
/// <see cref="PresetUnpack"/>, <see cref="CalculatorViewModel"/> and the calculator's own failure
/// types. That is the wrong way round: the session panel offers a scenario, the calculator computes
/// the date, and the engine stays the single source of it either way (rule 16).
/// </para>
/// <para>
/// The view model keeps the properties the panel binds to, so nothing in XAML moves. What moved is
/// the computation, which is now reachable from a test without a view model at all.
/// </para>
/// </summary>
internal static class ScenarioMoment
{
    /// <summary>
    /// The <c>chrono calc</c> arguments that turn one scenario into a moment. Pure, so the two things that
    /// would only ever show up as a wrong date are asserted rather than trusted: that the preset is
    /// evaluated as explicit steps (not <c>--preset</c>, which refuses substitution-only presets), and that
    /// it is computed in the SESSION zone rather than the host's - "the end of this month" is a different
    /// day on either side of midnight, and the session runs in the zone chosen in this panel (rule 2).
    /// </summary>
    internal static IReadOnlyList<string> BuildArgs(ScenarioItem scenario, int zoneBiasMinutes)
    {
        ArgumentNullException.ThrowIfNull(scenario);
        var unpacked = PresetUnpack.UnpackMoment(scenario.Info.Moment);
        return
        [
            .. CalculatorViewModel.BuildCalcArgs(
                unpacked.Base,
                unpacked.BaseText,
                unpacked.Steps.Select(UnpackedMoment.StepArgs),
                PresetInfo.CalendarIdForMarket(scenario.Info.Market)),
            "--zone",
            ZoneLabel.OffsetFromBiasMinutes(zoneBiasMinutes),
        ];
    }

    /// <summary>
    /// Ask the engine for the scenario's moment. Returns the canonical ISO moment, or the translation key
    /// naming why there is none - never both, and never neither. A scenario that will not compute says so
    /// instead of leaving the old date in place (rule 6).
    /// <para>
    /// The preset is unpacked into explicit steps and evaluated through the ordinary calc grammar, NOT
    /// through <c>calc --preset</c>: that path gates on <c>applies_to</c> and refuses a substitution-only
    /// preset, which is exactly the half of the catalogue this panel exists to offer.
    /// </para>
    /// </summary>
    internal static async Task<ScenarioResolution> ResolveAsync(
        CalcClient? engine, ScenarioItem scenario, int zoneBiasMinutes)
    {
        ArgumentNullException.ThrowIfNull(scenario);
        if (engine is null)
        {
            return new ScenarioResolution(null, "scenario.engine_missing");
        }

        try
        {
            var result = await engine.EvaluateAsync(BuildArgs(scenario, zoneBiasMinutes));
            var iso = result.Moment?.Iso;
            return iso is null
                ? new ScenarioResolution(null, "scenario.failed")
                : new ScenarioResolution(iso, null);
        }
        catch (NotSupportedException)
        {
            // A hand-edited preset the unpacker cannot represent - one honest failure type (R2-S8).
            return new ScenarioResolution(null, "scenario.unsupported");
        }
        catch (CalcException)
        {
            return new ScenarioResolution(null, "scenario.failed");
        }
        catch (Exception ex) when (ex is IOException or InvalidOperationException
                                       or UnauthorizedAccessException
                                       or System.ComponentModel.Win32Exception)
        {
            // The engine itself could not be run (a broken or incomplete install).
            return new ScenarioResolution(null, "scenario.engine_missing");
        }
    }
}

/// <summary>
/// What a scenario resolved to: exactly one of a canonical ISO moment or a translation key saying why
/// there is not one.
/// </summary>
/// <param name="Iso">The computed moment, or null when the scenario did not resolve.</param>
/// <param name="ErrorKey">Why it did not resolve, or null when it did.</param>
internal sealed record ScenarioResolution(string? Iso, string? ErrorKey);
