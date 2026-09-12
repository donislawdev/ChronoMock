using System.Windows;

namespace ChronoMock.App;

/// <summary>
/// States a part can be put into that the toolkit has no property for.
/// </summary>
/// <remarks>
/// 🔴 These are VALUES, never callbacks. The parts library draws - it does not decide anything and it
/// does not call back into a screen. A drawing that accepted a handler would grow logic inside the look,
/// and the look would stop being something a still catalogue can show truthfully.
///
/// <see cref="HasErrorProperty"/> exists rather than leaning on Validation.HasError because the error may
/// come from anywhere - a binding rule, a view model, or a catalogue entry demonstrating the state. The
/// drawing should not care which, and a catalogue that could not set it would have to draw a hand-made
/// picture of the error state, which is the exact failure this library is built to avoid.
/// </remarks>
public static class PartState
{
    /// <summary>The value in this field is not acceptable, and the field must say so.</summary>
    public static readonly DependencyProperty HasErrorProperty = DependencyProperty.RegisterAttached(
        "HasError", typeof(bool), typeof(PartState), new PropertyMetadata(false));

    public static bool GetHasError(DependencyObject element)
        => (bool)element.GetValue(HasErrorProperty);

    public static void SetHasError(DependencyObject element, bool value)
        => element.SetValue(HasErrorProperty, value);
}
