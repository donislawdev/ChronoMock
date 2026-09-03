namespace ChronoMock.Protocol.Tests;

/// <summary>
/// Locates the repo checkout and its built artifacts from the test's runtime directory, so the
/// conformance tests can launch the REAL core against the REAL test target. Fails loudly (never a silent
/// skip) when an artifact is missing - the honest signal is "build it first", not a green run (rule 6).
/// </summary>
internal static class RepoPaths
{
    /// <summary>Walk up from the test binaries until the directory holding the cargo workspace (Cargo.toml) is found.</summary>
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

    /// <summary>The x64 core executable, named by its explicit target triple like the x86 one - reading it
    /// from <c>target/release/</c> tests whatever was last built WITHOUT <c>--target</c>, which is how the
    /// harness spent a session on a day-old binary (R2-X3). Requires
    /// <c>cargo build --release --target x86_64-pc-windows-msvc</c>.</summary>
    public static string X64Core(string repoRoot)
        => RequireFile(
            Path.Combine(repoRoot, "target", "x86_64-pc-windows-msvc", "release", "chrono.exe"),
            "run `cargo build --release --target x86_64-pc-windows-msvc` first");

    /// <summary>The x86 core executable. Requires <c>cargo build --release --target i686-pc-windows-msvc</c>.</summary>
    public static string X86Core(string repoRoot)
        => RequireFile(
            Path.Combine(repoRoot, "target", "i686-pc-windows-msvc", "release", "chrono.exe"),
            "run `cargo build --release --target i686-pc-windows-msvc` first");

    /// <summary>The built .NET test target executable, matching the test's own build configuration.</summary>
    public static string TestTargetExe(string repoRoot)
    {
        var configuration = BuildConfiguration();
        var path = Path.Combine(
            repoRoot, "gui", "ChronoMock.TestTarget", "bin", configuration, "net10.0",
            "ChronoMock.TestTarget.exe");
        return RequireFile(path, "build the solution (dotnet build gui/ChronoMock.slnx) first");
    }

    private static string BuildConfiguration()
    {
        var marker = $"{Path.DirectorySeparatorChar}Release{Path.DirectorySeparatorChar}";
        return AppContext.BaseDirectory.Contains(marker, StringComparison.OrdinalIgnoreCase) ? "Release" : "Debug";
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
