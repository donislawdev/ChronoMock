using System.Windows.Controls;

namespace ChronoMock.App.Controls;

/// <summary>
/// The shared moment input: an ISO date field with a calendar popup, an optional 24-hour time field, and
/// inline per-part errors. Bound to a <see cref="ChronoMock.App.MomentField"/> DataContext (like ClockTile
/// binds a ClockView), so both the substitution panel and the calculator base reuse one control.
/// </summary>
public partial class MomentInput : UserControl
{
    public MomentInput() => InitializeComponent();

    // Close the calendar popup as soon as a day is picked.
    private void OnDatePicked(object sender, SelectionChangedEventArgs e) => CalendarToggle.IsChecked = false;
}
