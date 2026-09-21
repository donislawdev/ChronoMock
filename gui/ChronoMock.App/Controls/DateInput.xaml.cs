using System.Windows;
using System.Windows.Controls;

namespace ChronoMock.App.Controls;

/// <summary>
/// The shared date input: an ISO date field with a calendar popup. Bound to any DataContext that exposes
/// <c>DateText</c> and <c>SelectedDate</c> (<see cref="ChronoMock.App.MomentField"/>, the scenario
/// parameter inputs), so every date on the screen is typed and picked the same way. <see cref="MomentInput"/>
/// is this control plus a time field.
/// </summary>
public partial class DateInput : UserControl
{
    public DateInput() => InitializeComponent();

    /// <summary>The name a screen reader announces for the field, for a consumer whose label is not a
    /// control the field can point at (the scenario parameters name theirs after the parameter). Unset,
    /// the field keeps the plain text box's own accessible name.</summary>
    public static readonly DependencyProperty AccessibleNameProperty = DependencyProperty.Register(
        nameof(AccessibleName), typeof(string), typeof(DateInput), new PropertyMetadata(null));

    public string? AccessibleName
    {
        get => (string?)GetValue(AccessibleNameProperty);
        set => SetValue(AccessibleNameProperty, value);
    }

    // Close the calendar popup as soon as a day is picked.
    private void OnDatePicked(object sender, SelectionChangedEventArgs e) => CalendarToggle.IsChecked = false;
}
