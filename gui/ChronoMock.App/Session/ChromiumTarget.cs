using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).

namespace ChronoMock.App;

/// <summary>
/// Detects an Electron/Chromium target the same way the CLI does (cdp::is_chromium_target): its folder
/// ships the Chromium runtime. Such a target is not driven by the native core - its time-dependent logic
/// runs in a sandboxed renderer the hook cannot reach, and Chromium timers are QPC-based (ADR-2). The
/// CLI runs it over the DevTools protocol instead (ADR-8); the GUI panel does not drive that yet, so it
/// must refuse honestly rather than start a native session that would look like it worked without
/// accelerating (untouchable rule 4).
/// </summary>
internal static class ChromiumTarget
{
    /// <summary>Whether the target exe's folder holds the Chromium runtime (icudtl.dat + a V8 snapshot) -
    /// a version-stable signature, mirroring the CLI so the two agree on what "Chromium" means.</summary>
    public static bool IsChromium(string targetPath)
    {
        var dir = Path.GetDirectoryName(targetPath);
        if (string.IsNullOrEmpty(dir))
        {
            return false;
        }

        var hasIcu = File.Exists(Path.Combine(dir, "icudtl.dat"));
        var hasSnapshot = File.Exists(Path.Combine(dir, "v8_context_snapshot.bin"))
                          || File.Exists(Path.Combine(dir, "snapshot_blob.bin"));
        return hasIcu && hasSnapshot;
    }

    /// <summary>The CLI command that runs a Chromium target in Chromium mode, for the honest hand-off
    /// message (the user copies it). Quoted so a path with spaces works.</summary>
    public static string CliCommand(string targetPath) => $"chrono run \"{targetPath}\" --mode x60";
}
