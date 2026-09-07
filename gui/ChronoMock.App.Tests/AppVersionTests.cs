using ChronoMock.App;

namespace ChronoMock.App.Tests;

/// <summary>
/// The product version, and what happens to a title format an outside translator can edit.
/// </summary>
public class AppVersionTests
{
    [Fact]
    public void The_running_build_reports_a_version()
    {
        var version = AppVersion.Current;

        Assert.False(string.IsNullOrWhiteSpace(version));

        // "unknown" is the last-resort branch, and reaching it in a normal build would mean the
        // assembly carries no version attribute at all.
        Assert.NotEqual("unknown", version);
        Assert.Contains(".", version, StringComparison.Ordinal);
    }

    [Fact]
    public void A_title_format_receives_the_version()
    {
        Assert.Equal("Chrono Mock 1.2.3", AppVersion.FormatTitle("Chrono Mock {0}", "1.2.3"));
    }

    [Fact]
    public void A_format_without_a_placeholder_is_left_alone()
    {
        // A translator who prefers a bare name gets one, rather than an argument appended for them.
        Assert.Equal("Chrono Mock", AppVersion.FormatTitle("Chrono Mock", "1.2.3"));
    }

    [Fact]
    public void A_format_naming_an_argument_that_does_not_exist_degrades_instead_of_throwing()
    {
        // string.Format throws on {1} when one argument was supplied. Unhandled, that would come
        // out of Apply during startup, and ApplyOrDegrade does not catch FormatException - so a
        // single wrong character in a strings file would stop the window from opening.
        Assert.Equal("Chrono Mock {1}", AppVersion.FormatTitle("Chrono Mock {1}", "1.2.3"));
    }
}
