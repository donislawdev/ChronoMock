using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using ChronoMock.App;

namespace ChronoMock.App.Tests;

/// <summary>
/// The two rebuilt phases in the states that decide whether they work, shared by the sheet that draws
/// them and the layout guards that hold them to account.
/// </summary>
/// <remarks>
/// 🔴 ONE SOURCE FOR BOTH. The sheet built these models inline, and the guards that now read the phases
/// need exactly the same states. Two copies drift the first time one of them gains an option, and a guard
/// reading a state the sheet no longer draws is a guard nobody can check against a picture.
///
/// The session states themselves stay in <see cref="SessionStates"/>, which the shipped panel's renders
/// share. Only the application is added here, for the reason <see cref="WithTarget"/> gives.
/// </remarks>
internal static class PhaseStates
{
    /// <summary>The setup phase on first contact: nothing chosen, the scenario catalogue loaded.</summary>
    public static SessionViewModel SetupStartup()
        => new(new InMemorySessionHistoryStore(), presetsDir: Path.Combine(TestPaths.RepoRoot(), "presets"));

    /// <summary>The scenario catalogue filtered down to nothing.</summary>
    public static SessionViewModel SetupSearchingForNothing()
    {
        var model = SetupStartup();
        model.ScenarioPicker.Filter = "nothing is called this";
        return model;
    }

    /// <summary>Every option in the merged speed section turned on, the launch fields filled in.</summary>
    public static SessionViewModel SetupWithEveryOption()
    {
        var model = SetupStartup();
        model.ScaleDuration = true;
        model.ScaleQpc = true;
        model.ForceStart = true;
        model.TargetArgs = "--seed 7 --headless";
        model.WorkingFolder = @"C:\apps\data";
        return model;
    }

    /// <summary>An application chosen and a date that does not exist.</summary>
    public static SessionViewModel SetupWithBadDate()
    {
        var model = WithTarget(SetupStartup());
        model.Moment.DateText = "2038-02-31";
        return model;
    }

    /// <summary>An application chosen and the options that show as chips in the folded header.</summary>
    public static SessionViewModel SetupConfigured()
    {
        var model = WithTarget(SetupStartup());
        model.ScaleDuration = true;
        model.ScaleQpc = true;
        model.ForceStart = true;
        return model;
    }

    /// <summary>
    /// Gives a model the application every phase render assumes.
    /// </summary>
    /// <remarks>
    /// 🔴 A SESSION HAS AN APPLICATION, and SessionStates does not set one - it was written for the panel,
    /// where the form supplies it. Without this the session phase's first line is bound to a model with no
    /// target and renders as nothing, so the render would be missing the one thing that says which session
    /// it is. Set here rather than in SessionStates, because the panel renders use those fixtures and their
    /// baselines would move.
    /// </remarks>
    public static SessionViewModel WithTarget(SessionViewModel model)
    {
        model.SetTarget(Path.Combine(TestPaths.RepoRoot(), "target.exe"));
        return model;
    }
}
