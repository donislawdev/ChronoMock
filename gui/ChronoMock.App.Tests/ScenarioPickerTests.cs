using System.Globalization;
using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using ChronoMock.App;

namespace ChronoMock.App.Tests;

/// <summary>
/// The scenario list and the text that narrows it. Everything here is pure - no window, no process, no
/// engine - which is the point of the list living apart from the choice.
/// </summary>
public class ScenarioPickerTests
{
    private static string PresetsDir() => Path.Combine(TestPaths.RepoRoot(), "presets");

    private static ScenarioPicker Loaded()
    {
        var picker = new ScenarioPicker();
        picker.Load(PresetsDir());
        return picker;
    }

    [Fact]
    public void An_unloaded_picker_offers_nothing_and_says_so_without_reading_a_file()
    {
        var picker = new ScenarioPicker();

        Assert.Empty(picker.Visible);
        Assert.False(picker.HasScenarios);
        Assert.False(picker.HasNoMatches);
        Assert.False(picker.HasNeedingParameters);
    }

    [Fact]
    public void An_empty_filter_shows_the_whole_catalogue()
    {
        var picker = Loaded();
        var all = picker.Visible.Count;

        Assert.True(all > 0, "the shipped presets folder has substitution scenarios in it");

        picker.Filter = "   ";

        Assert.Equal(all, picker.Visible.Count);
        Assert.False(picker.HasNoMatches);
    }

    [Fact]
    public void A_filter_keeps_only_the_names_that_contain_it()
    {
        var picker = Loaded();
        var wanted = picker.Visible[0].DisplayName;

        picker.Filter = wanted;

        Assert.Contains(picker.Visible, s => s.DisplayName == wanted);
        Assert.All(
            picker.Visible,
            s => Assert.Contains(wanted, s.DisplayName, StringComparison.CurrentCultureIgnoreCase));
    }

    [Fact]
    public void Case_does_not_matter_to_the_person_typing()
    {
        var picker = Loaded();
        var wanted = picker.Visible[0].DisplayName;

        picker.Filter = wanted.ToUpper(CultureInfo.CurrentCulture);
        var upper = picker.Visible.Count;

        picker.Filter = wanted.ToLower(CultureInfo.CurrentCulture);

        Assert.Equal(upper, picker.Visible.Count);
        Assert.NotEmpty(picker.Visible);
    }

    /// <summary>
    /// The two blank states mean opposite things and must not look alike to a caller.
    /// </summary>
    [Fact]
    public void Nothing_matched_is_a_different_state_from_nothing_installed()
    {
        var installed = Loaded();
        installed.Filter = "no scenario is called this";

        Assert.Empty(installed.Visible);
        Assert.True(installed.HasScenarios);
        Assert.True(installed.HasNoMatches);

        var bare = new ScenarioPicker();

        Assert.Empty(bare.Visible);
        Assert.False(bare.HasScenarios);
        Assert.False(bare.HasNoMatches);
    }

    [Fact]
    public void Clearing_the_filter_brings_the_whole_catalogue_back()
    {
        var picker = Loaded();
        var all = picker.Visible.Count;

        picker.Filter = "no scenario is called this";
        picker.Filter = string.Empty;

        Assert.Equal(all, picker.Visible.Count);
    }

    /// <summary>
    /// A list that changed without saying so leaves the screen showing the previous one.
    /// </summary>
    [Fact]
    public void Narrowing_the_list_announces_the_list_and_the_empty_state()
    {
        var picker = Loaded();
        var announced = new List<string>();
        picker.PropertyChanged += (_, e) => announced.Add(e.PropertyName ?? string.Empty);

        picker.Filter = "no scenario is called this";

        Assert.Contains(nameof(ScenarioPicker.Filter), announced);
        Assert.Contains(nameof(ScenarioPicker.Visible), announced);
        Assert.Contains(nameof(ScenarioPicker.HasNoMatches), announced);
    }

    [Fact]
    public void Loading_announces_everything_a_screen_binds_before_the_catalogue_is_there()
    {
        var picker = new ScenarioPicker();
        var announced = new List<string>();
        picker.PropertyChanged += (_, e) => announced.Add(e.PropertyName ?? string.Empty);

        picker.Load(PresetsDir());

        Assert.Contains(nameof(ScenarioPicker.Visible), announced);
        Assert.Contains(nameof(ScenarioPicker.HasScenarios), announced);
        Assert.Contains(nameof(ScenarioPicker.NeedingParameters), announced);
        Assert.Contains(nameof(ScenarioPicker.HasNeedingParameters), announced);
    }

    /// <summary>
    /// A row that matched on text the reader cannot see is a result they cannot account for.
    /// </summary>
    [Fact]
    public void The_filter_reads_the_name_and_never_the_explanation()
    {
        var picker = Loaded();
        var withExplanation = picker.Visible.First(s => s.DisplayExplains.Length > 0);
        var wordFromTheExplanationAlone = withExplanation.DisplayExplains
            .Split(' ')
            .FirstOrDefault(w =>
                w.Length > 5
                && !withExplanation.DisplayName.Contains(w, StringComparison.CurrentCultureIgnoreCase));

        Assert.SkipWhen(
            wordFromTheExplanationAlone is null,
            "no shipped scenario has a long word in its explanation that is absent from its name");

        picker.Filter = wordFromTheExplanationAlone!;

        Assert.DoesNotContain(picker.Visible, s => s.Id == withExplanation.Id);
    }
}
