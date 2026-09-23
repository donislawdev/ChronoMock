using System.Windows;
using System.Windows.Controls;

namespace ChronoMock.App.Controls;

/// <summary>
/// A HeaderedItemsControl whose rows are named for assistive tech by the text they show. Every list and
/// table with a header uses it (PartTable, PartNoteList), and a view never uses the base control, which
/// ItemNameGuardTests enforces - see <see cref="TextNamedRow"/> for why the name has to come from the row.
/// </summary>
public class TextNamedList : HeaderedItemsControl
{
    protected override DependencyObject GetContainerForItemOverride() => new TextNamedRow();
}
