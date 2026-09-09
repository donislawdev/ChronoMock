using System.Globalization;
using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using System.Text.RegularExpressions;
using System.Windows;

namespace ChronoMock.App.Tests;

/// <summary>
/// Measures the gaps a layout actually produced between neighbouring elements, and says which of them
/// are not on the closed spacing scale.
/// </summary>
/// <remarks>
/// 🔴 WHY THE GAP AND NOT THE MARGIN. The literal guard already refuses a raw number in a design
/// attribute, so every margin in these views comes from SpaceXs through SpaceXxl. That is a guarantee
/// about the DECLARATION and none at all about the RESULT: two controls can both take SpaceMd and end
/// up seven pixels apart, because a margin adds to a parent's padding, to a row height, to a grid
/// definition and to whatever the control template puts around its content. What the reader sees is the
/// sum, and the sum is what this measures.
///
/// Only gaps inside LAYOUT PANELS are counted. A Border wrapping a ContentPresenter inside a
/// third-party control template arranges its parts to whatever its author chose, and reporting those
/// would bury our own spacing under a dependency's.
/// </remarks>
internal static class SpacingReport
{
    /// <summary>The closed scale from Themes/Values.xaml, plus zero for elements that touch.</summary>
    private static readonly double[] Scale = [0, 4, 8, 12, 16, 24, 32];

    /// <summary>Half a pixel, for arrange rounding.</summary>
    private const double Tolerance = 0.5;

    /// <summary>Panels whose spacing is ours to decide.</summary>
    private static readonly string[] LayoutPanels =
        ["Grid", "StackPanel", "DockPanel", "WrapPanel", "UniformGrid", "VirtualizingStackPanel"];

    /// <summary>One measured gap between two neighbours in the same panel.</summary>
    internal sealed record Gap
    {
        public required string Panel { get; init; }

        /// <summary>
        /// The nearest ancestor with an x:Name, which is the only address a reader can act on.
        /// </summary>
        /// <remarks>
        /// The first version of this report said "in Grid: Grid -> Grid" and that is unlocatable. It also
        /// misled directly: five such gaps in the panel matched its five checkboxes exactly, so the
        /// obvious conclusion was a checkbox template - and the calculator then showed three of them
        /// with no checkbox on the screen at all. A finding nobody can place is a finding that invites a
        /// wrong story.
        /// </remarks>
        public required string Where { get; init; }
        public required string Before { get; init; }
        public required string After { get; init; }
        public required double Size { get; init; }
        public required bool Vertical { get; init; }

        /// <summary>A gap that came from no step of the scale.</summary>
        public bool OffScale => !Scale.Any(step => Math.Abs(step - Size) <= Tolerance);

        /// <summary>
        /// The gap sits under a name declared in OUR views, so it is a spacing decision somebody here
        /// made.
        /// </summary>
        /// <remarks>
        /// 🔴 Without this split the report is unusable as a gate, and the measurement says so plainly:
        /// every one of the eight off-scale gaps across both screens sits under Border 'ContentBorder',
        /// a name that appears nowhere in this repository. It is a part of a wpfui control template,
        /// spaced 18 px by its author. A rule reddening on that asks us to fix a dependency.
        /// </remarks>
        public required bool Ours { get; init; }
    }

    /// <summary>Every gap between visible neighbours inside a layout panel.</summary>
    public static IReadOnlyList<Gap> Measure(IReadOnlyList<LaidOutElement> elements)
    {
        var gaps = new List<Gap>();
        var byParent = new Dictionary<int, List<LaidOutElement>>();

        for (int i = 0; i < elements.Count; i++)
        {
            var element = elements[i];
            if (!element.IsVisible || element.ParentIndex < 0 || IsEmpty(element.Bounds))
            {
                continue;
            }

            if (!LayoutPanels.Contains(elements[element.ParentIndex].Kind))
            {
                continue;
            }

            if (!byParent.TryGetValue(element.ParentIndex, out var siblings))
            {
                siblings = [];
                byParent[element.ParentIndex] = siblings;
            }

            siblings.Add(element);
        }

        foreach (var (parentIndex, siblings) in byParent)
        {
            if (siblings.Count < 2)
            {
                continue;
            }

            string panel = elements[parentIndex].Label;
            var (where, ours) = NearestNamed(elements, parentIndex);
            gaps.AddRange(Along(panel, where, ours, siblings, vertical: true));
            gaps.AddRange(Along(panel, where, ours, siblings, vertical: false));
        }

        return gaps;
    }

    /// <summary>The closest ancestor carrying an x:Name, walking up the recorded parent chain.</summary>
    private static (string Where, bool Ours) NearestNamed(IReadOnlyList<LaidOutElement> elements, int index)
    {
        int steps = 0;
        while (index >= 0 && steps++ < MaxAncestorWalk)
        {
            var element = elements[index];
            if (element.Name.Length > 0)
            {
                return (element.Kind + " '" + element.Name + "'", OurNames.Contains(element.Name));
            }

            index = element.ParentIndex;
        }

        return ("(unnamed)", false);
    }

