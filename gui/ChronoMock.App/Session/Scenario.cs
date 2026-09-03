using ChronoMock.App.Calc;
using ChronoMock.App.Localization;

namespace ChronoMock.App;

/// <summary>One scenario offered by the substitution panel (chrono-mock 7.1 pt 2): a named moment from the
/// shared preset catalogue, with the "what this date tests" line the catalogue author wrote.</summary>
public sealed record ScenarioItem(string Id, string DisplayName, string DisplayExplains, PresetInfo Info);

/// <summary>The scenarios the panel can offer, and how many it deliberately cannot.</summary>
public sealed record ScenarioCatalogue(IReadOnlyList<ScenarioItem> Ready, int NeedingParameters)
{
    public static readonly ScenarioCatalogue Empty = new([], 0);
}

/// <summary>
/// Reads the substitution-side view of the shared preset catalogue (docs/04 4.2). The panel offers the
/// presets it can turn into a date with one click - <c>substitution</c> or <c>both</c>, no parameters -
/// and counts the parametric ones so the panel can say they exist rather than hide them (rule 6).
/// </summary>
public static class ScenarioCatalog
{
    public static ScenarioCatalogue Load(string presetsDir)
    {
        var culture = LocalizationService.CurrentCulture;
        var forSubstitution = PresetCatalog.Load(presetsDir).Where(p => p.ForSubstitution).ToList();

        // Ordered invariantly, like every other ordering in this project (R2-N19): a current-culture sort
        // reorders the same catalogue between machines, so "the third scenario" would not be the same one.
        var ready = forSubstitution
            .Where(p => !p.IsParametric)
            .Select(p => new ScenarioItem(p.Id, p.LocalizedName(culture), p.LocalizedExplains(culture), p))
            .OrderBy(s => s.DisplayName, StringComparer.InvariantCulture)
            .ToList();

        return new ScenarioCatalogue(ready, forSubstitution.Count - ready.Count);
    }
}
