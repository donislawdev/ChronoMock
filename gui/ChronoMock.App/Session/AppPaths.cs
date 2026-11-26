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
    /// <summary>The assembly metadata key the packaging script sets, and the one value that means portable.</summary>
    internal const string LayoutKey = "ChronoMock.Layout";

    internal const string PortableLayout = "portable";

    /// <summary>
    /// Whether this build is the shipped portable layout, as the BUILD says - <c>packaging/build-dist.ps1</c>
    /// publishes with <c>ChronoMockLayout=portable</c>, which the project turns into assembly metadata. A dev
    /// build has none and uses the cargo outputs. Also used to suppress dev-only scaffolding (the pre-selected
    /// sample target) in a shipped build. Read once, so the layout cannot change under a running window.
    /// <para>
    /// 🔴 It used to be the presence of the x64 core beside the exe (R4-S26). An antivirus quarantining that
    /// file, or an archive extracted in part, turned the shipped build into a dev one: it looked for the repo
    /// root and stopped at start with "could not find the repo root (a parent with Cargo.toml)" - measured
    /// on the packaged build. A core that vanished while the window was open made the next Start blame the
    /// TARGET. Now a missing core is what it is: Start says the installation may be incomplete and names the
    /// path, and the calculator says the same in the interface's language.
    /// </para>
    /// </summary>
    internal static bool IsPortable { get; } = IsPortableBuild(typeof(AppPaths).Assembly);

    /// <summary>The layout an assembly was built for - separate so a test can ask it of an assembly it made.</summary>
    internal static bool IsPortableBuild(System.Reflection.Assembly assembly)
        => assembly.GetCustomAttributes(typeof(System.Reflection.AssemblyMetadataAttribute), inherit: false)
            .OfType<System.Reflection.AssemblyMetadataAttribute>()
            .Any(a => a.Key == LayoutKey && a.Value == PortableLayout);

    /// <summary>The file the Windows installer lays beside the executable, and nothing else does
    /// (<c>packaging/build-msi.ps1</c> writes it, <c>crates/cli/tests/msi.rs</c> holds the two names together).</summary>
    internal const string InstalledMarker = "installed.txt";

    /// <summary>
    /// Whether this copy was put here by the Windows installer, which owns the folder. Every copy keeps its own
    /// files in the per-user folder now (<see cref="WritableFolder.ForApp"/>), so what this still decides is
    /// whether there can be an older history beside the executable to read in once - an installed copy never
    /// kept one there (<see cref="FileSessionHistoryStore.ForApp"/>). The installer is built from the signed
    /// portable package, so the build cannot say so: a second build would be a second set of files to sign. The
    /// installer adds one file instead. Read once, like the layout.
    /// </summary>
    internal static bool IsInstalled { get; } = IsInstalledLayout(IsPortable, AppContext.BaseDirectory);

    /// <summary>The installed layout for a given folder - separate so a test can ask it of a folder it made.</summary>
    internal static bool IsInstalledLayout(bool portable, string exeDir)
        => portable && File.Exists(Path.Combine(exeDir, InstalledMarker));

    /// <summary>Root holding calendars/ and presets/: beside the window when portable, the date library's
    /// catalogue folder in a dev checkout.</summary>
    public static string DataRoot => IsPortable ? AppContext.BaseDirectory : EnginePaths.RepoCatalogue(DevPaths.RepoRoot());

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

    /// <summary>The preset catalogue as the engine reads it (<c>chrono presets</c>, R4/18): the same core the
    /// calculator asks, pointed at <see cref="PresetsDir"/> explicitly so the answer does not depend on where
    /// the engine runs from.</summary>
    public static PresetCatalogueClient PresetCatalogue => new(
        () => IsPortable ? EnginePaths.Portable(AppContext.BaseDirectory) : EnginePaths.Repo(DevPaths.RepoRoot()),
        PresetsDir);

    /// <summary>The calendar catalogue as the engine reads it (<c>chrono calendars</c>, R5-P12): the same core the
    /// calculator asks, pointed at the <c>calendars</c> folder beside <see cref="PresetsDir"/> - the one
    /// <c>calc --calendar</c> reads from in both layouts.</summary>
    public static CalendarCatalogueClient CalendarCatalogue => new(
        () => IsPortable ? EnginePaths.Portable(AppContext.BaseDirectory) : EnginePaths.Repo(DevPaths.RepoRoot()),
        Path.Combine(DataRoot, "calendars"));

    /// <summary>Client for the component register the About window shows, from the same core as the rest.</summary>
    public static LicenseClient LicenceClient => IsPortable
        ? LicenseClient.ForPortable(AppContext.BaseDirectory)
        : LicenseClient.ForRepo(DevPaths.RepoRoot());
}
