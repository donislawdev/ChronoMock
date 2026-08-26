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
    public void Switching_a_step_to_snap_toggles_the_visible_editor()
    {
        var step = NewStep();
        Assert.True(step.IsShift);
        Assert.False(step.IsSnap);

        step.SelectedKind = step.Kinds.First(k => k.Kind == StepKind.Snap);
        Assert.False(step.IsShift);
        Assert.True(step.IsSnap);
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
