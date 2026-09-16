using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;

namespace ChronoMock.App.Controls;

/// <summary>
/// A search field over a list: it clears itself with a glyph in its right end and submits its first hit when
/// Enter is pressed. One control so the two behaviours travel together and the catalogue can show them -
/// neither is expressible as a style. The drawing is
/// the parts library's TextBox (implicit style) with a clear glyph laid over it, the same shape MomentInput
/// lays a calendar toggle in. It knows nothing about what it is searching: the consumer binds
/// <see cref="Text"/>, <see cref="Placeholder"/> and <see cref="SubmitCommand"/>.
/// </summary>
public partial class SearchBox : UserControl
{
    public SearchBox()
    {
        InitializeComponent();
    }

    /// <summary>What the person has typed. Two-way by default, so a consumer binds a filter straight to it.</summary>
    public static readonly DependencyProperty TextProperty = DependencyProperty.Register(
        nameof(Text),
        typeof(string),
        typeof(SearchBox),
        new FrameworkPropertyMetadata(string.Empty, FrameworkPropertyMetadataOptions.BindsTwoWayByDefault));

    public string Text
    {
        get => (string)GetValue(TextProperty);
        set => SetValue(TextProperty, value);
    }

    /// <summary>The hint shown in the empty field - a translation key the consumer resolves (rule 15).</summary>
    public static readonly DependencyProperty PlaceholderProperty = DependencyProperty.Register(
        nameof(Placeholder), typeof(string), typeof(SearchBox), new PropertyMetadata(string.Empty));

    public string Placeholder
    {
        get => (string)GetValue(PlaceholderProperty);
        set => SetValue(PlaceholderProperty, value);
    }

    /// <summary>Run when Enter is pressed in the field - the keyboard equivalent of clicking the top hit.
    /// Skipped when it cannot execute, so a list showing nothing does nothing.</summary>
    public static readonly DependencyProperty SubmitCommandProperty = DependencyProperty.Register(
        nameof(SubmitCommand), typeof(ICommand), typeof(SearchBox), new PropertyMetadata(null));

    public ICommand? SubmitCommand
    {
        get => (ICommand?)GetValue(SubmitCommandProperty);
        set => SetValue(SubmitCommandProperty, value);
    }

    /// <summary>The clear button's accessible name - a translation key the consumer resolves (rule 15).</summary>
    public static readonly DependencyProperty ClearHintProperty = DependencyProperty.Register(
        nameof(ClearHint), typeof(string), typeof(SearchBox), new PropertyMetadata(string.Empty));

    public string ClearHint
    {
        get => (string)GetValue(ClearHintProperty);
        set => SetValue(ClearHintProperty, value);
    }

    // Clearing empties the field and hands the caret back, so the next thing typed narrows from the whole
    // list again. It never touches what the consumer chose from the list - the filter and the choice are
    // separate, which is the whole design of the picker behind it.
    private void OnClear(object sender, RoutedEventArgs e)
    {
        // SetCurrentValue, not the CLR setter: a plain SetValue would replace the consumer's binding with a
        // local value, so a filter bound to Text would stop updating after the first clear. This changes the
        // value while leaving the binding in place.
        SetCurrentValue(TextProperty, string.Empty);
        Field.Focus();
    }

    // Enter submits the first hit through the bound command. CanExecute is checked so an empty list does
    // nothing, and the key is marked handled only when the command ran, leaving Enter free otherwise.
    private void OnFieldKeyDown(object sender, KeyEventArgs e)
    {
        if (e.Key != Key.Enter)
        {
            return;
        }

        if (SubmitCommand is { } command && command.CanExecute(null))
        {
            command.Execute(null);
            e.Handled = true;
        }
    }
}
