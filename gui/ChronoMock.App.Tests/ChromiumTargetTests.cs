using System;
using System.IO;
using ChronoMock.App;

namespace ChronoMock.App.Tests;

/// <summary>
/// The GUI's Electron/Chromium detector (ADR-8/ADR-9): it must recognise a Chromium target the same way
/// the CLI does, so the panel routes it to the CDP core (skipping the bitness gate, labelling coverage as
/// JS contexts) instead of a native injection that could not reach it (untouchable rule 4).
/// </summary>
public class ChromiumTargetTests
{
    [Fact]
    public void Detects_a_chromium_folder_by_its_runtime_files()
    {
        var dir = Path.Combine(Path.GetTempPath(), "chrono-gui-cdp-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(dir);
        try
        {
            File.WriteAllText(Path.Combine(dir, "icudtl.dat"), "x");
            File.WriteAllText(Path.Combine(dir, "v8_context_snapshot.bin"), "x");
            var exe = Path.Combine(dir, "App.exe");
            File.WriteAllText(exe, "x");
            Assert.True(ChromiumTarget.IsChromium(exe));
        }
        finally
        {
            Directory.Delete(dir, true);
        }
    }

    [Fact]
    public void A_plain_folder_is_not_chromium()
    {
        var dir = Path.Combine(Path.GetTempPath(), "chrono-gui-native-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(dir);
        try
        {
            var exe = Path.Combine(dir, "native.exe");
            File.WriteAllText(exe, "x");
            Assert.False(ChromiumTarget.IsChromium(exe));
        }
        finally
        {
            Directory.Delete(dir, true);
        }
    }
}
