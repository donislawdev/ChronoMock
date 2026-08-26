using System.Windows.Controls;

namespace ChronoMock.App.Views;

/// <summary>
/// Date-calculator screen (chrono-mock 7.3). Slice G1 is the layout shell only; the builder controls,
/// live evaluation (via `chrono calc --json`), presets, reverse analysis, and the "use in substitution"
/// bridge arrive in slices G2-G6. No view model yet - the sample values are static preview content.
/// </summary>
public partial class CalculatorView : UserControl
{
    public CalculatorView()
    {
        InitializeComponent();
    }
}
