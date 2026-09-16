using System.Windows.Controls;

namespace ChronoMock.App.Views;

/// <summary>
/// The third phase: a session that is over, led by its verdict, with how it ended, what the application
/// lived through, the audit behind the verdict, the sessions before it, and the way on.
/// </summary>
/// <remarks>
/// No code-behind, like the two phases before it and for the same reason (GUI rule 11): the actions act
/// through Commands.X bindings (CopySummary, Repeat, ClearHistory, NewSession and the rest), not event
/// handlers.
/// </remarks>
public partial class ResultPhaseView : UserControl
{
    public ResultPhaseView() => InitializeComponent();
}
