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
    /// beside browser.exe), and is one too. So is an Electron application that keeps the runtime in a folder
    /// beside the exe together with its packed application (resources\app or resources\app.asar) and no
    /// program of its own in that folder - a starter keeping the application in a versioned folder has its
    /// program there, and is not.</summary>
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
        var withRuntime = Subfolders(dir).Where(HoldsRuntime).ToList();
        var installed = withRuntime.Any(folder =>
            IsVersionName(Path.GetFileName(folder))
            && (File.Exists(Path.Combine(folder, "chrome.dll")) || File.Exists(Path.Combine(folder, ownLibrary))));
        return installed || withRuntime.Any(folder => HoldsPackedApp(folder) && HoldsNoProgram(folder));
    }

    private static bool HoldsRuntime(string folder) =>
        File.Exists(Path.Combine(folder, "icudtl.dat"))
        && (File.Exists(Path.Combine(folder, "v8_context_snapshot.bin"))
            || File.Exists(Path.Combine(folder, "snapshot_blob.bin")));

    /// <summary>Whether a folder holds an Electron application in its resources, packed (app.asar) or not
    /// (app).</summary>
    private static bool HoldsPackedApp(string folder)
    {
        var resources = Path.Combine(folder, "resources");
        return File.Exists(Path.Combine(resources, "app.asar")) || Directory.Exists(Path.Combine(resources, "app"));
    }

    /// <summary>Whether a folder holds no program of its own: no .exe directly in it, whatever its subfolders
    /// hold. A folder that cannot be read is not known to hold none.</summary>
    private static bool HoldsNoProgram(string folder)
    {
        try
        {
            return !Directory.EnumerateFiles(folder)
                .Any(file => string.Equals(Path.GetExtension(file), ".exe", StringComparison.OrdinalIgnoreCase));
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            return false;
        }
    }

    /// <summary>The folders in <paramref name="dir"/>. A folder that cannot be read has none.</summary>
    private static List<string> Subfolders(string dir)
    {
        try
        {
            return Directory.EnumerateDirectories(dir).ToList();
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
