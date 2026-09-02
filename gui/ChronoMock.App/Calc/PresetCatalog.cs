using System.IO;
using System.Text.Json;
using ChronoMock.App.Localization;

namespace ChronoMock.App.Calc;

/// <summary>One preset parameter (docs/04 4.2): its id, type (<c>date</c> or <c>duration</c>), the file
/// default (a duration's amount and unit), and the default hint (e.g. <c>target_file_creation</c>, which is
/// resolvable only in substitution where a target exists - never in the calculator).</summary>
public sealed record PresetParameter(
    string Id,
    string Type,
    long? DefaultAmount, // i64, matching the CLI/core contract (docs/04 4.2) - not Int32 (RELEASE-011)
    string? DefaultUnit,
    string? DefaultHint);

/// <summary>
/// One preset from the shared catalogue (docs/04 4.2, schema <c>chronomock.preset/1</c>), as the calculator
/// needs it: identity, the localized framing, which module it applies to, its parameters, and its moment
/// (base + steps). The moment stays as raw JSON and is interpreted when the preset is unpacked into the
/// builder (slice G4-1b). Name and explains are DATA locales ({en, pl}), not interface keys.
/// </summary>
public sealed record PresetInfo(
    string Id,
    IReadOnlyDictionary<string, string> Name,
    IReadOnlyDictionary<string, string> Explains,
    string AppliesTo,
    string? Market,
    IReadOnlyList<PresetParameter> Parameters,
    JsonElement Moment)
{
    /// <summary>Whether this preset is offered by the calculator module (<c>calculator</c> or <c>both</c>).</summary>
    public bool ForCalculator => AppliesTo is "calculator" or "both";

    /// <summary>Whether this preset takes parameters (its moment refers to them by id).</summary>
    public bool IsParametric => Parameters.Count > 0;

    /// <summary>The name in the given culture, falling back to the default culture (English).</summary>
    public string LocalizedName(string culture) => Localized(Name, culture);

    /// <summary>The "what this date tests" framing in the given culture, English fallback.</summary>
    public string LocalizedExplains(string culture) => Localized(Explains, culture);

    private static string Localized(IReadOnlyDictionary<string, string> map, string culture)
        => map.TryGetValue(culture, out var value) ? value
            : map.TryGetValue(LocalizationService.DefaultCulture, out var fallback) ? fallback
            : string.Empty;
}

/// <summary>
/// Reads the shared preset catalogue from disk. A preset is a named moment expression (docs/04 4.2); the
/// calculator is a consumer of that contract, mirroring only what the list and the builder need. A file
/// that fails to parse is skipped rather than taking down the whole list (rule 6 - an honest partial list
/// beats a crash), and a missing directory yields an empty catalogue.
/// </summary>
public static class PresetCatalog
{
    public static IReadOnlyList<PresetInfo> Load(string presetsDir)
    {
        if (!Directory.Exists(presetsDir))
        {
            return [];
        }

        var presets = new List<PresetInfo>();
        try
        {
            foreach (var file in Directory.EnumerateFiles(presetsDir, "*.json"))
            {
                if (TryParse(file, out var info))
                {
                    presets.Add(info);
                }
            }
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            // The directory became unreadable mid-enumeration (permissions, a race with a delete) - return
            // what parsed so far rather than crash the calculator screen on reveal (L-15, rule 6). Per-file
            // parse errors are already swallowed inside TryParse.
        }

        return presets;
    }

    private static bool TryParse(string file, out PresetInfo info)
    {
        info = null!;
        try
        {
            using var doc = JsonDocument.Parse(File.ReadAllText(file));
            var root = doc.RootElement;

            var id = root.TryGetProperty("id", out var idEl) ? idEl.GetString() : null;
            if (string.IsNullOrEmpty(id))
            {
                return false;
            }

            var appliesTo = root.TryGetProperty("applies_to", out var a) ? a.GetString() ?? "both" : "both";
            string? market = root.TryGetProperty("market", out var m) && m.ValueKind == JsonValueKind.String
                ? m.GetString()
                : null;

            // Clone the moment so it outlives the disposed JsonDocument (used by the unpack in G4-1b).
            var moment = root.TryGetProperty("moment", out var mo) ? mo.Clone() : default;

            info = new PresetInfo(id, ReadLocalized(root, "name"), ReadLocalized(root, "explains"),
                appliesTo, market, ReadParameters(root), moment);
            return true;
        }
        catch (Exception e) when (e is JsonException or IOException or InvalidOperationException
                                      or FormatException or OverflowException)
        {
            // Skip one malformed file rather than take down the whole list (rule 6). FormatException /
            // OverflowException are defensive: a numeric accessor on an unexpected node could throw them, so
            // a hostile preset in a shared catalogue cannot crash the calculator's preset list (RELEASE-011).
            return false;
        }
    }

    private static IReadOnlyList<PresetParameter> ReadParameters(JsonElement root)
    {
        var list = new List<PresetParameter>();
        if (root.TryGetProperty("parameters", out var arr) && arr.ValueKind == JsonValueKind.Array)
        {
            foreach (var p in arr.EnumerateArray())
            {
                var id = p.TryGetProperty("id", out var idEl) ? idEl.GetString() : null;
                var type = p.TryGetProperty("type", out var typeEl) ? typeEl.GetString() : null;
                if (string.IsNullOrEmpty(id) || string.IsNullOrEmpty(type))
                {
                    continue;
                }

                long? defaultAmount = null;
                string? defaultUnit = null;
                if (p.TryGetProperty("default", out var def) && def.ValueKind == JsonValueKind.Object)
                {
                    // Read as i64 to match the contract (the CLI and core read amount as i64) and to never
                    // throw: TryGetInt64 returns false for a fractional or out-of-range number, leaving the
                    // default unset instead of crashing the whole catalogue on one bad file (RELEASE-011).
                    if (def.TryGetProperty("amount", out var amount) && amount.ValueKind == JsonValueKind.Number
                        && amount.TryGetInt64(out var amt))
                    {
                        defaultAmount = amt;
                    }

                    if (def.TryGetProperty("unit", out var unit) && unit.ValueKind == JsonValueKind.String)
                    {
                        defaultUnit = unit.GetString();
                    }
                }

                var hint = p.TryGetProperty("default_hint", out var dh) && dh.ValueKind == JsonValueKind.String
                    ? dh.GetString()
                    : null;
                list.Add(new PresetParameter(id, type, defaultAmount, defaultUnit, hint));
            }
        }

        return list;
    }

    private static IReadOnlyDictionary<string, string> ReadLocalized(JsonElement root, string property)
    {
        var map = new Dictionary<string, string>(StringComparer.Ordinal);
        if (root.TryGetProperty(property, out var el) && el.ValueKind == JsonValueKind.Object)
        {
            foreach (var member in el.EnumerateObject())
            {
                if (member.Value.ValueKind == JsonValueKind.String)
                {
                    map[member.Name] = member.Value.GetString()!;
                }
            }
        }

        return map;
    }
}
