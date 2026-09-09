using System.Windows;
using System.Windows.Media;

namespace ChronoMock.App.Tests;

/// <summary>
/// One element as the layout pass actually placed it, with its rectangle expressed in the root's own
/// coordinates so two elements can be compared without walking back up the tree.
/// </summary>
/// <remarks>
/// Three of these fields exist because a rule written without them fires on a correct interface.
///
/// <see cref="IsVisible"/>: a Collapsed element STAYS in the visual tree and arranges to nothing, so
/// "zero size" is how this app encodes "this state is off". Measured on the startup panel: 94 elements
/// arranged to zero, 11 of them carrying text, every one of them a state that was simply not showing.
/// A zero-size rule without this flag would have reported all eleven.
///
/// <see cref="InsideScrollable"/>: content below the fold is the point of a scrolling panel. Measured on
/// the same render: 173 elements reach past the bottom edge, all of them inside the panel's scroll
/// viewer, none of them a defect.
///
/// <see cref="HardClip"/>: the region an ancestor clips to WITHOUT offering a way to scroll there. Past
/// that edge content is not late, it is unreachable.
/// </remarks>
internal sealed record LaidOutElement
{
    /// <summary>Runtime type name, for example TextBlock or ScrollViewer.</summary>
    public required string Kind { get; init; }

    /// <summary>The x:Name from the view, empty when the element is an unnamed template part.</summary>
    public required string Name { get; init; }

    /// <summary>Bounds in the ROOT's coordinates, not the parent's.</summary>
    public required Rect Bounds { get; init; }

    /// <summary>Depth below the root, zero for the root itself.</summary>
    public required int Depth { get; init; }

    /// <summary>Rendered for the user, as opposed to present in the tree with its state switched off.</summary>
    public required bool IsVisible { get; init; }

    /// <summary>Some ancestor scrolls, so bounds reaching past the root are content below the fold.</summary>
    public required bool InsideScrollable { get; init; }

    /// <summary>The nearest non-scrolling clip region, or null when nothing clips this element.</summary>
    public required Rect? HardClip { get; init; }

    /// <summary>Enabled for input, which is what decides whether the user can act on it.</summary>
    public required bool IsEnabled { get; init; }

    /// <summary>Whatever text this element shows, empty when it shows none.</summary>
    public required string Text { get; init; }

    /// <summary>Type size for a text element, zero for everything else.</summary>
    public required double FontSize { get; init; }

    /// <summary>Ink colour for a text element, null when the brush is not a plain colour.</summary>
    public required Color? Foreground { get; init; }

    /// <summary>Index of this element's parent in the same walk, or -1 for a child of the root.</summary>
    public required int ParentIndex { get; init; }

    /// <summary>
    /// How wide this text WANTS to be on one line, measured on a detached copy, or zero when the element
    /// is not text.
    /// </summary>
    /// <remarks>
    /// The point of comparison for truncation. A trimmed label is CORRECT as far as WPF is concerned -
    /// it asked for an ellipsis and got one - while the reader simply loses the end of the sentence.
    /// </remarks>
    public required double NaturalWidth { get; init; }

    /// <summary>Text that is allowed to wrap, for which a natural width wider than the box is normal.</summary>
    public required bool Wraps { get; init; }

    /// <summary>A label for messages: the name when it has one, the type when it does not.</summary>
    public string Label => Name.Length > 0 ? $"{Kind} '{Name}'" : Kind;
}
