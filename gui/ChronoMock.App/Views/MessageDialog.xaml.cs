using System.Windows;

namespace ChronoMock.App.Views;

/// <summary>
/// The app's dialog for the questions and notices of normal operation, in the app's own theme and the
/// app's own language (rule 15). It has two shapes and one window: a QUESTION, which offers a named
/// affirmative beside a cancel, and a NOTICE, which has one way out and nothing to answer.
///
/// Both are opened through the static helpers rather than by constructing this type, so a caller cannot
/// forget the owner (a dialog with no owner opens wherever Windows likes and does not stay above the app)
/// and cannot half-configure the shape. The title bar carries the app's name and the heading carries what
/// the dialog is about, because a FluentWindow's title is small and grey and would hide the one line the
/// reader has to see.
/// </summary>
public partial class MessageDialog
{
    private bool _affirmed;

    private MessageDialog(Window owner, string heading, string message, string affirmative, bool isQuestion)
    {
        InitializeComponent();
        Owner = owner;
        Title = Text("app.title");
        Heading.Text = heading;
        Message.Text = message;
        Message.Visibility = message.Length > 0 ? Visibility.Visible : Visibility.Collapsed;

        // A notice has nothing to affirm, so the affirmative goes and the remaining button stops being a
        // cancel and becomes the way out. Leaving a hidden button behind a renamed one is how a dialog
        // ends up with an Escape key meaning something other than the button beside it.
        AffirmativeButton.Content = affirmative;
        AffirmativeButton.Visibility = isQuestion ? Visibility.Visible : Visibility.Collapsed;
        CancelButton.Content = Text(isQuestion ? "dialog.cancel" : "dialog.close");

        // Focus starts on the answer that changes nothing, so a stray Enter cannot confirm a destructive
        // action the reader has not got to yet.
        CancelButton.Loaded += (_, _) => CancelButton.Focus();
    }

    /// <summary>
    /// Ask a question with a named affirmative, and report whether it was chosen. Anything else - cancel,
    /// Escape, or closing the window - is a no, which is what makes this safe in front of a destructive
    /// action (rule 7).
    /// </summary>
    public static bool Ask(Window owner, string heading, string message, string affirmative)
    {
        var dialog = new MessageDialog(owner, heading, message, affirmative, isQuestion: true);
        dialog.ShowDialog();
        return dialog._affirmed;
    }

    /// <summary>Say something that needs no answer, with a single way out.</summary>
    public static void Tell(Window owner, string heading, string message)
        => new MessageDialog(owner, heading, message, string.Empty, isQuestion: false).ShowDialog();

    /// <summary>A translation key resolved to text, falling back to the raw key (rule 15).</summary>
    private static string Text(string key) => Application.Current?.TryFindResource(key) as string ?? key;

    private void OnAffirmativeClick(object sender, RoutedEventArgs e)
    {
        _affirmed = true;
        Close();
    }

    private void OnCancelClick(object sender, RoutedEventArgs e) => Close();
}
