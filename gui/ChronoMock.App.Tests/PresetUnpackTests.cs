using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using ChronoMock.App.Calc;

namespace ChronoMock.App.Tests;

/// <summary>
/// Unpacking a preset's moment into builder inputs (slice G4-1b), exercised against the real bundled
/// presets so the base/step shapes and the full-to-short token normalization are tested deterministically -
/// no UI thread, no process.
/// </summary>
public class PresetUnpackTests
{
    private static PresetInfo Preset(string id)
        => PresetCatalog.Load(Path.Combine(TestPaths.RepoRoot(), "presets")).Single(p => p.Id == id);

    [Fact]
    public void A_snap_preset_unpacks_to_today_plus_a_snap_step()
    {
        var moment = PresetUnpack.UnpackMoment(Preset("month-end").Moment);

        Assert.Equal(BaseKind.Today, moment.Base);
        var step = Assert.Single(moment.Steps);
        Assert.Equal(StepKind.Snap, step.Kind);
        Assert.Equal("eom", step.SnapToken);
    }

    [Fact]
    public void An_absolute_base_preset_unpacks_to_a_specific_date_with_no_steps()
    {
        var moment = PresetUnpack.UnpackMoment(Preset("epoch-zero").Moment);

        Assert.Equal(BaseKind.Specific, moment.Base);
        Assert.Equal("1970-01-01T00:00:00", moment.BaseText);
        Assert.Empty(moment.Steps);
    }

    [Fact]
    public void The_2038_preset_carries_its_absolute_moment()
    {
        var moment = PresetUnpack.UnpackMoment(Preset("year-2038").Moment);

        Assert.Equal(BaseKind.Specific, moment.Base);
        Assert.Equal("2038-01-19T03:14:07", moment.BaseText);
    }

    [Fact]
    public void A_shift_step_normalizes_the_full_unit_token()
    {
        // year-rollover: [snap end-of-year, shift -9 seconds] - covers snap and shift together.
        var moment = PresetUnpack.UnpackMoment(Preset("year-rollover").Moment);

        Assert.Equal(2, moment.Steps.Count);
        Assert.Equal(StepKind.Snap, moment.Steps[0].Kind);
        Assert.Equal("eoy", moment.Steps[0].SnapToken);
        Assert.Equal(StepKind.Shift, moment.Steps[1].Kind);
        Assert.Equal("-", moment.Steps[1].Sign);
        Assert.Equal("9", moment.Steps[1].Amount);
        Assert.Equal("s", moment.Steps[1].UnitToken);
    }

    [Fact]
    public void The_feb_29_preset_unpacks_to_today_plus_a_next_leap_day_nearest_step()
    {
        // feb-29: [nearest next-leap-day] - the leap-day target, pure arithmetic, needs no calendar.
        var moment = PresetUnpack.UnpackMoment(Preset("feb-29").Moment);

        Assert.Equal(BaseKind.Today, moment.Base);
        var step = Assert.Single(moment.Steps);
        Assert.Equal(StepKind.Nearest, step.Kind);
        Assert.Equal("next-leap-day", step.NearestToken);
    }

    [Fact]
    public void A_parametric_base_without_its_value_is_not_unpackable()
        => Assert.Throws<NotSupportedException>(() => PresetUnpack.UnpackMoment(Preset("trial-last-day").Moment));

    [Fact]
    public void A_duration_parameter_resolves_a_shift_from_its_value()
    {
        var values = new Dictionary<string, ParamValue> { ["days"] = new DurationValue("90", "business_days") };

        var moment = PresetUnpack.UnpackMoment(Preset("payment-due-business-days").Moment, values);

        Assert.Equal(BaseKind.Today, moment.Base);
        var step = Assert.Single(moment.Steps);
        Assert.Equal(StepKind.Shift, step.Kind);
        Assert.Equal("+", step.Sign);
        Assert.Equal("90", step.Amount);
        Assert.Equal("bd", step.UnitToken); // full "business_days" normalized to the short code
    }

    [Fact]
    public void A_date_parameter_resolves_an_absolute_base_with_its_shift_and_time()
    {
        var values = new Dictionary<string, ParamValue>
        {
            ["start_date"] = new DateValue("2026-01-01"),
            ["trial_length"] = new DurationValue("30", "days"),
        };

        var moment = PresetUnpack.UnpackMoment(Preset("trial-last-day").Moment, values);

        Assert.Equal(BaseKind.Specific, moment.Base);
        Assert.Equal("2026-01-01T00:00:00", moment.BaseText); // a bare date becomes midnight
        Assert.Equal(2, moment.Steps.Count);
        Assert.Equal(StepKind.Shift, moment.Steps[0].Kind);
        Assert.Equal("+", moment.Steps[0].Sign);
        Assert.Equal("30", moment.Steps[0].Amount);
        Assert.Equal("d", moment.Steps[0].UnitToken);
        Assert.Equal(StepKind.SetTime, moment.Steps[1].Kind);
        Assert.Equal("23:59:59", moment.Steps[1].SetTime);
    }
}
