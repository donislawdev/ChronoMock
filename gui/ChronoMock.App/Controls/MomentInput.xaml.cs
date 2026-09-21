using System.Windows.Controls;

namespace ChronoMock.App.Controls;

/// <summary>
/// The shared moment input: the shared <see cref="DateInput"/> (ISO date field with a calendar popup) and
/// a 24-hour time field. Bound to a <see cref="ChronoMock.App.MomentField"/> DataContext (like ClockTile
/// binds a ClockView), so the substitution panel, the running session's jump and the calculator base reuse
/// one control. The per-part errors are the consumer's to show, below the row.
/// </summary>
public partial class MomentInput : UserControl
{
    public MomentInput() => InitializeComponent();
}
