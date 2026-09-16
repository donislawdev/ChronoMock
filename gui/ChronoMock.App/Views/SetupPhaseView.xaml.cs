using System.Windows.Controls;

namespace ChronoMock.App.Views;

/// <summary>
/// The setup phase of the rebuilt substitution screen: what is about to be run, and under what clock.
/// </summary>
/// <remarks>
/// 🔴 IT HAS NO CODE-BEHIND, AND THAT IS THE POINT. Every action is a Commands.X binding (ChooseTarget,
/// RelativeApply, BrowseFolder, Start and the rest), never an event handler, because a phase view is layout
/// and bindings and nothing else (GUI rule 11). The measured fault it replaces had 29 event handlers
/// written into the markup of one screen, which is what made that screen impossible to rearrange - the work
/// and the drawing were the same file.
/// </remarks>
public partial class SetupPhaseView : UserControl
{
    public SetupPhaseView() => InitializeComponent();
}
