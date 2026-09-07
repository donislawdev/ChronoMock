using ChronoMock.App.Calc;
using ChronoMock.Protocol;

namespace ChronoMock.App.Tests;

/// <summary>
/// The calculator's argument builder (slices G3b/G3c): the pure mapping from builder state to a
/// <c>chrono calc</c> argument list, plus each step's own flag pair. Kept pure and static so it is tested
/// without the UI thread.
/// </summary>
public class CalculatorArgsTests
{
    [Fact]
    public void Today_base_with_no_steps()
    {
        var args = CalculatorViewModel.BuildCalcArgs(BaseKind.Today, string.Empty, [], null);
        Assert.Equal(new[] { "--base", "today" }, args);
    }

    [Fact]
    public void Now_base_omits_the_calendar_when_none()
    {
        var args = CalculatorViewModel.BuildCalcArgs(BaseKind.Now, string.Empty, [], null);
        Assert.Equal(new[] { "--base", "now" }, args);
    }

    [Fact]
    public void Specific_base_with_shifts_and_a_calendar()
    {
        var args = CalculatorViewModel.BuildCalcArgs(
            BaseKind.Specific,
            "2026-01-01T00:00:00",
            [["--shift", "+18y"], ["--shift", "-1d"]],
            "us-banking");
        Assert.Equal(
            new[]
            {
                "--base", "2026-01-01T00:00:00",
                "--shift", "+18y",
                "--shift", "-1d",
                "--calendar", "us-banking",
            },
            args);
    }

    [Fact]
    public void Steps_of_mixed_kinds_keep_their_flags_and_order()
    {
        var args = CalculatorViewModel.BuildCalcArgs(
            BaseKind.Today,
            string.Empty,
            [["--shift", "+1mo"], ["--snap", "eoq"]],
            null);
        Assert.Equal(
            new[] { "--base", "today", "--shift", "+1mo", "--snap", "eoq" },
            args);
    }

    [Fact]
    public void A_custom_format_mask_appends_the_format_flag_after_the_calendar()
    {
        var args = CalculatorViewModel.BuildCalcArgs(
            BaseKind.Today, string.Empty, [], "pl", "dd.MM.yyyy");
        Assert.Equal(
            new[] { "--base", "today", "--calendar", "pl", "--format", "dd.MM.yyyy" },
            args);
    }

    [Fact]
    public void A_blank_custom_format_mask_is_omitted()
    {
        var args = CalculatorViewModel.BuildCalcArgs(BaseKind.Today, string.Empty, [], null, "   ");
        Assert.Equal(new[] { "--base", "today" }, args);
    }

    /// <summary>The base zone is the SESSION zone the start point is read in, so it reaches the engine as
    /// <c>--zone</c> - not as the <c>--to-zone</c> step, which re-expresses the answer instead.</summary>
    [Fact]
    public void A_picked_base_zone_reaches_the_engine_as_the_session_zone_flag()
    {
        var args = CalculatorViewModel.BuildCalcArgs(
            BaseKind.Today, string.Empty, [], null, null, "-08:00");
        Assert.Equal(new[] { "--base", "today", "--zone", "-08:00" }, args);
        Assert.DoesNotContain("--to-zone", args);
    }

    /// <summary>The host entry sends NOTHING. This is the whole reason it exists: the calculator never sent
    /// <c>--zone</c> before the picker was added, so any default that pinned an offset would silently change
    /// every result for a machine not sitting on it. A blank is treated as host too, so a whitespace value
    /// can never reach the command line as an empty flag (which is a usage error).</summary>
    [Fact]
    public void The_host_base_zone_sends_no_flag_at_all()
    {
        Assert.Equal(
            new[] { "--base", "today" },
            CalculatorViewModel.BuildCalcArgs(BaseKind.Today, string.Empty, [], null, null, null));
        Assert.Equal(
            new[] { "--base", "today" },
            CalculatorViewModel.BuildCalcArgs(BaseKind.Today, string.Empty, [], null, null, "  "));
    }

    /// <summary>The catalogue's first entry is the host, and it is the one that sends no flag. Ordering is
    /// asserted because the view model selects index 0 as its default - a reorder would change what an
    /// untouched calculator computes.</summary>
    [Fact]
    public void The_calculator_zone_catalogue_leads_with_the_host_and_only_the_host_is_host()
    {
        var zones = TimeInputs.CalcZones();
        Assert.True(zones[0].IsHost, "the default selection is index 0, and it must be the host entry");
        Assert.Equal("zone.host", zones[0].HintKey);
        Assert.Single(zones, z => z.IsHost);
        // The rest is the substitution panel's closed list, unchanged and still explicit.
        Assert.Equal(TimeInputs.Zones.Count + 1, zones.Count);
        Assert.All(TimeInputs.Zones, z => Assert.False(z.IsHost));
    }

    [Fact]
    public void A_new_step_defaults_to_a_days_shift()
    {
        var step = NewStep();
        Assert.Equal(new[] { "--shift", "+1d" }, step.ToArgs());
    }

    [Fact]
    public void A_snap_step_emits_its_target_token()
    {
        var step = NewStep();
        step.SelectedKind = step.Kinds.First(k => k.Kind == StepKind.Snap);
        step.SnapTarget = step.SnapTargets.First(t => t.Token == "eoq");
        Assert.Equal(new[] { "--snap", "eoq" }, step.ToArgs());
    }

