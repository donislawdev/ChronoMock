using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;

namespace ChronoMock.App.Tests;

/// <summary>
/// Lays a view out at a given size and reports where every element landed.
///
/// This exists because the interface is judged on its RENDERED shape and nothing in the suite could see
/// that shape. The XAML guards read declarations - that a margin comes from the closed scale, that a
/// colour comes from the palette - and a declaration is not a result. Two controls can both take
/// SpaceMd and still end up overlapping, off the panel, or arranged to nothing.
/// </summary>
/// <remarks>
/// 🔴 An unshown Window has no HWND, so what gets laid out is the CONTENT root, by hand. Without the
/// measure and arrange pass the whole tree has zero size, and every rule below would pass over nothing -
/// which is why <see cref="LayoutGuardTests"/> keeps a canary that fails if the walk comes back short.
///
/// What this does NOT see, stated because a blind spot nobody names is the one that gets trusted:
/// anything in a SEPARATE visual tree. Popup, ToolTip, ContextMenu and the drop-down part of a ComboBox
/// all render into their own tree and never appear here.
///
/// What it DOES see, corrected after measuring rather than assumed: elements whose Visibility is
/// Collapsed. They stay in the visual tree and arrange to zero, so the walk reports them with
/// IsVisible false rather than omitting them. That is useful - it is how a rule tells "switched off"
/// apart from "arranged to nothing by mistake" - but a rule that forgets to ask would fire on all of
/// them.
/// </remarks>
internal static class LayoutProbe
{
    /// <summary>The main window's own declared size, so a layout run matches what a user gets.</summary>
    public const int WindowWidth = 1040;

    /// <summary>See <see cref="WindowWidth"/>.</summary>
    public const int WindowHeight = 800;

    /// <summary>
    /// Measure, arrange and update the element so its children have real sizes and positions.
    /// </summary>
    public static void Settle(FrameworkElement root, int width = WindowWidth, int height = WindowHeight)
    {
        root.Measure(new Size(width, height));
        root.Arrange(new Rect(0, 0, width, height));
        root.UpdateLayout();
    }

    /// <summary>
    /// Every framework element below the root, with bounds in the root's coordinates.
    /// </summary>
    public static IReadOnlyList<LaidOutElement> Walk(FrameworkElement root)
    {
        var found = new List<LaidOutElement>();
        var pending = new Stack<Step>();
        pending.Push(new Step(root, 0, false, null, true, -1));

        while (pending.Count > 0)
        {
            var step = pending.Pop();
            var bounds = step.Node is FrameworkElement element ? BoundsWithin(element, root) : null;
            bool visible = step.AncestorsVisible
                && (step.Node is not UIElement self || self.Visibility == Visibility.Visible);

            // 🔴 The root is recorded too, and it was not at first. Skipping it left every direct child
            // with a parent index of -1, so the spacing rule - which needs a parent - silently ignored
            // every gap on the TOP level of the screen, which is where the biggest ones live. Two probes
            // came back with an empty collection before this line changed.
            int index = -1;
            if (step.Node is FrameworkElement described && bounds.HasValue)
            {
                index = found.Count;
                found.Add(Describe(described, bounds.Value, step, visible));
            }

            if (step.Node is Visual)
            {
                PushChildren(pending, step, bounds, visible, index);
            }
        }

        return found;
    }

    /// <summary>
    /// Queue the node's children, working out what scrolls and what clips on the way down. A scrolling
    /// ancestor and a clipping one are deliberately not the same thing: past a scroll edge content is
    /// late, past a clip edge it is nowhere.
    /// </summary>
    private static void PushChildren(
        Stack<Step> pending, Step step, Rect? bounds, bool visible, int index)
    {
        bool scrolls = step.Scrolls || step.Node is ScrollViewer or ScrollContentPresenter;
        var clip = step.HardClip;
        if (step.Node is UIElement { ClipToBounds: true } and not (ScrollViewer or ScrollContentPresenter)
            && bounds.HasValue)
        {
            clip = clip.HasValue ? Rect.Intersect(clip.Value, bounds.Value) : bounds.Value;
        }

        for (int i = VisualTreeHelper.GetChildrenCount(step.Node) - 1; i >= 0; i--)
        {
            pending.Push(new Step(
                VisualTreeHelper.GetChild(step.Node, i), step.Depth + 1, scrolls, clip, visible, index));
        }
    }

    /// <summary>
    /// The element's rectangle in the root's coordinates, or null when it is not connected to the root -
    /// which happens for a template part detached mid-walk, and is a skip rather than a failure.
    /// </summary>
    private static Rect? BoundsWithin(FrameworkElement element, FrameworkElement root)
    {
        if (ReferenceEquals(element, root))
        {
            return new Rect(default, root.RenderSize);
        }

        try
        {
            return element.TransformToAncestor(root).TransformBounds(new Rect(default, element.RenderSize));
        }
        catch (InvalidOperationException)
        {
            return null;
        }
    }

    private static LaidOutElement Describe(
        FrameworkElement element, Rect bounds, Step step, bool visible) => new()
        {
            Kind = element.GetType().Name,
            Name = element.Name ?? string.Empty,
            Bounds = bounds,
            Depth = step.Depth,
            IsVisible = visible,
            InsideScrollable = step.Scrolls,
            HardClip = step.HardClip,
            IsEnabled = element.IsEnabled,
            Text = TextOf(element),
            FontSize = element is TextBlock sized ? sized.FontSize : 0,
            Foreground = element is TextBlock { Foreground: SolidColorBrush ink } ? ink.Color : null,
            ParentIndex = step.ParentIndex,
            NaturalWidth = element is TextBlock measured ? NaturalWidthOf(measured) : 0,
            Wraps = element is TextBlock wrapping && wrapping.TextWrapping != TextWrapping.NoWrap,
        };

    /// <summary>
    /// How wide this text would be on one unconstrained line.
    /// </summary>
    /// <remarks>
    /// Measured on a DETACHED copy rather than by re-measuring the element itself, because measuring a
    /// live element inside a finished layout pass changes the layout the caller is about to read.
    /// </remarks>
    private static double NaturalWidthOf(TextBlock source)
    {
        var copy = new TextBlock
        {
            Text = source.Text,
            FontFamily = source.FontFamily,
            FontSize = source.FontSize,
            FontStyle = source.FontStyle,
            FontWeight = source.FontWeight,
            FontStretch = source.FontStretch,
            TextWrapping = TextWrapping.NoWrap,
        };
        copy.Measure(new Size(double.PositiveInfinity, double.PositiveInfinity));
        return copy.DesiredSize.Width;
    }

    /// <summary>Whatever the element shows as text, which is what a reader would call its content.</summary>
    private static string TextOf(FrameworkElement element) => element switch
    {
        TextBlock block => block.Text ?? string.Empty,
        TextBox box => box.Text ?? string.Empty,
        ContentControl { Content: string content } => content,
        _ => string.Empty,
    };

    /// <summary>One node on the way down, carrying what its ancestors decided about it.</summary>
    /// <remarks>
    /// 🔴 AncestorsVisible is computed here rather than read from UIElement.IsVisible, and that is not a
    /// preference. IsVisible is false for EVERY element in a tree that was laid out by hand, because WPF
    /// reports it against a live PresentationSource and there is no window here. Measured: it returned
    /// false for all 557 elements of a panel whose render plainly showed them. A rule filtering on it
    /// would have run over an empty set and passed, for ever.
    /// </remarks>
    private readonly record struct Step(
        DependencyObject Node, int Depth, bool Scrolls, Rect? HardClip, bool AncestorsVisible,
        int ParentIndex);
}
