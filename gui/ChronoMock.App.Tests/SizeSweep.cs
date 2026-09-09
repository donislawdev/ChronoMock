using System.Globalization;
using System.Windows;

namespace ChronoMock.App.Tests;

/// <summary>
/// Lays a screen out at a range of window sizes and runs every layout rule at each one.
/// </summary>
/// <remarks>
/// 🔴 WHY A RANGE AND NOT A SIZE. A layout is correct at the size it was designed at, because that is
/// the size it was looked at. It gives way somewhere else - at the floor the window allows, at a width
/// where a row stops fitting on one line, at a height where a panel starts scrolling. Finding that by
/// hand means dragging a live window and watching, once, for one screen, and never again.
///
/// The sizes are not invented. The floor is MainWindow's own MinWidth and MinHeight, the middle is its
/// declared Width and Height, and the rest are the shapes a window actually gets dragged into: wider
/// than tall, and tall and narrow.
/// </remarks>
internal static class SizeSweep
{
    /// <summary>
    /// One sweep step: the surface, and what it is there to catch.
    /// </summary>
    internal sealed record Step(string Name, int Width, int Height);

    /// <summary>
    /// The sizes swept. MainWindow declares Width 1040, Height 800, MinWidth 1000, MinHeight 360.
    /// </summary>
    public static readonly IReadOnlyList<Step> Sizes =
    [
        new("floor", 1000, 360),
        new("default", 1040, 800),
        new("wide", 1440, 800),
        new("widest", 1920, 1000),
        new("tall-narrow", 1000, 1200),
    ];

    /// <summary>
    /// Every complaint every rule has about this screen at this size.
    /// </summary>
    public static IReadOnlyList<string> Inspect(FrameworkElement root, Step size)
    {
        LayoutProbe.Settle(root, size.Width, size.Height);
        var elements = LayoutProbe.Walk(root);
        var surface = new Size(size.Width, size.Height);

        return
        [
            .. LayoutRules.OutsideTheSurface(elements, surface),
            .. LayoutRules.ArrangedToNothing(elements),
            .. LayoutRules.PastAHardClip(elements),
            .. LayoutRules.TrimmedAway(elements),
            .. LayoutRules.SpillsOutOfItsParent(elements),
        ];
    }

    /// <summary>A line naming the size, for a report or a failure message.</summary>
    public static string Describe(Step size) => string.Create(
        CultureInfo.InvariantCulture, $"{size.Name} ({size.Width}x{size.Height})");
}
