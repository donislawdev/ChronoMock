using System.Globalization;
using System.Text.Json;

namespace ChronoMock.App.Calc;

/// <summary>A resolved value for a preset parameter, supplied to <see cref="PresetUnpack.UnpackMoment"/> to
/// fill a parametric base or shift.</summary>
public abstract record ParamValue;

/// <summary>A date value for a <c>date</c> parameter (becomes an absolute base). A bare date is midnight.</summary>
public sealed record DateValue(string DateTimeText) : ParamValue;

/// <summary>A magnitude and unit for a <c>duration</c> parameter (becomes a shift; the sign is the step's).</summary>
public sealed record DurationValue(string Amount, string UnitToken) : ParamValue;

/// <summary>A preset's moment expressed as builder inputs: the base and a list of steps ready to configure
/// <see cref="StepViewModel"/>s. Each step carries the value(s) its kind reads; the others keep the same
/// defaults a fresh step has.</summary>
public sealed record UnpackedStep(
    StepKind Kind,
    string Sign = "+",
    string Amount = "1",
    string UnitToken = "d",
    string SnapToken = "eom",
    string NearestToken = "nbd",
    string SetTime = "23:59:59",
    string ZoneOffset = "+00:00");

/// <summary>A preset's moment as builder inputs (base + steps), produced by <see cref="PresetUnpack"/>.</summary>
public sealed record UnpackedMoment(BaseKind Base, string BaseText, IReadOnlyList<UnpackedStep> Steps);

/// <summary>
/// Translates a preset's <c>moment</c> (raw JSON, schema <c>chronomock.preset/1</c>) into builder inputs so
/// a click fills the constructor (7.3). This keeps ONE source of truth - the builder - and reuses the whole
/// live-recompute pipeline; there is no separate preset compute path. Pure and unit-tested. A parametric
/// piece (a <c>{ "parameter": ... }</c> base or shift) or an unrecognized shape throws
/// <see cref="NotSupportedException"/>: the caller checks the parametric flag first and never fills the
/// builder from a preset it cannot represent honestly (rule 6).
/// </summary>
public static class PresetUnpack
{
    private static readonly IReadOnlyDictionary<string, ParamValue> NoParameters =
        new Dictionary<string, ParamValue>();

    public static UnpackedMoment UnpackMoment(
        JsonElement moment, IReadOnlyDictionary<string, ParamValue>? parameters = null)
    {
        var values = parameters ?? NoParameters;
        var (baseKind, baseText) = ParseBase(moment.GetProperty("base"), values);

        var steps = new List<UnpackedStep>();
        if (moment.TryGetProperty("steps", out var stepsEl) && stepsEl.ValueKind == JsonValueKind.Array)
        {
            foreach (var step in stepsEl.EnumerateArray())
            {
                steps.Add(ParseStep(step, values));
            }
        }

        return new UnpackedMoment(baseKind, baseText, steps);
    }

    private static (BaseKind, string) ParseBase(JsonElement baseEl, IReadOnlyDictionary<string, ParamValue> values)
    {
        if (baseEl.ValueKind == JsonValueKind.String)
        {
            return baseEl.GetString() switch
            {
                "today" => (BaseKind.Today, string.Empty),
                "now" => (BaseKind.Now, string.Empty),
                var other => throw new NotSupportedException($"unknown base '{other}'"),
            };
        }

        if (baseEl.ValueKind == JsonValueKind.Object)
        {
            if (baseEl.TryGetProperty("absolute", out var abs) && abs.ValueKind == JsonValueKind.String)
            {
                return (BaseKind.Specific, abs.GetString()!);
            }

            if (baseEl.TryGetProperty("parameter", out var pn))
            {
                var id = pn.GetString()!;
                if (values.TryGetValue(id, out var value) && value is DateValue date)
                {
                    return (BaseKind.Specific, NormalizeDate(date.DateTimeText));
                }

                throw new NotSupportedException($"base parameter '{id}' has no date value");
            }
        }

        throw new NotSupportedException("unrecognized base");
    }

    private static UnpackedStep ParseStep(JsonElement step, IReadOnlyDictionary<string, ParamValue> values)
    {
        var property = step.EnumerateObject().First();
        return property.Name switch
        {
            "snap" => new UnpackedStep(StepKind.Snap, SnapToken: NormalizeSnap(property.Value.GetString()!)),
            "shift" => ParseShift(property.Value, values),
            "set_time" => new UnpackedStep(StepKind.SetTime, SetTime: property.Value.GetString()!),
            "nearest" => new UnpackedStep(StepKind.Nearest, NearestToken: NormalizeNearest(property.Value.GetString()!)),
            "to_zone" or "zone" => new UnpackedStep(StepKind.Zone, ZoneOffset: property.Value.GetString()!),
            var other => throw new NotSupportedException($"unknown step '{other}'"),
        };
    }

    private static UnpackedStep ParseShift(JsonElement shift, IReadOnlyDictionary<string, ParamValue> values)
    {
        var sign = shift.GetProperty("sign").GetString()!;

        if (shift.TryGetProperty("parameter", out var pn))
        {
            var id = pn.GetString()!;
            if (values.TryGetValue(id, out var value) && value is DurationValue duration)
            {
                return new UnpackedStep(
                    StepKind.Shift, Sign: sign, Amount: duration.Amount, UnitToken: NormalizeUnit(duration.UnitToken));
            }

            throw new NotSupportedException($"shift parameter '{id}' has no duration value");
        }

        var amount = shift.GetProperty("amount").GetInt64().ToString(CultureInfo.InvariantCulture);
        var unit = NormalizeUnit(shift.GetProperty("unit").GetString()!);
        return new UnpackedStep(StepKind.Shift, Sign: sign, Amount: amount, UnitToken: unit);
    }

    // A bare date is midnight, matching the engine's parse_param_date so a date parameter resolves the same.
    private static string NormalizeDate(string value)
        => value.Contains('T', StringComparison.Ordinal) || value.Contains(' ', StringComparison.Ordinal)
            ? value
            : $"{value}T00:00:00";

    // The preset files carry full-form tokens (end-of-month, seconds); the builder options use the short
    // codes (eom, s). Normalize to short so the builder can select the matching option. Both forms accepted.
    private static string NormalizeSnap(string token) => token switch
    {
        "start-of-month" or "som" => "som",
        "end-of-month" or "eom" => "eom",
        "start-of-quarter" or "soq" => "soq",
        "end-of-quarter" or "eoq" => "eoq",
        "start-of-year" or "soy" => "soy",
        "end-of-year" or "eoy" => "eoy",
        _ => throw new NotSupportedException($"unknown snap target '{token}'"),
    };

    private static string NormalizeUnit(string unit) => unit switch
    {
        "seconds" or "s" => "s",
        "minutes" or "m" => "m",
        "hours" or "h" => "h",
        "days" or "d" => "d",
        "weeks" or "w" => "w",
        "months" or "mo" => "mo",
        "quarters" or "q" => "q",
        "years" or "y" => "y",
        "business_days" or "bd" => "bd",
        _ => throw new NotSupportedException($"unknown unit '{unit}'"),
    };

    private static string NormalizeNearest(string target) => target switch
    {
        "next-business-day" or "nbd" => "nbd",
        "prev-business-day" or "previous-business-day" or "pbd" => "pbd",
        _ => throw new NotSupportedException($"unknown nearest target '{target}'"),
    };
}
