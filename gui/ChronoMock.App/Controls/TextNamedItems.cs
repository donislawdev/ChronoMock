using System.Windows;
using System.Windows.Controls;

namespace ChronoMock.App.Controls;

/// <summary>
/// <see cref="TextNamedList"/> for a list with no header: a row of buttons, a column of readings. A type of
/// its own only because the base control is.
/// </summary>
public class TextNamedItems : ItemsControl
{
    protected override DependencyObject GetContainerForItemOverride() => new TextNamedRow();
}
