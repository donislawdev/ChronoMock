using ChronoMock.App.Calc;

namespace ChronoMock.App.Tests;

/// <summary>
/// A start moment relative to now - the panel's <c>chrono run --at +30d</c>. The arithmetic is the engine's
/// (months and years fold onto the civil date), so what is asserted here is the QUESTION this panel asks:
/// the shift token, the argument list, the units it offers, and that the controls are wired to all three.
/// </summary>
public class RelativeMomentTests
{
    [Theory]
    [InlineData("+", "30", "d", "+30d")]
    [InlineData("-", "2", "h", "-2h")]
    [InlineData("+", "1", "mo", "+1mo")]
    [InlineData("+", " 7 ", "y", "+7y")] // trimmed, because a pasted value carries spaces
    public void A_sign_an_amount_and_a_unit_make_the_engines_shift_token(
        string sign, string amount, string unit, string expected)
        => Assert.Equal(expected, RelativeMoment.ShiftToken(sign, amount, unit));

    /// <summary>Rejected here rather than sent. The sign is its OWN control, so a minus typed into the
    /// amount would reach the engine as "+-2d" and come back named after the token, not after the field the
    /// tester can fix. Zero is refused too: "now plus nothing" is the Now button, not a delta.</summary>
    [Theory]
    [InlineData("")]
    [InlineData("   ")]
    [InlineData("0")]
    [InlineData("-2")]
    [InlineData("+2")]
    [InlineData("2.5")]
    [InlineData("thirty")]
    [InlineData("99999999999999999999")]
    public void An_amount_that_is_not_a_whole_number_above_zero_produces_no_token(string amount)
        => Assert.Null(RelativeMoment.ShiftToken("+", amount, "d"));

    /// <summary>The same builder the calculator and the scenario path use, so all three surfaces put one
    /// grammar on the command line - base now, one shift, and the session zone travelling with it.</summary>
    [Fact]
    public void The_arguments_are_now_plus_one_shift_in_the_session_zone()
        => Assert.Equal(
            new[] { "--base", "now", "--shift", "+30d", "--zone", "-08:00" },
            RelativeMoment.BuildArgs("+30d", 480));

    /// <summary>Business days are absent on purpose: a session carries no calendar, so the engine could only
    /// answer "business days need a calendar". Offering a unit that can only fail is the kind of silence
    /// untouchable rule 4 exists to stop. Asserted against the calculator's list rather than a retyped one,
    /// so a unit added there reaches this picker too.</summary>
    [Fact]
    public void The_units_are_the_calculators_minus_business_days()
    {
        Assert.DoesNotContain(RelativeMoment.Units, u => u.Token == "bd");
        Assert.Contains(StepViewModel.AllUnits, u => u.Token == "bd");
        Assert.Equal(StepViewModel.AllUnits.Count - 1, RelativeMoment.Units.Count);
        Assert.Equal(
            StepViewModel.AllUnits.Where(u => u.Token != "bd").Select(u => u.Token),
            RelativeMoment.Units.Select(u => u.Token));
    }

    /// <summary>The wiring, not the builder: what the CONTROLS would send. The builder tests above would all
    /// stay green over a row bound to nothing, which is the failure this catches.</summary>
    [Fact]
    public void The_row_sends_what_its_controls_hold()
    {
        var vm = NewViewModel();

        // Fresh: "+ 1 day", the same default a new calculator shift step opens on.
        Assert.Equal("+", vm.Sign);
        Assert.Equal("1", vm.Amount);
        Assert.Equal("d", vm.Unit.Token);
        Assert.Equal(new[] { "--base", "now", "--shift", "+1d", "--zone", "+02:00" }, vm.CurrentArgs(-120));

        vm.Sign = "-";
        vm.Amount = "6";
        vm.Unit = vm.Units.First(u => u.Token == "mo");
        Assert.Equal(new[] { "--base", "now", "--shift", "-6mo", "--zone", "+00:00" }, vm.CurrentArgs(0));

        // An amount the engine could not use produces no question at all, rather than a malformed one.
        vm.Amount = "0";
        Assert.Null(vm.CurrentArgs(0));
    }

    /// <summary>A bad amount is answered by the row itself - no process is started, and the moment field is
    /// left exactly as it was. Asserting the FIELD matters: "it did not throw" would pass over a row that
    /// silently wiped the date.</summary>
    [Fact]
    public async Task A_bad_amount_names_the_field_and_leaves_the_moment_alone()
    {
        var field = new MomentField();
        field.LoadCanonical("2038-01-19T03:14:07");
        var vm = new RelativeMomentViewModel(field, null) { Amount = "not a number" };

        await vm.ApplyAsync(-120);

        Assert.True(vm.HasError);
        Assert.Equal("moment.relative_bad_amount", vm.ErrorKey);
        Assert.Equal("2038-01-19T03:14:07", field.Canonical);
    }

    /// <summary>With no engine the row says so instead of going quiet, and still leaves the field alone. The
    /// amount is valid here, so this reaches the engine check rather than stopping at the amount.</summary>
    [Fact]
    public async Task Without_an_engine_the_row_says_so_rather_than_going_quiet()
    {
        var field = new MomentField();
        field.LoadCanonical("2038-01-19T03:14:07");
        var vm = new RelativeMomentViewModel(field, null);

        await vm.ApplyAsync(-120);

        Assert.Equal("scenario.engine_missing", vm.ErrorKey);
        Assert.Equal("2038-01-19T03:14:07", field.Canonical);
    }

    /// <summary>The row in the window is bound to this view model. Without this the XAML could be pointed at
    /// the session view model, every binding would fail silently, and the controls would sit there empty -
    /// the one failure mode a unit test over the view model cannot see.</summary>
    [Fact]
    public void The_window_binds_the_relative_row_to_this_view_model()
    {
        var context = WpfTestHost.InvokeSettled(() => new MainWindow().RelativeMomentRow.DataContext);
        Assert.IsType<RelativeMomentViewModel>(context);
    }

    private static RelativeMomentViewModel NewViewModel() => new(new MomentField(), null);
}
