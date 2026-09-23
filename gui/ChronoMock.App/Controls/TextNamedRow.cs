using System.Globalization;
using System.Text;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Automation.Peers;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Documents;
using System.Windows.Media;

namespace ChronoMock.App.Controls;

/// <summary>
/// The container of one row in a <see cref="TextNamedList"/> or <see cref="TextNamedItems"/>: an ordinary
/// ContentPresenter whose accessible name is the text the row shows.
/// </summary>
/// <remarks>
/// 🔴 WHY A CONTAINER AND NOT A NAME IN A STYLE. WPF names a list row from its container, and a plain
/// ContentPresenter answers with its content's ToString(). Our rows hold translation KEYS and records, so
/// a screen reader on a warning read "runtime.dotnet_stopwatch_qpc" and on an audit row read
/// "AuditRow { Channel = GetTickCount64, ... }" - measured on every list of the audit, 87 rows, while the
/// screen showed sentences and cells. A name set in an ItemContainerStyle would have to restate what each
/// row template shows, triggers and runs included, and a second statement of the same thing is one that
/// drifts. This reads the template's own result instead, when assistive tech asks, so the name cannot
/// disagree with the row and follows a count that changes while a session runs.
///
/// A name set explicitly with AutomationProperties.Name on the container still wins.
/// </remarks>
public sealed class TextNamedRow : ContentPresenter
{
    protected override AutomationPeer OnCreateAutomationPeer() => new TextNamedRowAutomationPeer(this);

    /// <summary>What joins the row's pieces of text, the way its cells read aloud one after another.</summary>
    private const string Separator = ", ";

    /// <summary>The text a reader sees in the row, in reading order, joined.</summary>
    /// <remarks>
    /// Inputs give what they hold, not what they are made of: a text box its text (never its placeholder,
    /// which is a hint and not the row's content), a drop-down the choice it shows (its list lives in a
    /// popup, outside this tree). A calculator step is inputs from end to end, and read this way it says
    /// which step it is - the first version skipped inputs and named every step after its remove button.
    ///
    /// Left out, each for a reason: anything not visible (a collapsed cell says nothing on screen), a
    /// password, any list nested in the row - a calculator reading holds its significance notes in one,
    /// whose rows are named on their own, and reading them into the row too would say them twice - and
    /// icon glyphs from the private-use area, which a symbol font draws and a screen reader would spell
    /// as nothing useful. A drop-down is a list as well, and is matched first, for the choice it shows.
    ///
    /// A button is left out too, while the row has anything else to say: a format row reads "ISO date,
    /// 2026-09-23" and its Copy button is announced as the button it is, rather than the row ending in
    /// ", Copy" (measured live, the first version did). A row that IS a button - a speed, a jump - has
    /// nothing else, so it reads as its button, by the button's own accessible name when it has one.
    /// </remarks>
    private static string ShownText(DependencyObject root)
    {
        var parts = new List<string>();
        Collect(root, parts, withButtons: false);
        if (parts.Count == 0)
        {
            Collect(root, parts, withButtons: true);
        }

        return string.Join(Separator, parts);
    }

    private static void Collect(DependencyObject node, List<string> parts, bool withButtons)
    {
        if (node is UIElement { Visibility: not Visibility.Visible })
        {
            return;
        }

        switch (node)
        {
            case TextBlock block:
                Add(parts, TextOf(block));
                return;
            case TextBox box:
                Add(parts, box.Text);
                return;
            case ComboBox combo:
                // The choice on show, which the combo's own template draws. Its toggle is a button, so the
                // walk inside it takes buttons in, and a chevron glyph drops out as private use.
                Descend(combo, parts, withButtons: true);
                return;
            case TextBoxBase or PasswordBox or ItemsControl:
            case ButtonBase when !withButtons:
                return;
            case ButtonBase button when AutomationProperties.GetName(button) is { Length: > 0 } label:
                Add(parts, label);
                return;
        }

        Descend(node, parts, withButtons);
    }

    private static void Descend(DependencyObject node, List<string> parts, bool withButtons)
    {
        for (int i = 0; i < VisualTreeHelper.GetChildrenCount(node); i++)
        {
            Collect(VisualTreeHelper.GetChild(node, i), parts, withButtons);
        }
    }

    private static void Add(List<string> parts, string? text)
    {
        if (Readable(text ?? string.Empty) is { Length: > 0 } readable)
        {
            parts.Add(readable);
        }
    }

    /// <summary>
    /// A TextBlock's text, from its Text property or from the runs inside it. A block composed of runs keeps
    /// Text EMPTY - measured by the layout probe in the tests - so reading only the property would drop
    /// every cell built from a label and a bound value.
    /// </summary>
    /// <remarks>
    /// 🔴 THE RUNS ARE READ AS LOGICAL CHILDREN, NOT THROUGH Inlines. The Inlines getter converts a block of
    /// plain text into one of complex content, so the first version CHANGED a cell every time a screen
    /// reader asked for a row's name - and measured live, a row whose count cell was empty named itself ""
    /// on the first query and correctly on the second. Reading a name must not touch what it reads, and the
    /// logical children are the runs of a composed block and nothing at all for a plain one.
    /// </remarks>
    private static string TextOf(TextBlock block)
        => string.IsNullOrEmpty(block.Text)
            ? string.Concat(LogicalTreeHelper.GetChildren(block).OfType<Inline>().Select(TextOf))
            : block.Text;

    private static string TextOf(Inline inline) => inline switch
    {
        Run run => run.Text ?? string.Empty,
        Span span => string.Concat(span.Inlines.Select(TextOf)),
        LineBreak => " ",
        _ => string.Empty,
    };

    /// <summary>The text without private-use glyphs, trimmed.</summary>
    /// <remarks>
    /// Asked by category rather than by a range written into this file: the first version wrote the range
    /// as two escaped character literals, and the tool that wrote the file turned both escapes into the
    /// invisible characters themselves - correct, and unreadable to anybody after.
    /// </remarks>
    private static string Readable(string text)
    {
        var kept = new StringBuilder(text.Length);
        foreach (char c in text)
        {
            if (CharUnicodeInfo.GetUnicodeCategory(c) != UnicodeCategory.PrivateUse)
            {
                kept.Append(c);
            }
        }

        return kept.ToString().Trim();
    }

    /// <summary>The peer that names the row by what it shows.</summary>
    private sealed class TextNamedRowAutomationPeer(TextNamedRow owner) : FrameworkElementAutomationPeer(owner)
    {
        protected override string GetNameCore()
        {
            string set = AutomationProperties.GetName(Owner);
            return string.IsNullOrEmpty(set) ? ShownText(Owner) : set;
        }
    }
}
