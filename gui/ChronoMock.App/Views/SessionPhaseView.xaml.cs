using System.Windows.Controls;

namespace ChronoMock.App.Views;

/// <summary>
/// The second phase: a session under way, its two clocks, the controls that act on it in flight, and the
/// audit behind the verdict.
/// </summary>
/// <remarks>
/// No code-behind, like the setup phase and for the same reason (GUI rule 11): the in-flight controls act
/// through Commands.X bindings (Speed, Jump, SetCustomSpeed, JumpToEntered, Stop), not event handlers.
/// </remarks>
public partial class SessionPhaseView : UserControl
{
    public SessionPhaseView() => InitializeComponent();
}
