using System.Windows.Controls;

namespace ChronoMock.App.Views;

/// <summary>
/// The setup phase of the rebuilt substitution screen: what is about to be run, and under what clock.
/// </summary>
/// <remarks>
/// 🔴 IT HAS NO CODE, AND THAT IS THE POINT. Every button here is a shape without a handler, because a
/// phase view is layout and bindings and nothing else (GUI rule 11). The measured fault it replaces had
/// 29 event handlers written into the markup of one screen, which is what made that screen impossible to
/// rearrange - the work and the drawing were the same file.
///
/// Where the actions go is a decision for the slice that moves the product over, not for the drawing.
/// </remarks>
public partial class SetupPhaseView : UserControl
{
    public SetupPhaseView() => InitializeComponent();
}
