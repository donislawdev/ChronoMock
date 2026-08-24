using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).

namespace ChronoMock.App;

/// <summary>
/// DEV SCAFFOLDING (slice 3.2-II). Locates the cargo-built cores and the bundled test target from a dev
/// checkout, the same way the tests do, so the live panel has something to drive before the real target
/// picker exists (a later slice). Fails loudly when a build artifact is missing - the honest signal is
/// "build it first", never a silent skip (untouchable rule 6). This whole type is replaced by the picker.
/// </summary>
internal static class DevPaths
{
    /// <summary>Walk up from the running app until the directory holding the cargo workspace (Cargo.toml) is found.</summary>
    public static string RepoRoot()
    {
        var dir = new DirectoryInfo(AppContext.BaseDirectory);
        while (dir is not null)
        {
            if (File.Exists(Path.Combine(dir.FullName, "Cargo.toml")))
            {
                return dir.FullName;
            }

            dir = dir.Parent;
        }

        throw new InvalidOperationException(
            $"could not find the repo root (a parent with Cargo.toml) from '{AppContext.BaseDirectory}'");
    }

    /// <summary>The bundled .NET test target, matching the app's own build configuration.</summary>
    public static string TestTargetExe(string repoRoot)
    {
        var marker = $"{Path.DirectorySeparatorChar}Release{Path.DirectorySeparatorChar}";
        var configuration =
            AppContext.BaseDirectory.Contains(marker, StringComparison.OrdinalIgnoreCase) ? "Release" : "Debug";
        var path = Path.Combine(
            repoRoot, "gui", "ChronoMock.TestTarget", "bin", configuration, "net10.0", "ChronoMock.TestTarget.exe");
        return RequireFile(path, "build the solution (dotnet build gui/ChronoMock.slnx) first");
    }

    private static string RequireFile(string path, string howToFix)
    {
        if (!File.Exists(path))
        {
            throw new FileNotFoundException($"missing build artifact '{path}' - {howToFix}", path);
        }

        return path;
    }
}
