using ChronoMock.App.Calc;

namespace ChronoMock.App.Tests;

/// <summary>
/// The calculator's argument builder (slice G3b): the pure mapping from builder state to a
/// <c>chrono calc</c> argument list. Kept pure and static so it is tested without the UI thread.
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
            BaseKind.Specific, "2026-01-01T00:00:00", ["+18y", "-1d"], "us-banking");
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
}
