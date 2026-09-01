using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using ChronoMock.Protocol;

namespace ChronoMock.App;

/// <summary>
/// Resolves the cores, the calculator client and the data directories for the layout the app runs in.
/// A shipped portable install (Stage 5) puts the cores under &lt;exe&gt;/core/&lt;arch&gt;/ with calendars/
/// and presets/ at the root beside the exe. A dev checkout has neither, so we fall back to the cargo build
/// outputs via <see cref="DevPaths"/>. This is the one seam between "runs from a zip" and "runs from a
/// checkout" - the reason <see cref="CoreLocator"/> takes a pluggable base-directory strategy.
/// </summary>
internal static class AppPaths
{
    /// <summary>
    /// The shipped layout's marker: the x64 core sits at &lt;exe&gt;/core/x64/chrono.exe. A dev checkout
    /// builds the GUI into gui/.../bin/... with no core/ beside it, so this is false there and we use the
    /// cargo outputs. The x64 core is always present in a shipped build (the host is x64), so it is a
    /// reliable marker even for an x86 target (whose core lives under core/x86/).
    /// </summary>
    private static bool IsPortable
        => File.Exists(Path.Combine(AppContext.BaseDirectory, "core", "x64", "chrono.exe"));

    /// <summary>Root holding calendars/ and presets/ (and, when portable, core/).</summary>
    public static string DataRoot => IsPortable ? AppContext.BaseDirectory : DevPaths.RepoRoot();

    /// <summary>Locator for the substitution core matching a target's bitness.</summary>
    public static CoreLocator SubstitutionCores => IsPortable
        ? CoreLocator.ForPortable(AppContext.BaseDirectory)
        : CoreLocator.ForRepo(DevPaths.RepoRoot());

    /// <summary>Calculator engine client, with a working directory where calendars/ and presets/ resolve.</summary>
    public static CalcClient CalcClient => IsPortable
        ? CalcClient.ForPortable(AppContext.BaseDirectory)
        : CalcClient.ForRepo(DevPaths.RepoRoot());

    /// <summary>The shared preset catalogue directory.</summary>
    public static string PresetsDir => Path.Combine(DataRoot, "presets");
}
