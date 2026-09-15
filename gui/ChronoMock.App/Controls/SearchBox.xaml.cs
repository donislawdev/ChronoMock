using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Threading;

namespace ChronoMock.App.Controls;

/// <summary>
/// A search field over a list: it clears itself with a glyph in its right end, submits its first hit when
/// Enter is pressed, and takes the caret the moment it is revealed. One control so the three behaviours
/// travel together and the catalogue can show them - none of them is expressible as a style. The drawing is
/// the parts library's TextBox (implicit style) with a clear glyph laid over it, the same shape MomentInput
/// lays a calendar toggle in. It knows nothing about what it is searching: the consumer binds
/// <see cref="Text"/>, <see cref="Placeholder"/> and <see cref="SubmitCommand"/>.
/// </summary>
public partial class SearchBox : UserControl
{
    public SearchBox()
    {
        InitializeComponent();

        // The caret follows the field into view. Wired here rather than in markup because a phase view is
        // not allowed a handler (GUI rule 11), and this is a reusable control, not a screen.
        IsVisibleChanged += OnVisibleChanged;
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
        Text = string.Empty;
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

    // When the section holding the field opens, the person can type at once. Posted at input priority so
    // the focus lands after the reveal has laid the field out - a straight Focus() in this handler is lost
    // while the element is still being made visible. Never on the way out, and never when it is not shown.
    private void OnVisibleChanged(object sender, DependencyPropertyChangedEventArgs e)
    {
        if (e.NewValue is true)
        {
            _ = Dispatcher.BeginInvoke(DispatcherPriority.Input, new Action(() => Field.Focus()));
        }
    }
}
