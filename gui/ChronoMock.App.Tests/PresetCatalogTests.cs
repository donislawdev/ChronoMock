using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using ChronoMock.App.Calc;

namespace ChronoMock.App.Tests;

/// <summary>
/// The preset catalogue loader (slice G4-1a): parsing the shared preset files (schema
/// <c>chronomock.preset/1</c>) into what the calculator list needs, and reading the real bundled presets so
/// the metadata (module, parametric flag, localized name) is exercised against the checked-in files.
/// </summary>
public class PresetCatalogTests
{
    private static string PresetsDir() => Path.Combine(TestPaths.RepoRoot(), "presets");

    [Fact]
    public void Loads_the_bundled_presets_with_their_metadata()
    {
        var monthEnd = PresetCatalog.Load(PresetsDir()).Single(p => p.Id == "month-end");

        Assert.Equal("both", monthEnd.AppliesTo);
        Assert.True(monthEnd.ForCalculator);
        Assert.False(monthEnd.IsParametric);
        Assert.Equal("Last day of month", monthEnd.LocalizedName("en"));
        Assert.Equal("Ostatni dzień miesiąca", monthEnd.LocalizedName("pl"));
    }

    [Fact]
    public void Parametric_presets_are_flagged()
        => Assert.True(PresetCatalog.Load(PresetsDir()).Single(p => p.Id == "trial-last-day").IsParametric);

    [Fact]
    public void Substitution_only_presets_load_but_are_not_offered_by_the_calculator()
    {
        var rollover = PresetCatalog.Load(PresetsDir()).Single(p => p.Id == "year-rollover");

        Assert.Equal("substitution", rollover.AppliesTo);
        Assert.False(rollover.ForCalculator);
    }

    [Fact]
    public void An_unknown_culture_falls_back_to_english()
        => Assert.Equal(
            "Last day of month",
            PresetCatalog.Load(PresetsDir()).Single(p => p.Id == "month-end").LocalizedName("de"));

    [Fact]
    public void A_missing_directory_yields_an_empty_catalogue()
        => Assert.Empty(PresetCatalog.Load(Path.Combine(TestPaths.RepoRoot(), "no-such-presets")));

    [Theory]
    [InlineData("Last day of quarter", "reporting on the last day", "quarter", true)]
    [InlineData("Unix epoch zero", "treats Unix epoch zero as a real date", "epoch", true)]
    [InlineData("2038 boundary", "survives the 32-bit boundary", "EPOCH", false)]
    [InlineData("Last day of month", "month-end closing", "", true)]
    [InlineData("Payment due", "N business days out", "  business  ", true)]
    [InlineData("Payment due", "N business days out", "quarter", false)]
    public void The_filter_matches_name_or_explains_case_insensitively(
        string name, string explains, string filter, bool expected)
        => Assert.Equal(expected, CalculatorViewModel.PresetMatchesFilter(name, explains, filter));
}
