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

    /// <summary>
    /// An installed Chromium browser, laid out as two installed browsers were measured (2026-11-09): the
    /// exe alone beside a folder named after its version, holding the runtime and the library the exe
    /// loads - Chromium's own chrome.dll, or one named after the exe. An update can leave the previous
    /// version's folder beside the new one. The same cases as the CLI's test.
    /// </summary>
    [Fact]
    public void An_installed_browser_is_told_by_the_version_folder_beside_it()
    {
        var dir = Path.Combine(Path.GetTempPath(), "chrono-gui-browser-" + Guid.NewGuid().ToString("N"));
        try
        {
            var chromium = Exe(dir, "chrome.exe");
            FolderWith(dir, "150.0.1234.56", "icudtl.dat", "v8_context_snapshot.bin", "chrome.dll");
            Assert.True(ChromiumTarget.IsChromium(chromium), "Chromium's own library");

            var otherDir = Path.Combine(dir, "other");
            var browser = Exe(otherDir, "browser.exe");
            FolderWith(otherDir, "150.0.2345.67", "icudtl.dat");
            FolderWith(otherDir, "151.0.2400.10", "icudtl.dat", "snapshot_blob.bin", "browser.dll");
            Assert.True(ChromiumTarget.IsChromium(browser), "a library named after the exe, an old version folder left beside");
        }
        finally
        {
            Directory.Delete(dir, true);
        }
    }

    /// <summary>
    /// What does not make a version folder a browser: the runtime without the library the exe loads, a
    /// folder named like an updater's, the library without the runtime, a file with a version's name, or
    /// a library named after neither Chromium nor the exe.
    /// </summary>
    [Theory]
    [InlineData("app.exe", "1.2.3.4", "icudtl.dat|v8_context_snapshot.bin")]
    [InlineData("chrome.exe", "app-1.2.3", "icudtl.dat|v8_context_snapshot.bin|chrome.dll")]
    [InlineData("chrome.exe", "150.0.1234.56", "chrome.dll")]
    [InlineData("chrome.exe", "150.0.1234.56", "")]
    [InlineData("app.exe", "1.2.3.4", "icudtl.dat|snapshot_blob.bin|libcef.dll")]
    public void A_version_folder_is_a_browser_only_with_the_runtime_and_the_library(string exeName, string folder, string files)
    {
        var dir = Path.Combine(Path.GetTempPath(), "chrono-gui-browser-" + Guid.NewGuid().ToString("N"));
        try
        {
            var exe = Exe(dir, exeName);
            if (files.Length == 0)
            {
                File.WriteAllText(Path.Combine(dir, folder), "x");
            }
            else
            {
                FolderWith(dir, folder, files.Split('|'));
            }

            Assert.False(ChromiumTarget.IsChromium(exe));
        }
        finally
        {
            Directory.Delete(dir, true);
        }
    }

    /// <summary>
    /// An Electron application that keeps the runtime and its packed application in a folder beside the exe,
    /// with no program in that folder, is one. A program deeper down does not count, nor a folder named like
    /// one, the folder's name is no part of it, and a program of its own in the folder - a starter's
    /// versioned folder - or a folder
    /// without the application makes it not one. The same cases as the CLI's test.
    /// </summary>
    [Fact]
    public void An_electron_application_with_its_runtime_in_a_folder_beside_the_exe_is_chromium()
    {
        var dir = Path.Combine(Path.GetTempPath(), "chrono-gui-bundled-" + Guid.NewGuid().ToString("N"));
        try
        {
            var exe = Exe(dir, "app.exe");
            FolderWith(dir, "0123abcdef", "icudtl.dat", "snapshot_blob.bin");
            var bundle = Path.Combine(dir, "0123abcdef");
            Assert.False(ChromiumTarget.IsChromium(exe), "a runtime without the application");

            Directory.CreateDirectory(Path.Combine(bundle, "resources", "app"));
            FolderWith(bundle, "tools", "updater.exe");
            Directory.CreateDirectory(Path.Combine(bundle, "cache.exe"));
            Assert.True(ChromiumTarget.IsChromium(exe), "the runtime with the application and no program beside it");

            File.WriteAllText(Path.Combine(bundle, "App.EXE"), "x");
            Assert.False(ChromiumTarget.IsChromium(exe), "a folder with a program of its own is a starter's");
        }
        finally
        {
            Directory.Delete(dir, true);
        }
    }

    [Theory]
    [InlineData("150.0.1234.56", true)]
    [InlineData("1.2", true)]
    [InlineData("150", false)]
    [InlineData("app-1.2.3", false)]
    [InlineData("1..2", false)]
    [InlineData(".1.2", false)]
    [InlineData("1.2.", false)]
    [InlineData("1.2a.3", false)]
    [InlineData("", false)]
    public void A_version_name_is_digits_and_dots(string name, bool expected) =>
        Assert.Equal(expected, ChromiumTarget.IsVersionName(name));

    private static string Exe(string dir, string name)
    {
        Directory.CreateDirectory(dir);
        var exe = Path.Combine(dir, name);
        File.WriteAllText(exe, "x");
        return exe;
    }

    private static void FolderWith(string dir, string name, params string[] files)
    {
        var folder = Path.Combine(dir, name);
        Directory.CreateDirectory(folder);
        foreach (var file in files)
        {
            File.WriteAllText(Path.Combine(folder, file), "x");
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
