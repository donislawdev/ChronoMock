using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).

using ChronoMock.App;
using ChronoMock.Protocol;

namespace ChronoMock.App.Tests;

/// <summary>
/// Arguments and working folder (chrono-mock 7.1 pt 1), and what the panel says the target will see
/// (7.1 pt 3). The wire and the mechanism have carried <c>args</c> and <c>cwd</c> since the protocol was
/// written - these guard the two surfaces that never offered them, and the honest-blank rule that goes
/// with the folder.
/// </summary>
public class LaunchOptionsTests
{
    private static TimeSpec AnyTime() => new()
    {
        Moment = new MomentSpec { Kind = "absolute", Local = "2038-01-19T03:14:07", TzBiasMin = 0 },
        Mode = "flow",
    };

    /// <summary>A real PE to build a plan from: the app's own test host, which is certainly one.</summary>
    private static string APeFile() => System.Reflection.Assembly.GetExecutingAssembly().Location
        is { Length: > 0 } dll && File.Exists(Path.ChangeExtension(dll, ".exe"))
        ? Path.ChangeExtension(dll, ".exe")
        : Environment.ProcessPath!;

    [Fact]
    public void Arguments_and_folder_reach_the_start_command()
    {
        var plan = SessionPlan.Build(APeFile(), AnyTime(), force: false, args: ["--x", "a b"], workingFolder: @"C:\work");

        Assert.Equal(new[] { "--x", "a b" }, plan.Start.Target.Args);
        Assert.Equal(@"C:\work", plan.Start.Target.Cwd);
    }

    /// <summary>
    /// Blank is not the same as empty. The core hands <c>cwd</c> straight to CreateProcessW, where an
    /// empty string is not a valid directory - so "the tester typed nothing" has to arrive as absent,
    /// not as "". Whitespace counts as nothing typed, since that is what a stray space in the box is.
    /// </summary>
    [Theory]
    [InlineData(null)]
    [InlineData("")]
    [InlineData("   ")]
    public void A_blank_folder_is_absent_rather_than_an_empty_string(string? typed)
    {
        var plan = SessionPlan.Build(APeFile(), AnyTime(), force: false, workingFolder: typed);

        Assert.Null(plan.Start.Target.Cwd);
    }

    [Fact]
    public void No_arguments_is_an_empty_list_not_null()
    {
        var plan = SessionPlan.Build(APeFile(), AnyTime());

        Assert.NotNull(plan.Start.Target.Args);
        Assert.Empty(plan.Start.Target.Args);
    }

    /// <summary>
    /// The preview reads the ISO input back in words. The inputs are locale-invariant on purpose (a dev box
    /// and a test VM in different locales must read the same typed date the same way) - this line is what
    /// pays back the readability that costs, and the weekday is frequently the whole point of the test.
    /// </summary>
    [Fact]
    public void The_preview_names_the_weekday_and_the_zone()
    {
        var vm = new SessionViewModel();
        vm.Moment.LoadCanonical("2027-12-31T23:59:50");
        vm.SelectedZone = TimeInputs.Zones.First(z => z.BiasMinutes == 0);

        Assert.True(vm.HasMomentPreview);
        Assert.Contains("2027", vm.MomentPreview, StringComparison.Ordinal);
        Assert.Contains("23:59:50", vm.MomentPreview, StringComparison.Ordinal);
        // 31 December 2027 is a Friday - the fact the ISO text does not show.
        Assert.Contains("Friday", vm.MomentPreview, StringComparison.OrdinalIgnoreCase);
        Assert.Contains(vm.SelectedZone.Label, vm.MomentPreview, StringComparison.Ordinal);
    }

    /// <summary>
    /// The preview and the validation message share one slot, so they must never both have something to
    /// say - otherwise the row grows by a line and knocks the inputs out of alignment.
    /// </summary>
    [Fact]
    public void A_moment_that_does_not_parse_has_no_preview_only_an_error()
    {
        var vm = new SessionViewModel();
        vm.Moment.DateText = "31/12/2027"; // a locale format, deliberately rejected rather than guessed at

        Assert.True(vm.Moment.HasError);
        Assert.False(vm.HasMomentPreview);
        Assert.Equal(string.Empty, vm.MomentPreview);
    }

    [Fact]
    public void Changing_the_zone_rewrites_the_preview()
    {
        var vm = new SessionViewModel();
        vm.Moment.LoadCanonical("2027-12-31T23:59:50");

        vm.SelectedZone = TimeInputs.Zones.First(z => z.BiasMinutes == 0);
        var atUtc = vm.MomentPreview;
        vm.SelectedZone = TimeInputs.Zones.First(z => z.BiasMinutes != 0);

        Assert.NotEqual(atUtc, vm.MomentPreview);
    }

    /// <summary>
    /// Reading the property is not enough. The panel shows the preview through a binding, so without the
    /// change notification the line would keep the OLD zone on screen while every test that reads the
    /// property directly still passed - a green suite over a stale panel.
    /// </summary>
    [Fact]
    public void Changing_the_zone_announces_the_preview_changed()
    {
        var vm = new SessionViewModel();
        vm.Moment.LoadCanonical("2027-12-31T23:59:50");
        vm.SelectedZone = TimeInputs.Zones.First(z => z.BiasMinutes == 0);

        var announced = new List<string?>();
        vm.PropertyChanged += (_, e) => announced.Add(e.PropertyName);
        vm.SelectedZone = TimeInputs.Zones.First(z => z.BiasMinutes != 0);

        Assert.Contains(nameof(SessionViewModel.MomentPreview), announced);
    }

    /// <summary>The same for editing the moment itself, and for the same reason.</summary>
    [Fact]
    public void Editing_the_moment_announces_the_preview_changed()
    {
        var vm = new SessionViewModel();
        var announced = new List<string?>();
        vm.PropertyChanged += (_, e) => announced.Add(e.PropertyName);

        vm.Moment.LoadCanonical("2030-06-15T08:00:00");

        Assert.Contains(nameof(SessionViewModel.MomentPreview), announced);
    }

    /// <summary>The drop hint is a first-run affordance: once a target is set it would be noise.</summary>
    [Fact]
    public void The_drop_hint_shows_only_until_a_target_is_chosen()
    {
        var vm = new SessionViewModel();
        Assert.True(vm.ShowsDropHint);

        vm.SetTarget(APeFile());

        Assert.False(vm.ShowsDropHint);
        Assert.True(vm.HasTarget);
    }
}
