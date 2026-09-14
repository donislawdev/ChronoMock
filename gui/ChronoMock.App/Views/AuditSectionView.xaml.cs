using System.Windows.Controls;

namespace ChronoMock.App.Views;

/// <summary>
/// What the application read: the audit block in its three states - waiting, never coming, and the folded
/// report with its counts - shared by the session phase and the result phase.
/// </summary>
/// <remarks>
/// No behaviour, like the phases that host it. It is a drawing bound to the session model, and the model
/// says which state applies.
/// </remarks>
public partial class AuditSectionView : UserControl
{
    public AuditSectionView() => InitializeComponent();
}
