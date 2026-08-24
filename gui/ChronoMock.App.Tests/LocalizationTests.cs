using System.Windows;
using ChronoMock.App.Localization;

namespace ChronoMock.App.Tests;

/// <summary>
/// The interface strings mechanism (untouchable rule 15): keys not literals, every language file the same
/// key set, and the language list discovered by scanning the folder rather than a hardcoded list.
/// </summary>
public class LocalizationTests
{
    [Fact]
    public void English_and_Polish_have_the_same_key_set()
    {
        // XamlReader builds WPF objects, so load on the UI thread.
        var en = WpfTestHost.Invoke(() => KeysOf(LocalizationService.Load("en")));
        var pl = WpfTestHost.Invoke(() => KeysOf(LocalizationService.Load("pl")));
        Assert.Equal(en, pl);
    }

    [Fact]
    public void Available_cultures_are_discovered_by_scanning_the_folder()
    {
        var cultures = LocalizationService.AvailableCultures();
        Assert.Contains("en", cultures);
        Assert.Contains("pl", cultures);
    }

    private static HashSet<string> KeysOf(ResourceDictionary dictionary)
        => dictionary.Keys.Cast<object>().Select(key => key.ToString()!).ToHashSet(StringComparer.Ordinal);
}
