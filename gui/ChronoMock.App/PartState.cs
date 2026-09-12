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

    /// <summary>What an empty field is for, shown inside it until something is typed.</summary>
    /// <remarks>
    /// 🔴 It is a VALUE like the one above, so the catalogue can show the state, and a translation key
    /// resolves to it exactly as every other user-visible string does (untouchable rule 15).
    ///
    /// The hint stays while the field has focus and goes only when there is text, which is the opposite
    /// of the older habit of clearing it on focus. A search box people click into and then think about
    /// is precisely when the hint is still worth reading, and a field that empties its own explanation
    /// the moment somebody looks at it is answering a question nobody asked.
    /// </remarks>
    public static readonly DependencyProperty PlaceholderProperty = DependencyProperty.RegisterAttached(
        "Placeholder", typeof(string), typeof(PartState), new PropertyMetadata(string.Empty));

    public static string GetPlaceholder(DependencyObject element)
        => (string)element.GetValue(PlaceholderProperty);

    public static void SetPlaceholder(DependencyObject element, string value)
        => element.SetValue(PlaceholderProperty, value);

    /// <summary>This is the thing to do next, and it wears the accent while that is true.</summary>
    /// <remarks>
    /// 🔴 A STATE RATHER THAN A STYLE, so that "the accent marks the next step" can be a fact about the
    /// application instead of a decision each screen makes again. A screen binds it to whatever makes the
    /// step next - an empty target, a valid form - and the accent moves on its own.
    ///
    /// The alternative was a second accent style to swap in, which cannot be done by a trigger and would
    /// have meant a second copy of the four accent values. One suggested control at a time is the screen's
    /// job to arrange, not something the drawing can check.
    /// </remarks>
    public static readonly DependencyProperty IsSuggestedProperty = DependencyProperty.RegisterAttached(
        "IsSuggested", typeof(bool), typeof(PartState), new PropertyMetadata(false));

    public static bool GetIsSuggested(DependencyObject element)
        => (bool)element.GetValue(IsSuggestedProperty);

    public static void SetIsSuggested(DependencyObject element, bool value)
        => element.SetValue(IsSuggestedProperty, value);
}
