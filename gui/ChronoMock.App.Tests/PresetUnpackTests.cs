using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using System.Text.Json;
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
    public void The_age_of_majority_preset_unpacks_a_variant_into_a_signless_shift()
    {
        // birth_date base, then +18y, then the boundary variant (day_before -> "- 1 day", its own sign).
        var values = new Dictionary<string, ParamValue>
        {
            ["birth_date"] = new DateValue("2008-03-15"),
            ["boundary"] = new VariantValue("day_before"),
        };

        var moment = PresetUnpack.UnpackMoment(Preset("age-of-majority").Moment, values);

        Assert.Equal(BaseKind.Specific, moment.Base);
        Assert.Equal("2008-03-15T00:00:00", moment.BaseText);
        Assert.Equal(2, moment.Steps.Count);
        Assert.Equal(("+", "18", "y"), (moment.Steps[0].Sign, moment.Steps[0].Amount, moment.Steps[0].UnitToken));
        Assert.Equal(StepKind.Shift, moment.Steps[1].Kind);
        Assert.Equal(("-", "1", "d"), (moment.Steps[1].Sign, moment.Steps[1].Amount, moment.Steps[1].UnitToken));
    }

    [Theory]
    [InlineData("on_day", "+", "0")]
    [InlineData("day_after", "+", "1")]
    public void A_variant_resolves_to_its_signed_day_offset(string label, string sign, string amount)
    {
        var values = new Dictionary<string, ParamValue>
        {
            ["birth_date"] = new DateValue("2008-03-15"),
            ["boundary"] = new VariantValue(label),
        };

        var moment = PresetUnpack.UnpackMoment(Preset("age-of-majority").Moment, values);

        Assert.Equal((sign, amount, "d"), (moment.Steps[1].Sign, moment.Steps[1].Amount, moment.Steps[1].UnitToken));
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

    /// <summary>
    /// R2-S8. The class contract names ONE failure type, and the caller catches by type. Reading the raw
    /// accessors meant a hand-edited preset threw whatever it happened to hit: a JSON null where a name
    /// belongs produced an ArgumentNullException the caller does not catch, which reached the dispatcher's
    /// last-resort message box, and a JSON null in set_time threw nothing at all and quietly filled the
    /// builder with a null time. The README invites people to write these files.
    /// </summary>
    [Theory]
    [InlineData("{}")] // no base at all
    [InlineData("[]")] // not an object
    [InlineData("{\"base\":{\"parameter\":null}}")] // JSON null where a parameter name belongs
    [InlineData("{\"base\":{\"parameter\":7}}")] // a number where a parameter name belongs
    [InlineData("{\"base\":\"today\",\"steps\":[{}]}")] // an empty step
    [InlineData("{\"base\":\"today\",\"steps\":[\"shift\"]}")] // a step that is not an object
    [InlineData("{\"base\":\"today\",\"steps\":[{\"set_time\":null}]}")] // silently accepted before
    [InlineData("{\"base\":\"today\",\"steps\":[{\"snap\":5}]}")] // a token that is not a string
    [InlineData("{\"base\":\"today\",\"steps\":[{\"shift\":{\"amount\":1,\"unit\":\"days\"}}]}")] // no sign
    [InlineData("{\"base\":\"today\",\"steps\":[{\"shift\":{\"sign\":\"+\",\"unit\":\"days\"}}]}")] // no amount
    [InlineData("{\"base\":\"today\",\"steps\":[{\"shift\":{\"sign\":\"+\",\"amount\":\"1\",\"unit\":\"d\"}}]}")] // amount as text
    [InlineData("{\"base\":\"today\",\"steps\":[{\"shift\":{\"parameter\":null,\"sign\":\"+\"}}]}")]
    public void A_malformed_moment_fails_as_unsupported_whatever_the_malformed_part_is(string json)
        => Assert.Throws<NotSupportedException>(() => PresetUnpack.UnpackMoment(Moment(json)));

    // Cloned so the element outlives the document, exactly as PresetCatalog keeps a preset's moment.
    private static JsonElement Moment(string json)
    {
        using var doc = JsonDocument.Parse(json);
        return doc.RootElement.Clone();
    }
}
