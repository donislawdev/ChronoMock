using ChronoMock.App;

namespace ChronoMock.App.Tests;

/// <summary>
/// The window-build test (method from the betterwindowsservices project): constructing the real window on
/// the real application catches the failures a human otherwise finds by running the app - a theme
/// dictionary that throws at startup, or a resource key that does not resolve. It does NOT catch pixels,
/// hierarchy, or spacing - those still need a look (a screenshot), never only a green test.
/// </summary>
public class WindowBuildTests
{
    [Fact]
    public void MainWindow_constructs_with_the_theme_applied()
    {
        var window = WpfTestHost.InvokeSettled(() => new MainWindow());
        Assert.NotNull(window);
    }

    [Fact]
    public void MainWindow_resolves_its_localized_title_from_the_merged_strings()
    {
        var title = WpfTestHost.InvokeSettled(() => new MainWindow().Title);
        Assert.Equal("Chrono Mock", title);
    }
}
