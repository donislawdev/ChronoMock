using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Media;
using ChronoMock.App.Views;

namespace ChronoMock.App.Tests;

/// <summary>
/// The info glyph (PartInfoHint) has to be reachable without a mouse. The first version was a plain
/// TextBlock whose only way of opening its explanation was a hover, and on the setup screen that
/// explanation carries the product's whole promise - the real clock is never touched. Tab could not
/// land on it and a screen reader had nothing to read.
///
/// Two halves. The part: a glyph wearing the style takes keyboard focus, is a tab stop, asks the
/// toolkit to open its tooltip on that focus, and tells assistive technology what it is (a localised
/// name) and what it says (the ToolTip mirrored as HelpText). The use: on the laid-out setup screen the
/// intro text is exposed as HelpText by exactly one element, and that element is focusable - so the
/// promise stays reachable if someone swaps the part under it.
///
/// What this does NOT prove: that the tooltip actually opens when focus arrives by Tab. That is the
/// toolkit's doing (ToolTipService.ShowsToolTipOnKeyboardFocus, windowsdesktop 6.0+) and needs a shown
/// window with real keyboard input, which is measured live through UI Automation, not here.
/// </summary>
public class InfoHintTests
{
    private const string SampleExplanation = "An explanation carried by the glyph.";

    [Fact]
    public void The_info_glyph_is_a_tab_stop_that_shows_its_tooltip_on_keyboard_focus()
    {
        var read = WpfTestHost.InvokeSettled(() =>
        {
            var glyph = new TextBlock
            {
                Style = (Style)Application.Current.FindResource("PartInfoHint"),
                ToolTip = SampleExplanation,
            };
            var host = new StackPanel();
            host.Children.Add(glyph);
            LayoutProbe.Settle(host, 200, 100);

            return (
                glyph.Focusable,
                KeyboardNavigation.GetIsTabStop(glyph),
                ToolTipService.GetShowsToolTipOnKeyboardFocus(glyph),
                AutomationProperties.GetName(glyph),
                AutomationProperties.GetHelpText(glyph),
                Application.Current.TryFindResource("part.info_hint") as string);
        });

        Assert.True(read.Focusable, "the glyph cannot take keyboard focus, so Tab skips it");
        Assert.True(read.Item2, "the glyph is focusable but not a tab stop, so Tab still skips it");
        Assert.True(read.Item3 == true, "the glyph does not ask for its tooltip on keyboard focus");
        Assert.False(string.IsNullOrWhiteSpace(read.Item6), "part.info_hint is not in the default strings");
        Assert.Equal(read.Item6, read.Item4); // the accessible name is the localised one, not the glyph character
        Assert.Equal(SampleExplanation, read.Item5); // the explanation is readable as HelpText, not only as a hover
    }

    [Fact]
    public void The_setup_intro_is_exposed_by_one_focusable_element()
    {
        var (carriers, intro) = WpfTestHost.InvokeSettled(() =>
        {
            var intro = Application.Current.TryFindResource("setup.intro") as string;
            var view = new SetupPhaseView();
            LayoutProbe.Settle(view);

            var carriers = Descendants(view)
                .Where(element => AutomationProperties.GetHelpText(element) == intro)
                .Select(element => (element.GetType().Name, element.Focusable))
                .ToList();
            return (carriers, intro);
        });

        Assert.False(string.IsNullOrWhiteSpace(intro), "setup.intro is not in the default strings");
        Assert.True(
            carriers.Count == 1,
            $"expected exactly one element on the setup screen to expose the intro as HelpText, found {carriers.Count}");
        Assert.True(carriers[0].Focusable, $"the intro sits on a {carriers[0].Name} that keyboard focus cannot reach");
    }

    // The visual tree after layout, so template parts count as well as the view's own elements.
    private static IEnumerable<FrameworkElement> Descendants(DependencyObject root)
    {
        for (int i = 0; i < VisualTreeHelper.GetChildrenCount(root); i++)
        {
            var child = VisualTreeHelper.GetChild(root, i);
            if (child is FrameworkElement element)
            {
                yield return element;
            }

            foreach (var deeper in Descendants(child))
            {
                yield return deeper;
            }
        }
    }
}
