using System.Globalization;
using System.Windows;

namespace ChronoMock.App.Tests;

/// <summary>
/// The layout facts that are objectively wrong rather than merely ugly, stated over a finished layout
/// pass.
/// </summary>
/// <remarks>
/// 🔴 THE LINE THESE RULES DO NOT CROSS. They do not judge how a screen looks. Alignment, rhythm,
/// density and balance are read from the rendered sheet by a person, because a rule that scored them
/// would be an opinion wearing the costume of a measurement, and it would go on being green over an
/// interface nobody would ship. What lives here is the small set of things that are wrong under any
/// taste: content the user cannot reach, and content that is present to automation while being invisible
/// to the eye.
///
/// Every threshold below was measured on the real startup renders before it was written down. Each rule
/// reports nothing on the current panel and calculator, and has a probe in
/// <see cref="LayoutGuardTests"/> that builds a broken tree and watches the rule fire. A rule that has
/// never been seen red is a decoration.
/// </remarks>
internal static class LayoutRules
{
    /// <summary>Half a pixel, so a rounding difference in an arrange pass is not a finding.</summary>
    private const double Tolerance = 0.5;

    /// <summary>
    /// Visible content placed outside the surface, where no ancestor scrolls to it.
    /// </summary>
    /// <remarks>
    /// The scrolling exemption is what makes this usable rather than deafening. Measured on the startup
    /// panel: 27 visible elements reach past the bottom edge and every one of them sits inside the
    /// panel's scroll viewer, so they are content below the fold rather than content nobody can have.
    /// </remarks>
    public static IReadOnlyList<string> OutsideTheSurface(IReadOnlyList<LaidOutElement> elements, Size surface)
    {
        var surfaceRect = new Rect(0, 0, surface.Width, surface.Height);
        return elements
            .Where(e => e.IsVisible && !e.InsideScrollable && !IsEmpty(e.Bounds))
            .Where(e => !Contains(surfaceRect, e.Bounds))
            .Select(e => $"{LayoutReport.Locate(e)} lies outside the {Describe(surface)} surface")
            .ToList();
    }

    /// <summary>
    /// Visible elements that carry text and were arranged to nothing, so the words exist for assistive
    /// tech and for a test, and not for the reader.
    /// </summary>
    /// <remarks>
    /// The visibility filter is the whole rule. A Collapsed element stays in the visual tree and
    /// arranges to zero by design - 217 of the panel's 557 elements are in that state at startup - so
    /// without the filter this would report the interface working correctly.
    /// </remarks>
    public static IReadOnlyList<string> ArrangedToNothing(IReadOnlyList<LaidOutElement> elements) => elements
        .Where(e => e.IsVisible && e.Text.Length > 0 && IsEmpty(e.Bounds))
        .Select(e => $"{LayoutReport.Locate(e)} shows text but was arranged to nothing")
        .ToList();

    /// <summary>
    /// Visible elements pushed entirely past a clip that offers no way to scroll to them, which is the
    /// difference between content that is late and content that is nowhere.
    /// </summary>
    public static IReadOnlyList<string> PastAHardClip(IReadOnlyList<LaidOutElement> elements) => elements
        .Where(e => e.IsVisible && !IsEmpty(e.Bounds) && e.HardClip.HasValue)
        .Where(e => Rect.Intersect(e.HardClip!.Value, e.Bounds).IsEmpty)
        .Select(e => $"{LayoutReport.Locate(e)} is clipped away entirely by an ancestor")
        .ToList();

    /// <summary>
    /// Text that does not fit the box it was given and is not allowed to wrap, so the reader gets an
    /// ellipsis or a hard edge instead of the end of the sentence.
    /// </summary>
    /// <remarks>
    /// 🔴 This is the rule that exists because of the SECOND product rule rather than the first. A
    /// trimmed label is correct as far as the framework is concerned - it asked for an ellipsis and got
    /// one - and it is still an interface quietly withholding information from someone deciding whether
    /// a test passed. Nothing else in the suite can see it: the text is present in the tree, present to
    /// automation, and present in a binding assertion.
    /// </remarks>
    public static IReadOnlyList<string> TrimmedAway(IReadOnlyList<LaidOutElement> elements) => elements
        .Where(e => e.IsVisible && !e.Wraps && !IsEmpty(e.Bounds) && e.NaturalWidth > 0)
        .Where(e => e.NaturalWidth > e.Bounds.Width + TrimTolerance)
        .Select(e => $"{LayoutReport.Locate(e)} needs {e.NaturalWidth:F0}px for its text and got "
            + $"{e.Bounds.Width:F0}px, so the end of it is not readable")
        .ToList();

    /// <summary>
    /// A pixel of slack. Text layout rounds, and a label one rounding step short of its own width is not
    /// a defect anybody can see.
    /// </summary>
    private const double TrimTolerance = 1.0;

    /// <summary>
    /// Content that spills out of the box it was put in.
    /// </summary>
    /// <remarks>
    /// 🔴 This rule exists because a measurement contradicted what the truncation rule assumed. A
    /// TextBlock inside a Grid explicitly 60 wide came back 285 wide - WPF did not squeeze it, it let it
    /// OVERFLOW and go on painting over whatever was beside it. So in this framework the common defect
    /// is not a cut label, it is a label sitting on top of its neighbour, and the two need separate
    /// rules. Text only gets trimmed when something really constrains it, such as a fixed grid column.
    ///
    /// Content under a scrolling ancestor is exempt for the usual reason: reaching past the viewport is
    /// what scrolling is for.
    /// </remarks>
    public static IReadOnlyList<string> SpillsOutOfItsParent(IReadOnlyList<LaidOutElement> elements)
    {
        var complaints = new List<string>();
        for (int i = 0; i < elements.Count; i++)
        {
            var child = elements[i];
            if (!child.IsVisible || child.InsideScrollable || IsEmpty(child.Bounds) || child.ParentIndex < 0)
            {
                continue;
            }

            var parent = elements[child.ParentIndex];
            if (!parent.IsVisible || IsEmpty(parent.Bounds) || Contains(parent.Bounds, child.Bounds))
            {
                continue;
            }

            complaints.Add($"{LayoutReport.Locate(child)} spills out of {parent.Label} "
                + $"at [{parent.Bounds.X:F0},{parent.Bounds.Y:F0} {parent.Bounds.Width:F0}x{parent.Bounds.Height:F0}]");
        }

        return complaints;
    }

    private static bool IsEmpty(Rect bounds) => bounds.Width <= Tolerance || bounds.Height <= Tolerance;

    private static bool Contains(Rect outer, Rect inner) =>
        inner.Left >= outer.Left - Tolerance
        && inner.Top >= outer.Top - Tolerance
        && inner.Right <= outer.Right + Tolerance
        && inner.Bottom <= outer.Bottom + Tolerance;

    private static string Describe(Size surface) => string.Create(
        CultureInfo.InvariantCulture, $"{(int)surface.Width}x{(int)surface.Height}");
}
