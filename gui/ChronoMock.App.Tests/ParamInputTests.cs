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
    public void The_label_humanizes_the_parameter_id()
        => Assert.Equal(
            "install date",
            new ParamInputViewModel(new PresetParameter("install_date", "date", null, null, null), Units()).Label);
}
