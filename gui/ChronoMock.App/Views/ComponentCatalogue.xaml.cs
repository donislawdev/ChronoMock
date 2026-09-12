using System.Windows.Controls;

namespace ChronoMock.App.Views;

/// <summary>
/// The component catalogue: every part of the interface, in every state that can be shown without input.
/// </summary>
/// <remarks>
/// It carries no behaviour on purpose. A catalogue that computed anything would be a second screen to
/// keep working rather than a picture of the parts library, and the parts it shows are drawings, not
/// controls with logic behind them.
/// </remarks>
public partial class ComponentCatalogue : UserControl
{
    public ComponentCatalogue() => InitializeComponent();
}
