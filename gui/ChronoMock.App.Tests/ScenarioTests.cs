using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using ChronoMock.App;
using ChronoMock.App.Calc;

namespace ChronoMock.App.Tests;

/// <summary>
/// The substitution panel's scenario list (chrono-mock 7.1 pt 2): which presets it offers, which it says
/// it cannot offer, and how a chosen scenario relates to the date field. Evaluating one spawns the engine,
/// so that path is proven live rather than here; everything around it is pure and asserted.
/// </summary>
public class ScenarioTests
{
    private static string PresetsDir() => Path.Combine(TestPaths.RepoRoot(), "presets");

    [Fact]
    public void The_list_offers_substitution_presets_and_never_calculator_only_ones()
    {
        var catalogue = ScenarioCatalog.Load(PresetsDir());

        // year-rollover is substitution-only: the panel MUST still offer it, which is the whole reason a
        // scenario is evaluated through unpacked steps rather than through `calc --preset` (that path
        // refuses it). age-of-majority is calculator-only and must not appear.
        Assert.Contains(catalogue.Ready, s => s.Id == "year-rollover");
        Assert.Contains(catalogue.Ready, s => s.Id == "month-end"); // applies_to: both
        Assert.DoesNotContain(catalogue.Ready, s => s.Id == "age-of-majority");
        Assert.DoesNotContain(catalogue.Ready, s => s.Id == "payment-due-business-days");
    }

    [Fact]
    public void A_parametric_preset_is_counted_rather_than_silently_dropped()
    {
        var catalogue = ScenarioCatalog.Load(PresetsDir());

        // trial-first-day-after applies to substitution but takes parameters, so it cannot be one click.
        Assert.DoesNotContain(catalogue.Ready, s => s.Id == "trial-first-day-after");
        Assert.True(catalogue.NeedingParameters > 0, "the panel must be able to say these exist (rule 6)");
        Assert.All(catalogue.Ready, s => Assert.False(s.Info.IsParametric));
    }

    [Fact]
    public void Scenarios_are_ordered_invariantly_so_the_list_is_the_same_on_every_machine()
    {
        var names = ScenarioCatalog.Load(PresetsDir()).Ready.Select(s => s.DisplayName).ToList();
        Assert.Equal(names.OrderBy(n => n, StringComparer.InvariantCulture), names);
    }

    [Fact]
    public void A_view_model_with_no_preset_directory_offers_nothing_and_reads_no_files()
    {
        var vm = new SessionViewModel();

        Assert.Empty(vm.Scenarios);
        Assert.False(vm.HasScenarios);
        Assert.False(vm.HasScenariosNeedingParameters);
    }

    [Fact]
    public void Editing_the_date_by_hand_drops_the_scenario_selection()
    {
        // Otherwise the panel would keep naming a scenario whose moment is no longer in the field.
        var vm = new SessionViewModel(new InMemorySessionHistoryStore(), presetsDir: PresetsDir());
        var scenario = vm.Scenarios.First(s => s.Id == "year-rollover");

        vm.SelectedScenario = scenario;
        Assert.NotNull(vm.SelectedScenario);
        Assert.Equal(scenario.DisplayExplains, vm.ScenarioExplains);

        vm.Moment.LoadCanonical("2030-01-01T00:00:00");

        Assert.Null(vm.SelectedScenario);
        Assert.Equal(string.Empty, vm.ScenarioExplains);
    }

    [Fact]
    public void A_scenario_says_so_when_the_engine_is_not_available()
    {
        // No CalcClient injected: the panel reports it instead of leaving the old date and going quiet.
        var vm = new SessionViewModel(new InMemorySessionHistoryStore(), presetsDir: PresetsDir());

        vm.SelectedScenario = vm.Scenarios.First();

        Assert.True(vm.HasScenarioError);
        Assert.Equal("scenario.engine_missing", vm.ScenarioErrorKey);
    }

    [Fact]
    public void Choosing_a_scenario_never_starts_a_session_nor_changes_the_time_mode()
    {
        var vm = new SessionViewModel(new InMemorySessionHistoryStore(), presetsDir: PresetsDir());
        var mode = vm.SelectedMode;

        vm.SelectedScenario = vm.Scenarios.First();

        Assert.Equal(SessionStatusKind.Idle, vm.StatusKind); // rule 7: fills the form, never starts
        Assert.Same(mode, vm.SelectedMode); // "when" and "how fast" are separate axes
    }

    [Fact]
    public void A_scenario_is_computed_in_the_session_zone_and_not_through_the_preset_flag()
    {
        var scenario = ScenarioCatalog.Load(PresetsDir()).Ready.First(s => s.Id == "year-rollover");

        var args = SessionViewModel.BuildScenarioArgs(scenario, zoneBiasMinutes: 300); // UTC-05:00

        // The session zone travels with the request (rule 2) - without it the engine answers in the HOST's
        // zone, which is a different calendar day either side of midnight.
        Assert.Contains("--zone", args);
        Assert.Equal("-05:00", args[args.ToList().IndexOf("--zone") + 1]);

        // Evaluated as explicit steps: `calc --preset` gates on applies_to and refuses this very preset.
        Assert.DoesNotContain("--preset", args);
        Assert.Contains("--base", args);
        Assert.Contains("--snap", args);
    }

    [Theory]
    [InlineData(StepKind.Shift, "--shift", "+1d")]
    [InlineData(StepKind.Snap, "--snap", "eom")]
    [InlineData(StepKind.Nearest, "--nearest", "nbd")]
    [InlineData(StepKind.SetTime, "--set-time", "23:59:59")]
    [InlineData(StepKind.Zone, "--to-zone", "+00:00")]
    public void Each_step_kind_maps_to_its_calc_flag(StepKind kind, string flag, string value)
    {
        // The single mapping the builder and the scenario list share - two spellings of one grammar would
        // drift, and the drift would only ever show up as a wrong date.
        var args = UnpackedMoment.StepArgs(new UnpackedStep(kind));

        Assert.Equal([flag, value], args);
    }
}
