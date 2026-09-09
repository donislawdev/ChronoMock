using ChronoMock.App;
using ChronoMock.Protocol;

namespace ChronoMock.App.Tests;

/// <summary>
/// The About window: that it builds against the real theme, and that the two pieces of text-shaping it
/// does are right in both directions - when they find what they are looking for, and when they do not.
/// </summary>
public class AboutTests
{
    [Fact]
    public void AboutDialog_constructs_with_the_theme_applied()
    {
        // Same method as the other window-build tests: constructing the real window on the real
        // application resolves every StaticResource and DynamicResource exactly as at runtime, which is
        // where a missing key or a throwing dictionary shows up. It proves nothing about how it LOOKS -
        // that was measured on renders and belongs in the changelog.
        var dialog = WpfTestHost.InvokeSettled(
            () => new Views.AboutDialog(LicenseClient.ForRepo(TestPaths.RepoRoot())));

        Assert.NotNull(dialog);
    }

    [Fact]
    public void AboutDialog_titles_itself_from_the_merged_strings()
    {
        // The title is what names this window to assistive tech and to the shell, and it was empty on the
        // first build of it: the key had been put on the title BAR, which draws the text without the
        // window ever having a name. A window with no name is not a cosmetic problem.
        var title = WpfTestHost.InvokeSettled(
            () => new Views.AboutDialog(LicenseClient.ForRepo(TestPaths.RepoRoot())).Title);

        Assert.False(string.IsNullOrWhiteSpace(title), "the About window must carry a title");
        Assert.DoesNotContain("about.", title, StringComparison.Ordinal);
    }

    [Fact]
    public void The_copyright_line_drops_the_licence_sentence_the_metadata_appends()
    {
        // The assembly attribute says "... Licensed under GPL-3.0-only.", which is right for the file
        // property and wrong two rows above a line that states the licence on its own.
        Assert.Equal(
            "Copyright (C) 2026 Someone",
            AppLicence.WithoutLicenceSentence("Copyright (C) 2026 Someone. Licensed under GPL-3.0-only."));
    }

    [Fact]
    public void A_copyright_line_it_does_not_recognise_is_returned_whole()
    {
        // Losing the holder would be far worse than showing one sentence too many, so anything this does
        // not recognise passes through untouched.
        const string reworded = "(c) 2026 Someone, all rights reserved";
        Assert.Equal(reworded, AppLicence.WithoutLicenceSentence(reworded));
    }

    [Fact]
    public void The_component_list_drops_the_notice_the_core_prints_above_it()
    {
        var listing = "Chrono Mock 0.1.0 (x64)\nABSOLUTELY NO WARRANTY\n\n"
            + LicenseClient.FirstGroupHeading + "\n  itoa 1.0.18\n";

        var register = LicenseClient.RegisterOnly(listing);

        Assert.StartsWith(LicenseClient.FirstGroupHeading, register, StringComparison.Ordinal);
        Assert.DoesNotContain("ABSOLUTELY NO WARRANTY", register, StringComparison.Ordinal);
        Assert.Contains("itoa 1.0.18", register, StringComparison.Ordinal);
    }

    [Fact]
    public void A_listing_without_the_heading_is_shown_whole_rather_than_emptied()
    {
        // The direction that matters. Showing the notice twice is a blemish, and showing an empty
        // component list would be this tool claiming there is nothing third-party inside it - which is
        // the one answer it must never give by accident (untouchable rule 4).
        const string unexpected = "the core said something else entirely";
        Assert.Equal(unexpected, LicenseClient.RegisterOnly(unexpected));
    }
}
