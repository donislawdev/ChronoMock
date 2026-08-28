using System;
using System.IO;
using ChronoMock.App;

namespace ChronoMock.App.Tests;

/// <summary>
/// The GUI's Electron/Chromium detector (ADR-8): it must recognise a Chromium target the same way the
/// CLI does, so the panel refuses a native session that would mislead (untouchable rule 4) and hands off
/// the CLI command instead.
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

    [Fact]
    public void Cli_command_quotes_the_path()
    {
        Assert.Equal(
            "chrono run \"C:\\Apps\\Foo.exe\" --mode x60",
            ChromiumTarget.CliCommand("C:\\Apps\\Foo.exe"));
    }
}
