namespace ChronoMock.App.Tests;

/// <summary>
/// Proves the literal guard reddens on a real literal (deterministic, via synthetic input - stronger than
/// hand-inserting and removing one) and that the real views are clean (zasady/13 section 5).
/// </summary>
public class LiteralGuardTests
{
    [Fact]
    public void Views_have_no_design_literals()
    {
        var violations = XamlLiteralGuard.ScanViews(TestPaths.AppDirectory());
        Assert.True(
            violations.Count == 0,
            "design literals found in views (use a named resource):\n"
                + string.Join("\n", violations.Select(v => $"  {v.File}:{v.Line} [{v.Kind}] {v.Snippet}")));
    }

    [Fact]
    public void Guard_reddens_on_a_literal_colour()
        => Assert.NotEmpty(XamlLiteralGuard.FindViolations("x.xaml", """<TextBlock Foreground="#FFFF0000" />"""));

    [Fact]
    public void Guard_reddens_on_a_literal_font_size()
        => Assert.NotEmpty(XamlLiteralGuard.FindViolations("x.xaml", """<TextBlock FontSize="13" />"""));

    [Fact]
    public void Guard_reddens_on_a_literal_margin()
        => Assert.NotEmpty(XamlLiteralGuard.FindViolations("x.xaml", """<Border Margin="7" />"""));

    [Fact]
    public void Guard_allows_named_resources_and_zero()
    {
        Assert.Empty(XamlLiteralGuard.FindViolations(
            "x.xaml",
            """<TextBlock Foreground="{DynamicResource BrushTextPrimary}" FontSize="{StaticResource FontSizeBody}" Margin="{StaticResource SpaceSm}" />"""));
        Assert.Empty(XamlLiteralGuard.FindViolations("x.xaml", """<Border Margin="0" />"""));
    }
}
