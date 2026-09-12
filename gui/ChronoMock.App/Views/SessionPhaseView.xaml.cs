using System.Windows.Controls;

namespace ChronoMock.App.Views;

/// <summary>
/// The second phase: a session under way, its two clocks, the controls that act on it in flight, and the
/// audit behind the verdict.
/// </summary>
/// <remarks>
/// No behaviour, like the setup phase and for the same reason: this is a drawing under review. The buttons
/// are inert until the slice that moves the product onto the parts library, where they get the handlers
/// the shipped panel already has.
/// </remarks>
public partial class SessionPhaseView : UserControl
{
    public SessionPhaseView() => InitializeComponent();
}