    [Fact]
    public void A_nearest_step_emits_its_target_token()
    {
        var step = NewStep();
        step.SelectedKind = step.Kinds.First(k => k.Kind == StepKind.Nearest);
        step.NearestTarget = step.NearestTargets.First(t => t.Token == "pbd");
        Assert.Equal(new[] { "--nearest", "pbd" }, step.ToArgs());
    }

    [Fact]
    public void A_set_time_step_emits_the_time_verbatim()
    {
        var step = NewStep();
        step.SelectedKind = step.Kinds.First(k => k.Kind == StepKind.SetTime);
        step.SetTimeText = "00:00:01";
        Assert.Equal(new[] { "--set-time", "00:00:01" }, step.ToArgs());
    }

    [Fact]
    public void A_zone_step_emits_a_to_zone_offset()
    {
        var step = NewStep();
        step.SelectedKind = step.Kinds.First(k => k.Kind == StepKind.Zone);
        step.ZoneText = "+05:45";
        Assert.Equal(new[] { "--to-zone", "+05:45" }, step.ToArgs());
    }

    [Fact]
    public void Switching_a_step_to_snap_toggles_the_visible_editor()
    {
        var step = NewStep();
        Assert.True(step.IsShift);
        Assert.False(step.IsSnap);

        step.SelectedKind = step.Kinds.First(k => k.Kind == StepKind.Snap);
        Assert.False(step.IsShift);
        Assert.True(step.IsSnap);
    }

    [Fact]
    public void A_preset_with_a_malformed_moment_shows_needs_parameters_and_does_not_crash()
    {
        // A preset file missing "moment" (PresetCatalog stores default(JsonElement)), or with an empty
        // step / a shift without an amount, makes PresetUnpack throw InvalidOperationException /
        // KeyNotFoundException / FormatException - not NotSupportedException. Those used to escape
        // ApplyPreset's NotSupportedException-only catch and crash the dispatcher (M-8). Now they degrade
        // to the honest "needs parameters" note. Non-parametric + malformed never reaches a recompute, so
        // the calc client is not invoked.
        var vm = new CalculatorViewModel(new CalcClient(() => "chrono"));
        var preset = new PresetInfo(
            "broken",
            new Dictionary<string, string> { ["en"] = "Broken" },
            new Dictionary<string, string>(),
            "calculator",
            null,
            [],
            default); // no moment -> default(JsonElement), GetProperty("base") throws InvalidOperationException

        var ex = Record.Exception(() => vm.ApplyPreset(preset));

        Assert.Null(ex); // no crash
        Assert.True(vm.HasActivePreset);
        Assert.True(vm.ActiveNeedsParameters); // honest note instead of a wrong or absent date
    }

    // The Specific base now reads a locale-safe MomentField (the shared MomentInput), seeded to the default
    // the calculator first showed. Constructing the view model spawns nothing (compute is on first reveal),
    // and editing the field before reveal only stages state.
    [Fact]
    public void Base_seeds_to_the_default_specific_moment()
    {
        var vm = new CalculatorViewModel(new CalcClient(() => "chrono"));
        Assert.Equal("2026-01-01T00:00:00", vm.Base.Canonical);
    }

    [Fact]
    public void Specific_base_uses_the_moment_field_canonical()
    {
        var vm = new CalculatorViewModel(new CalcClient(() => "chrono"));
        vm.Base.DateText = "2030-05-05";
        vm.Base.TimeText = "12:30";
        Assert.True(vm.Base.IsValid);
        Assert.Equal(
            new[] { "--base", "2030-05-05T12:30:00" },
            CalculatorViewModel.BuildCalcArgs(BaseKind.Specific, vm.Base.Canonical, [], null));
    }

    /// <summary>The wiring, not just the builder: what the PANEL would send. An untouched calculator sends
    /// no zone at all, and picking one puts it on the command line with the offset the label shows. Without
    /// this the argument tests above would stay green over a picker that was bound to nothing, or over a
    /// sign flip - bias 480 is UTC-08:00, and getting that backwards moves the answer sixteen hours and a
    /// calendar day (untouchable rule 2).</summary>
    [Fact]
    public void The_panel_sends_no_zone_until_one_is_picked_and_then_sends_that_one()
    {
        var vm = new CalculatorViewModel(new CalcClient(() => "chrono"));

        Assert.True(vm.SelectedBaseZone.IsHost, "an untouched calculator starts on the host entry");
        Assert.DoesNotContain("--zone", vm.BuildCurrentArgs());

        vm.SelectedBaseZone = vm.BaseZones.First(z => z.HintKey == "zone.us_pacific");
        var args = vm.BuildCurrentArgs();
        Assert.Equal("-08:00", args[args.ToList().IndexOf("--zone") + 1]);
        Assert.Single(args, a => a == "--zone");

        // Back to host: the flag disappears again rather than sticking at the last pick.
        vm.SelectedBaseZone = vm.BaseZones.First(z => z.IsHost);
        Assert.DoesNotContain("--zone", vm.BuildCurrentArgs());
    }

    // A step built the way the view model builds it (real option lists), without a UI thread. The calc
    // client is never invoked here - EnsureComputedAsync is not called, so adding a step spawns nothing.
    private static StepViewModel NewStep()
    {
        var vm = new CalculatorViewModel(new CalcClient(() => "chrono"));
        vm.AddStep();
        return vm.Steps[0];
    }
}
