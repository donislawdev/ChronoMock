using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;

namespace ChronoMock.App.Tests;

/// <summary>
/// The value styles, measured as EFFECT rather than as markup: a TextBlock given the style comes out with
/// the same three properties the calculator used to write at each element.
/// </summary>
/// <remarks>
/// 🔴 THIS IS THE GUARD FOR A SILENT CHANGE. Replacing three inline property sets with a named style is
/// only correct if the style produces what the inlines produced, and nothing else in this project could
/// have told us: the calculator's render fixture has no DataContext, so its value cells are EMPTY on the
/// rendered sheet and a font or colour change there is invisible (that gap is a known open question, not
/// this class's to close). So the style is applied to a TextBlock and read back.
///
/// Reversal probe: change either setter in PartValue and this fails on the property that moved.
/// </remarks>
public class ValueStyleTests
{
    [Fact]
    public void A_value_reads_in_the_primary_ink_at_caption_size()
    {
        // The pair the calculator wrote out three times. Primary ink, not the caption's secondary: a
        // caption describes a value and this IS one, and giving both the quiet ink made the result column
        // read as one grey block.
        var (size, ink, expectedSize, expectedInk) = WpfTestHost.InvokeSettled(() =>
        {
            var block = new TextBlock { Style = (Style)Application.Current.Resources["PartValue"] };
            return (
                block.FontSize,
                ((SolidColorBrush)block.Foreground).Color,
                (double)Application.Current.Resources["FontSizeCaption"],
                ((SolidColorBrush)Application.Current.Resources["BrushTextPrimary"]).Color);
        });

        Assert.Equal(expectedSize, size);
        Assert.Equal(expectedInk, ink);
    }

    [Fact]
    public void A_value_that_can_be_copied_is_set_in_the_data_face()
    {
        // The mono variant, through BasedOn: it has to keep the base's size and ink as well as add the
        // face, or the two kinds of value stop matching each other down the column.
        var (family, size, ink, expectedFamily, expectedSize, expectedInk) = WpfTestHost.InvokeSettled(() =>
        {
            var block = new TextBlock { Style = (Style)Application.Current.Resources["PartValueMono"] };
            return (
                block.FontFamily,
                block.FontSize,
                ((SolidColorBrush)block.Foreground).Color,
                (FontFamily)Application.Current.Resources["FontFamilyMono"],
                (double)Application.Current.Resources["FontSizeCaption"],
                ((SolidColorBrush)Application.Current.Resources["BrushTextPrimary"]).Color);
        });

        Assert.Equal(expectedFamily, family);
        Assert.Equal(expectedSize, size);
        Assert.Equal(expectedInk, ink);
    }

    [Fact]
    public void A_value_wraps_rather_than_running_off_its_column()
    {
        // The third property the inlines carried. A FILETIME or a custom format string is longer than the
        // column it sits in, and the calculator's columns do not scroll sideways.
        var wrapping = WpfTestHost.InvokeSettled(
            () => new TextBlock { Style = (Style)Application.Current.Resources["PartValue"] }.TextWrapping);

        Assert.Equal(TextWrapping.Wrap, wrapping);
    }
}
