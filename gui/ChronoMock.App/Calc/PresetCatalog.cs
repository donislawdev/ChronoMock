using System.IO;
using System.Text.Json;
using ChronoMock.App.Localization;

namespace ChronoMock.App.Calc;

/// <summary>
/// One preset from the shared catalogue (docs/04 4.2, schema <c>chronomock.preset/1</c>), as the calculator
/// needs it: identity, the localized framing, which module it applies to, whether it takes parameters, and
/// its moment (base + steps). The moment stays as raw JSON and is interpreted when the preset is unpacked
/// into the builder (slice G4-1b). Name and explains are DATA locales ({en, pl}), not interface keys.
/// </summary>
public sealed record PresetInfo(
    string Id,
    IReadOnlyDictionary<string, string> Name,
    IReadOnlyDictionary<string, string> Explains,
    string AppliesTo,
    string? Market,
    bool IsParametric,
    JsonElement Moment)
{
    /// <summary>Whether this preset is offered by the calculator module (<c>calculator</c> or <c>both</c>).</summary>
    public bool ForCalculator => AppliesTo is "calculator" or "both";

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
        foreach (var file in Directory.EnumerateFiles(presetsDir, "*.json"))
        {
            if (TryParse(file, out var info))
            {
                presets.Add(info);
            }
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
            var isParametric = root.TryGetProperty("parameters", out var p)
                && p.ValueKind == JsonValueKind.Array && p.GetArrayLength() > 0;

            // Clone the moment so it outlives the disposed JsonDocument (used by the unpack in G4-1b).
            var moment = root.TryGetProperty("moment", out var mo) ? mo.Clone() : default;

            info = new PresetInfo(id, ReadLocalized(root, "name"), ReadLocalized(root, "explains"),
                appliesTo, market, isParametric, moment);
            return true;
        }
        catch (Exception e) when (e is JsonException or IOException or InvalidOperationException)
        {
            return false;
        }
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
