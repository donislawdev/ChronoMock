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
    public void A_duration_parameter_is_parsed_with_its_default()
    {
        var payment = PresetCatalog.Load(PresetsDir()).Single(p => p.Id == "payment-due-business-days");

        var param = Assert.Single(payment.Parameters);
        Assert.Equal("days", param.Id);
        Assert.Equal("duration", param.Type);
        Assert.Equal(90L, param.DefaultAmount);
        Assert.Equal("business_days", param.DefaultUnit);
    }

    [Fact]
    public void A_date_parameter_carries_its_hint_and_has_no_amount()
    {
        var startDate = PresetCatalog.Load(PresetsDir())
            .Single(p => p.Id == "trial-last-day").Parameters.Single(p => p.Id == "start_date");

        Assert.Equal("date", startDate.Type);
        Assert.Equal("target_file_creation", startDate.DefaultHint);
        Assert.Null(startDate.DefaultAmount);
    }

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

    [Fact]
    public void A_bad_amount_does_not_crash_the_whole_catalogue()
    {
        // RELEASE-011: a preset whose default.amount is out of Int32 range or fractional must not take down
        // the whole list. GetInt32 threw FormatException/OverflowException the catch did not cover; now the
        // amount is read as i64 (TryGetInt64, no throw), and a fractional one leaves the default unset.
        var dir = Path.Combine(Path.GetTempPath(), $"chrono-presets-{Guid.NewGuid():N}");
        Directory.CreateDirectory(dir);
        try
        {
            File.WriteAllText(Path.Combine(dir, "good.json"), Preset("good", "5"));
            File.WriteAllText(Path.Combine(dir, "huge.json"), Preset("huge", "3000000000")); // > Int32.MaxValue
            File.WriteAllText(Path.Combine(dir, "fractional.json"), Preset("fractional", "90.5"));

            var loaded = PresetCatalog.Load(dir);

            Assert.Equal(3, loaded.Count); // one bad file never removes the others
            Assert.Equal(5L, loaded.Single(p => p.Id == "good").Parameters[0].DefaultAmount);
            Assert.Equal(3_000_000_000L, loaded.Single(p => p.Id == "huge").Parameters[0].DefaultAmount); // i64, not Int32
            Assert.Null(loaded.Single(p => p.Id == "fractional").Parameters[0].DefaultAmount); // fractional -> unset
        }
        finally
        {
            Directory.Delete(dir, recursive: true);
        }
    }

    [Fact]
    public void A_file_whose_schema_this_build_does_not_read_is_not_offered()
    {
        // R2-S8: the engine's parse_preset refuses anything but chronomock.preset/1, and this reader did not
        // look at the field at all - so a preset/2 file was listed in the calculator and failed later, deep
        // in the engine, answering a schema question with a date error. The preset keys are a public
        // contract (untouchable rule 17), so the version gates the file here too.
        var dir = Path.Combine(Path.GetTempPath(), $"chrono-presets-{Guid.NewGuid():N}");
        Directory.CreateDirectory(dir);
        try
        {
            File.WriteAllText(Path.Combine(dir, "good.json"), Preset("good", "5"));
            File.WriteAllText(
                Path.Combine(dir, "future.json"),
                Preset("future", "5").Replace("chronomock.preset/1", "chronomock.preset/2", StringComparison.Ordinal));
            File.WriteAllText(
                Path.Combine(dir, "schemaless.json"),
                Preset("schemaless", "5").Replace("\"schema\":\"chronomock.preset/1\",", "", StringComparison.Ordinal));

            // The readable one still loads - a refused neighbour never takes the list down (rule 6).
            Assert.Equal("good", Assert.Single(PresetCatalog.Load(dir)).Id);
        }
        finally
        {
            Directory.Delete(dir, recursive: true);
        }
    }

    // Built by concatenation, not an interpolated raw string: the JSON's own "}}" runs collide with the
    // interpolation delimiter.
    private static string Preset(string id, string amount) =>
        "{\"schema\":\"chronomock.preset/1\",\"id\":\"" + id + "\",\"applies_to\":\"calculator\"," +
        "\"name\":{\"en\":\"" + id + "\"},\"explains\":{\"en\":\"x\"}," +
        "\"parameters\":[{\"id\":\"days\",\"type\":\"duration\"," +
        "\"default\":{\"amount\":" + amount + ",\"unit\":\"days\"}}]," +
        "\"moment\":{\"base\":\"today\",\"steps\":[]}}";

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
