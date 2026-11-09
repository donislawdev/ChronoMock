using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).

namespace ChronoMock.App;

/// <summary>
/// Detects an Electron/Chromium target the same way the CLI does (cdp::is_chromium_target): its folder
/// ships the Chromium runtime. Such a target is not driven by the native hook - its time-dependent logic
/// runs in a sandboxed renderer the hook cannot reach, and Chromium timers are QPC-based (ADR-2). The core
/// auto-detects it and drives it over the DevTools protocol instead (ADR-8/ADR-9), speaking the same
/// machine protocol - the GUI routes it there (skips the bitness gate, labels coverage as JS contexts).
/// </summary>
internal static class ChromiumTarget
{
    /// <summary>Whether the target exe's folder holds the Chromium runtime (icudtl.dat + a V8 snapshot) -
    /// a version-stable signature, mirroring the CLI so the two agree on what "Chromium" means. An installed
    /// Chromium browser keeps the runtime in a folder named after its version beside the exe, together with
    /// the library the exe loads (Chromium's own chrome.dll, or one named after the exe, as browser.dll
    /// beside browser.exe), and is one too.</summary>
    public static bool IsChromium(string targetPath)
    {
        var dir = Path.GetDirectoryName(targetPath);
        if (string.IsNullOrEmpty(dir))
        {
            return false;
        }

        if (HoldsRuntime(dir))
        {
            return true;
        }

        var ownLibrary = Path.GetFileNameWithoutExtension(targetPath) + ".dll";
        foreach (var folder in VersionFolders(dir))
        {
            if (HoldsRuntime(folder)
                && (File.Exists(Path.Combine(folder, "chrome.dll")) || File.Exists(Path.Combine(folder, ownLibrary))))
            {
                return true;
            }
        }

        return false;
    }

    private static bool HoldsRuntime(string folder) =>
        File.Exists(Path.Combine(folder, "icudtl.dat"))
        && (File.Exists(Path.Combine(folder, "v8_context_snapshot.bin"))
            || File.Exists(Path.Combine(folder, "snapshot_blob.bin")));

    /// <summary>The folders in <paramref name="dir"/> named after a version - digits and dots only, as
    /// 150.0.1234.56. A folder that cannot be read has none.</summary>
    private static List<string> VersionFolders(string dir)
    {
        try
        {
            return Directory.EnumerateDirectories(dir)
                .Where(folder => IsVersionName(Path.GetFileName(folder)))
                .ToList();
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            return [];
        }
    }

    /// <summary>Digits in at least two parts split by dots, every part non-empty: 150.0.1234.56, not
    /// app-1.2.3.</summary>
    internal static bool IsVersionName(string name) =>
        name.Contains('.', StringComparison.Ordinal)
        && name.Split('.').All(part => part.Length > 0 && part.All(char.IsAsciiDigit));
}