    /// <summary>
    /// 🔴 Declared BEFORE the set that uses it. Static initialisers run in TEXTUAL order, so with this
    /// line below the set the regex was still null when ReadOurNames ran, and the whole type threw a
    /// TypeInitializationException wrapping a NullReferenceException - a failure that names neither the
    /// field nor the ordering.
    /// </summary>
    private static readonly Regex NameAttribute = new("x:Name=\"([^\"]+)\"", RegexOptions.Compiled);

    /// <summary>
    /// Every x:Name declared in our own views, read from the source XAML.
    /// </summary>
    /// <remarks>
    /// The same method the other XAML guards use: read the source rather than the compiled BAML, so the
    /// list is what a person wrote. A name absent here belongs to a control template we did not write.
    /// </remarks>
    private static readonly HashSet<string> OurNames = ReadOurNames();

    private static HashSet<string> ReadOurNames()
    {
        var names = new HashSet<string>(StringComparer.Ordinal);
        var directory = TestPaths.AppDirectory();
        if (!Directory.Exists(directory))
        {
            return names;
        }

        foreach (var file in Directory.EnumerateFiles(directory, "*.xaml", SearchOption.AllDirectories))
        {
            if (file.Contains(Path.DirectorySeparatorChar + "obj" + Path.DirectorySeparatorChar, StringComparison.Ordinal)
                || file.Contains(Path.DirectorySeparatorChar + "bin" + Path.DirectorySeparatorChar, StringComparison.Ordinal))
            {
                continue;
            }

            foreach (Match match in NameAttribute.Matches(File.ReadAllText(file)))
            {
                names.Add(match.Groups[1].Value);
            }
        }

        return names;
    }


    /// <summary>A ceiling on the walk up, so a cycle in a malformed chain cannot hang the report.</summary>
    private const int MaxAncestorWalk = 64;

    /// <summary>
    /// Gaps along one axis, between neighbours that actually line up on the other one.
    /// </summary>
    /// <remarks>
    /// The overlap test is what keeps this honest. Two controls in different columns of a grid are not
    /// vertical neighbours however their tops compare, and measuring the distance between them would
    /// invent a gap nobody laid out.
    /// </remarks>
    private static IEnumerable<Gap> Along(
        string panel, string where, bool ours, List<LaidOutElement> siblings, bool vertical)
    {
        var ordered = siblings
            .OrderBy(e => vertical ? e.Bounds.Top : e.Bounds.Left)
            .ToList();

        for (int i = 1; i < ordered.Count; i++)
        {
            var before = ordered[i - 1];
            var after = ordered[i];
            if (!SharesTheOtherAxis(before.Bounds, after.Bounds, vertical))
            {
                continue;
            }

            double gap = vertical
                ? after.Bounds.Top - before.Bounds.Bottom
                : after.Bounds.Left - before.Bounds.Right;

            // A negative gap is an overlap, which is the spill rule's business rather than spacing's.
            if (gap < 0)
            {
                continue;
            }

            yield return new Gap
            {
                Panel = panel,
                Where = where,
                Ours = ours,
                Before = before.Label,
                After = after.Label,
                Size = Math.Round(gap, 1),
                Vertical = vertical,
            };
        }
    }

    private static bool SharesTheOtherAxis(Rect a, Rect b, bool vertical) => vertical
        ? a.Left < b.Right && b.Left < a.Right
        : a.Top < b.Bottom && b.Top < a.Bottom;

    private static bool IsEmpty(Rect bounds) => bounds.Width <= Tolerance || bounds.Height <= Tolerance;

    /// <summary>The gaps as text: the distribution first, then the ones off the scale.</summary>
    public static string Describe(IReadOnlyList<Gap> gaps)
    {
        var distribution = gaps
            .GroupBy(g => g.Size)
            .OrderBy(g => g.Key)
            .Select(g => string.Create(
                CultureInfo.InvariantCulture,
                $"  {g.Key,6:N1} px  x{g.Count(),-4} {(Scale.Any(s => Math.Abs(s - g.Key) <= Tolerance) ? "" : "OFF SCALE")}"));

        var off = gaps.Where(g => g.OffScale)
            .OrderByDescending(g => g.Size)
            .Select(g => string.Create(
                CultureInfo.InvariantCulture,
                $"  {g.Size,6:N1} px  {(g.Vertical ? "vertical" : "horizontal")}  under {g.Where}"
                + $"  ({g.Panel}: {g.Before} -> {g.After})"));

        return $"{gaps.Count} gaps measured, {gaps.Count(g => g.OffScale)} off the scale, "
            + $"{gaps.Count(g => g.OffScale && g.Ours)} of them OURS\n\nDISTRIBUTION\n"
            + string.Join('\n', distribution)
            + "\n\nOFF THE SCALE\n"
            + string.Join('\n', off);
    }
}
