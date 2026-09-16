using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using System.Text.RegularExpressions;
using ChronoMock.App;
using Wpf.Ui.Controls;

namespace ChronoMock.App.Tests;

/// <summary>
/// No window may take a Mica or Acrylic backdrop, because that costs ClearType across the whole window.
/// </summary>
/// <remarks>
/// 🔴 THE CHAIN, measured link by link on another project of the owner's and carried here as a FACT
/// rather than as a preference. ThemeMode="Dark" applies a Mica backdrop (DwmGetWindowAttribute, attribute
/// 38: MICA with the library, AUTO without). A window with Mica is translucent as far as DWM is concerned.
/// WPF turns ClearType OFF on a translucent surface - counted as text pixels carrying a colour fringe,
/// 80.5% without the backdrop against 0% with it. The text then reads soft, visible at 8x on a screenshot.
///
/// 🔴 It is NOT a layered window. WS_EX_LAYERED is false either way, so the obvious first hypothesis is
/// wrong and is written down here so nobody spends an afternoon on it again. Mica lives on the DWM side.
///
/// WHY A TEXT SCAN, when this project prefers measuring the effect. The effect is a DWM attribute on a
/// real HWND, and everything this suite renders is laid out off-screen with no HWND at all - an offscreen
/// bitmap never passes through DWM, so it cannot show the difference. Two weaker checks together are what
/// is actually available: the resolved property on a constructed window, which proves the value rather
/// than the spelling, and a scan over every XAML file, which is the only thing that can see a window
/// nobody has added to a test yet.
///
/// WHAT THIS DOES NOT PROVE: that ClearType is on. It proves that the setting known to remove it is not
/// present. If the owner ever wants Mica, this is the test to argue with, and the argument has a price
/// attached to it: the sharpness of every character in the window.
///
/// 🔴 REVERSE PROBE, RUN, AND IT CORRECTED THIS COMMENT. The first version of these remarks promised that
/// deleting WindowBackdropType from MainWindow.xaml would redden both assertions. Measured, it reddens
/// ONE: the scan names the file, and the property check passes, because the toolkit's own default for
/// FluentWindow is already None. So the attribute is belt and braces rather than the thing holding the
/// line, and the two checks answer different questions after all - the scan asks whether a window said
/// it, the property asks what WPF resolved, which is the only one that could see a backdrop arriving from
/// a style or from code-behind.
///
/// The probe that reddens both is setting WindowBackdropType="Mica" - measured, 2 of 3 - and that is the
/// edit this guard actually exists to stop.
/// </remarks>
public class BackdropGuardTests
{
    [Fact]
    public void The_main_window_resolves_to_no_backdrop_rather_than_merely_spelling_one()
    {
        var backdrop = WpfTestHost.InvokeSettled(() => new MainWindow().WindowBackdropType);

        Assert.Equal(WindowBackdropType.None, backdrop);
    }

    [Fact]
    public void Every_window_in_the_repository_declares_no_backdrop()
    {
        var (windows, missing) = ScanWindows();

        // The floor is a LITERAL, not a count taken from the same scan: a guard that counts what it found
        // and then checks its own count passes just as happily over an empty directory.
        Assert.True(
            windows >= FluentWindowsAtLeast,
            $"only {windows} windows were scanned, expected at least {FluentWindowsAtLeast} - the views "
                + "moved or the scan stopped reading them, so this guard was about to pass over nothing");

        Assert.True(
            missing.Count == 0,
            "these windows do not declare WindowBackdropType=\"None\", so they take the system default - "
                + $"a Mica backdrop, which turns ClearType off for every character in them:"
                + $"{Environment.NewLine}{string.Join(Environment.NewLine, missing)}");
    }

    /// <summary>
    /// ThemeMode is the other door to the same backdrop, and it is shut for a second, independent reason:
    /// set alongside the wpfui dictionaries it throws at startup. Both reasons are worth keeping, because
    /// a later toolkit release could fix the throw and leave the ClearType cost exactly where it is.
    /// </summary>
    [Fact]
    public void No_file_sets_ThemeMode()
    {
        var offenders = new List<string>();

        foreach (var (relative, text) in AppXaml())
        {
            if (text.Contains("ThemeMode=", StringComparison.Ordinal))
            {
                offenders.Add(relative);
            }
        }

        Assert.True(
            offenders.Count == 0,
            "ThemeMode applies a Mica backdrop, which costs ClearType across the whole window, and set "
                + $"alongside the wpfui dictionaries it also throws at startup:{Environment.NewLine}"
                + string.Join(Environment.NewLine, offenders));
    }

    /// <summary>MainWindow, the About dialog and the message dialog.</summary>
    private const int FluentWindowsAtLeast = 3;

    private static readonly Regex FluentWindowTag =
        new("<ui:FluentWindow\\b", RegexOptions.Compiled);

    private static readonly Regex DeclaresNone =
        new("WindowBackdropType\\s*=\\s*\"None\"", RegexOptions.Compiled);

    private static (int Windows, IReadOnlyList<string> Missing) ScanWindows()
    {
        var missing = new List<string>();
        int windows = 0;

        foreach (var (relative, text) in AppXaml())
        {
            if (!FluentWindowTag.IsMatch(text))
            {
                continue;
            }

            windows++;
            if (!DeclaresNone.IsMatch(text))
            {
                missing.Add(relative);
            }
        }

        return (windows, missing);
    }

    private static IEnumerable<(string Relative, string Text)> AppXaml()
    {
        var appDirectory = TestPaths.AppDirectory();

        foreach (var file in Directory.EnumerateFiles(appDirectory, "*.xaml", SearchOption.AllDirectories))
        {
            var relative = Path.GetRelativePath(appDirectory, file).Replace('\\', '/');
            if (relative.Contains("/bin/", StringComparison.OrdinalIgnoreCase)
                || relative.Contains("/obj/", StringComparison.OrdinalIgnoreCase))
            {
                continue;
            }

            yield return (relative, File.ReadAllText(file));
        }
    }
}
