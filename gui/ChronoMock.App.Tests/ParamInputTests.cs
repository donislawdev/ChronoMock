using ChronoMock.App.Calc;

namespace ChronoMock.App.Tests;

/// <summary>
/// A preset-parameter input (slice G4-2b): a date is null until entered (so a preset stays unfilled), a
/// duration seeds its file default and yields a value, and the label humanizes the parameter id. Pure - no
/// UI thread.
/// </summary>
public class ParamInputTests
{
    private static IReadOnlyList<UnitOption> Units() =>
    [
        new("s", "calc.unit.seconds"), new("m", "calc.unit.minutes"), new("h", "calc.unit.hours"),
        new("d", "calc.unit.days"), new("w", "calc.unit.weeks"), new("mo", "calc.unit.months"),
        new("q", "calc.unit.quarters"), new("y", "calc.unit.years"), new("bd", "calc.unit.business_days"),
    ];

    [Fact]
    public void The_default_unit_is_found_by_token_not_by_position()
    {
        // R2-N11: the fallback unit was units[3], which happened to be days. Reordering the dropdown would
        // have silently changed what a parameter with no declared unit means. Same list, reversed.
        var reversed = Units().Reverse().ToList();
        var input = new ParamInputViewModel(new PresetParameter("days", "duration", 5, null, null), reversed);

        Assert.Equal("d", input.Unit.Token);
    }

    [Fact]
    public void A_parameter_type_this_build_does_not_resolve_yields_no_value()
    {
        // R2-S8: the engine refuses an unbuilt parameter type with an honest "not built" (parse_parameter).
        // Here it fell into the duration branch, so the preset silently computed a date from an amount and
        // unit nobody entered. Null keeps the preset unfilled and the "needs parameters" note honest.
        var input = new ParamInputViewModel(new PresetParameter("count", "int", null, null, null), Units());

        Assert.False(input.IsDate);
        Assert.False(input.IsDuration);
        Assert.False(input.IsVariant);
        Assert.Null(input.ToValue());
    }

    [Fact]
    public void A_date_input_is_null_until_a_date_is_entered()
    {
        var input = new ParamInputViewModel(
            new PresetParameter("start_date", "date", null, null, "target_file_creation"), Units());

        Assert.Null(input.ToValue());

        input.DateText = "2026-01-01";
        var value = Assert.IsType<DateValue>(input.ToValue());
        Assert.Equal("2026-01-01", value.DateTimeText);
    }

    [Fact]
    public void A_duration_input_seeds_its_default_and_yields_a_duration_value()
    {
        var input = new ParamInputViewModel(
            new PresetParameter("days", "duration", 90, "business_days", null), Units());

        Assert.Equal("90", input.Amount);
        Assert.Equal("bd", input.Unit.Token); // the full "business_days" default maps to the short option

        var value = Assert.IsType<DurationValue>(input.ToValue());
        Assert.Equal("90", value.Amount);
        Assert.Equal("bd", value.UnitToken);
    }

    [Fact]
    public void A_variant_input_seeds_its_default_label_and_yields_a_variant_value()
    {
        var input = new ParamInputViewModel(
            new PresetParameter("boundary", "variant", null, null, null, "day_before"), Units());

        Assert.True(input.IsVariant);
        Assert.Equal("day_before", input.Variant.Token); // seeded from the file default

        var value = Assert.IsType<VariantValue>(input.ToValue());
        Assert.Equal("day_before", value.Label);

        input.Variant = input.VariantOptions.First(v => v.Token == "day_after");
        Assert.Equal("day_after", Assert.IsType<VariantValue>(input.ToValue()!).Label);
    }

    [Fact]
    public void The_label_humanizes_the_parameter_id()
        => Assert.Equal(
            "install date",
            new ParamInputViewModel(new PresetParameter("install_date", "date", null, null, null), Units()).Label);
}
