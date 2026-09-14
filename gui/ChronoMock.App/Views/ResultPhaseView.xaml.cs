using System.Windows.Controls;

namespace ChronoMock.App.Views;

/// <summary>
/// The third phase: a session that is over, led by its verdict, with how it ended, what the application
/// lived through, the audit behind the verdict, the sessions before it, and the way on.
/// </summary>
/// <remarks>
/// No behaviour, like the two phases before it and for the same reason: this is a drawing under review. The
/// buttons are inert until the slice that moves the product onto the parts library, where they get the
/// handlers the shipped panel already has.
/// </remarks>
public partial class ResultPhaseView : UserControl
{
    public ResultPhaseView() => InitializeComponent();
}
