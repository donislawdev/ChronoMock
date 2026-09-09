using System.ComponentModel;
using System.Globalization;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;

namespace ChronoMock.App.Tests;

/// <summary>
/// Says where a rendered value came from: our view, a style, the control template, or nothing at all.
/// </summary>
/// <remarks>
/// 🔴 WHY THIS QUESTION IS THE EXPENSIVE ONE HERE. This interface sits on a third-party control
/// library, and the values a reader sees are the sum of four layers: the wpfui theme dictionary, our
/// palette, the control's own default style, and whatever the view writes. When a colour or a size
/// comes out wrong, the costly part is never fixing it - it is finding out WHICH of the four put it
/// there.
///
/// Measured cases from this project, both of which took a session to explain: a Background written
/// on a ui:FluentWindow is accepted and paints nothing, and the controls are semi transparent, so
/// their painted colour appears in no declaration anywhere. Neither is visible by reading XAML.
///
/// WHAT THE ANSWER MEANS. BaseValueSource is WPF's own account of the winning value:
///   Local          - the view wrote it. Ours, and the first place to look.
///   Style          - a style we or the library applied.
///   DefaultStyle   - the control's own template. Not ours, and changing the view will not move it.
///   Inherited      - it came down from an ancestor, so the ancestor is the place to change.
///   Default        - nobody set it. If the value looks deliberate, it is a coincidence.
///
/// The idea of asking GetValueSource at all is TAKEN from the owner's neighbouring project
/// (tools/gui-probe/why.ps1 in BetterWindowsServices), per zasady/16 section 1: a method transfers,
/// a decision does not.
/// </remarks>
internal static class ValueSourceReport
{
    /// <summary>The properties worth asking about: everything a reader can see.</summary>
    private static readonly (string Name, DependencyProperty Property)[] Watched =
    [
        ("Background", Control.BackgroundProperty),
        ("Foreground", Control.ForegroundProperty),
        ("BorderBrush", Control.BorderBrushProperty),
        ("BorderThickness", Control.BorderThicknessProperty),
        ("FontSize", Control.FontSizeProperty),
        ("FontFamily", Control.FontFamilyProperty),
        ("Margin", FrameworkElement.MarginProperty),
        ("Padding", Control.PaddingProperty),
        ("Opacity", UIElement.OpacityProperty),
    ];

    /// <summary>One property, its value, and who won it.</summary>
    internal sealed record Answer
    {
        public required string Element { get; init; }
        public required string Property { get; init; }
        public required string Value { get; init; }
        public required string Source { get; init; }

        /// <summary>The value came from somewhere we control, rather than from the library.</summary>
        public bool Ours => Source is "Local" or "Style" or "Inherited";
    }

    /// <summary>
    /// Ask every watched property of every element whose name matches, on a laid-out tree.
    /// </summary>
    public static IReadOnlyList<Answer> Ask(FrameworkElement root, string namePattern)
    {
        LayoutProbe.Settle(root);
        var answers = new List<Answer>();

        foreach (var element in Descendants(root))
        {
            if (element.Name.Length == 0 || !element.Name.Contains(namePattern, StringComparison.OrdinalIgnoreCase))
            {
                continue;
            }

            foreach (var (name, property) in Watched)
            {
                // 🔴 Applicability is asked of the PROPERTY SYSTEM, not of OwnerType. The first version
                // compared OwnerType against the element's type and silently dropped Foreground and
                // FontSize on every control - both are registered by TextElement and inherited down,
                // so the owner check answered "not applicable" for exactly the two properties this
                // report exists to explain. Measured: TargetBox came back with three answers instead
                // of six, and the missing three were the interesting ones.
                if (DependencyPropertyDescriptor.FromProperty(property, element.GetType()) is null)
                {
                    continue;
                }

                var source = DependencyPropertyHelper.GetValueSource(element, property);
                answers.Add(new Answer
                {
                    Element = $"{element.GetType().Name} '{element.Name}'",
                    Property = name,
                    Value = Describe(element.GetValue(property)),
                    Source = source.BaseValueSource.ToString(),
                });
            }
        }

        return answers;
    }

    private static IEnumerable<FrameworkElement> Descendants(DependencyObject root)
    {
        var pending = new Stack<DependencyObject>();
        pending.Push(root);
        while (pending.Count > 0)
        {
            var node = pending.Pop();
            if (node is FrameworkElement element)
            {
                yield return element;
            }

            if (node is not Visual)
            {
                continue;
            }

            for (int i = VisualTreeHelper.GetChildrenCount(node) - 1; i >= 0; i--)
            {
                pending.Push(VisualTreeHelper.GetChild(node, i));
            }
        }
    }

    private static string Describe(object? value) => value switch
    {
        null => "(null)",
        SolidColorBrush brush => $"#{brush.Color.R:X2}{brush.Color.G:X2}{brush.Color.B:X2}",
        Brush other => other.GetType().Name,
        double number => number.ToString("0.##", CultureInfo.InvariantCulture),
        _ => value.ToString() ?? "(null)",
    };

    /// <summary>The answers as text, grouped by element, with the library's own values marked.</summary>
    public static string Describe(IReadOnlyList<Answer> answers)
    {
        var lines = new List<string>();
        foreach (var group in answers.GroupBy(a => a.Element))
        {
            lines.Add(group.Key);
            foreach (var answer in group)
            {
                string mark = answer.Ours ? " " : "*";
                lines.Add(string.Create(
                    CultureInfo.InvariantCulture,
                    $"  {mark} {answer.Property,-16} {answer.Value,-24} <- {answer.Source}"));
            }
        }

        lines.Add(string.Empty);
        lines.Add("* = set outside our code (DefaultStyle or Default) - editing the view will not move it");
        return string.Join('\n', lines);
    }
}
