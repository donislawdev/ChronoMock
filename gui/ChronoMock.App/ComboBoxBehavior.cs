using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;

namespace ChronoMock.App;

/// <summary>
/// Runs a command when a ComboBox opens its drop-down.
/// </summary>
/// <remarks>
/// A ComboBox carries no Command of its own, and DropDownOpened is a plain CLR event rather than a routed
/// one, so it cannot be bound and cannot bubble to a window. A view that wants "re-check the list as it
/// opens" would otherwise need a handler in its code-behind, which a phase view is not allowed to hold (GUI
/// rule 11). This turns the event into a command the markup binds, keeping the drawing logic-free. It is a
/// value the view sets, never a callback it registers - the same contract <see cref="PartState"/> keeps.
/// </remarks>
public static class ComboBoxBehavior
{
    /// <summary>The command run each time the ComboBox opens its drop-down. Skipped when it cannot execute.</summary>
    public static readonly DependencyProperty OpenedCommandProperty = DependencyProperty.RegisterAttached(
        "OpenedCommand",
        typeof(ICommand),
        typeof(ComboBoxBehavior),
        new PropertyMetadata(null, OnOpenedCommandChanged));

    public static ICommand? GetOpenedCommand(DependencyObject element)
        => (ICommand?)element.GetValue(OpenedCommandProperty);

    public static void SetOpenedCommand(DependencyObject element, ICommand? value)
        => element.SetValue(OpenedCommandProperty, value);

    private static void OnOpenedCommandChanged(DependencyObject element, DependencyPropertyChangedEventArgs e)
    {
        if (element is not ComboBox box)
        {
            return;
        }

        // Re-attach cleanly, so setting the property a second time does not run the command twice per open.
        box.DropDownOpened -= OnDropDownOpened;
        if (e.NewValue is ICommand)
        {
            box.DropDownOpened += OnDropDownOpened;
        }
    }

    private static void OnDropDownOpened(object? sender, EventArgs e)
    {
        if (sender is ComboBox box && GetOpenedCommand(box) is { } command && command.CanExecute(null))
        {
            command.Execute(null);
        }
    }
}
