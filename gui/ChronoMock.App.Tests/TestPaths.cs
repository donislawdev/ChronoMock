using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).

namespace ChronoMock.App.Tests;

/// <summary>Locates the repo checkout and the app's source directory from the test's runtime directory,
/// so the literal guard can scan the real view XAML (source, not compiled BAML).</summary>
internal static class TestPaths
{
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

    public static string AppDirectory() => Path.Combine(RepoRoot(), "gui", "ChronoMock.App");
}
