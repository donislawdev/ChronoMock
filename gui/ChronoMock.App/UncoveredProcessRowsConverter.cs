using System.Globalization;
using System.Windows.Data;
using ChronoMock.Protocol;

namespace ChronoMock.App;

/// <summary>
/// One row of the process table: an executable this session's family spawned without the hook inside it,
/// the role it ran in, and how many such processes there were. Built by
/// <see cref="UncoveredProcessRowsConverter"/> from the list the session verdict names, so the view model
/// keeps the list and the screen gets one table.
/// </summary>
/// <param name="Image">The executable's file name in the spelling it was first reported with, or empty
/// for the processes that were gone before they could be asked (<paramref name="IsUnnamed"/>).</param>
/// <param name="IsUnnamed">True for the row that gathers the children the core could not name - the
/// template prints the sentence for that instead of a name, in the secondary ink.</param>
/// <param name="Role">The <c>--type=</c> role a Chromium-based engine gives a subprocess, verbatim, or
/// empty for a process without one.</param>
/// <param name="IsRenderer">True when the role is the one web pages run in - the row that means the
/// application's pages read the real clock, so it takes the failure ink.</param>
/// <param name="PagesReached">True for a renderer whose engine the session reached through its debugging
/// port, so the pages it hosts ran on the session clock even though the process itself did not. The row
/// then reads as partial rather than as a failure - which is what the family verdict says too.</param>
/// <param name="Count">How many processes this row stands for.</param>
public sealed record UncoveredProcessRow(
    string Image, bool IsUnnamed, string Role, bool IsRenderer, bool PagesReached, int Count)
{
    /// <summary>The count as the table prints it, in invariant digits like every count on this interface.</summary>
    public string CountText => Count.ToString(CultureInfo.InvariantCulture);
}

/// <summary>
/// Folds the session's uncovered children into the rows of one table: one row per executable and role,
/// counted. Bound as a value converter over <c>UncoveredChildren</c>.
/// </summary>
/// <remarks>
/// A converter rather than a view model property, for the reason <see cref="AuditRowsConverter"/> gives:
/// the rows are a presentation of a list the view model already exposes, and that class stands on its
/// coupling ceiling (gui/CodeMetricsConfig.txt). A value that is not a list (a view rendered without a data
/// context hands over UnsetValue) counts as an empty list, so the table renders empty rather than throwing
/// out of a binding.
/// </remarks>
public sealed class UncoveredProcessRowsConverter : IMultiValueConverter
{
    /// <summary>The role token web pages run in, as the core names it (crates/cli/src/embedded.rs).</summary>
    internal const string RendererRole = "renderer";

    /// <summary>
    /// Two lists in, one table out: the processes the hook never entered, and the engines the session
    /// reached. It takes both because a renderer row cannot be read without the second - the same row
    /// means "this application's pages ran on the real clock" or "its pages were reached anyway",
    /// depending on whether the session got into the engine that spawned it.
    /// </summary>
    public object Convert(object[] values, Type targetType, object? parameter, CultureInfo culture)
        => Fold(At<UncoveredChild>(values, 0), At<ReachedEngine>(values, 1));

    /// <summary>One binding of a multi-binding as a list, or empty - a view rendered without a data
    /// context hands over UnsetValue, and a shorter array than expected is the same case.</summary>
    private static IEnumerable<T> At<T>(object[] values, int index)
        => values is not null && index < values.Length && values[index] is IEnumerable<T> list ? list : [];

    /// <summary>
    /// The fold itself, testable without a binding. Rows of one executable stay together, and the
    /// executables are ordered by what they tell the reader: the one that hosted a renderer first (its
    /// pages read the real clock), then by how many processes it stands for, then by name - and the
    /// processes that were gone before they could be named come last, as one row. Inside an executable
    /// the renderer row leads, then the roles by count and name.
    /// </summary>
    /// <remarks>
    /// Executables are matched without regard to case, because the file system is - two spellings of one
    /// runtime's name are one runtime, and the row prints the spelling it saw first. A role is compared
    /// exactly: it is a token the engine wrote, not a file name.
    /// </remarks>
    public static IReadOnlyList<UncoveredProcessRow> Fold(
        IEnumerable<UncoveredChild> children, IEnumerable<ReachedEngine>? engines = null)
    {
        // The pids whose engines the session reached. A renderer is spawned BY the browser process that
        // holds the debugging port, and the core records the hooked process that spawned each uncovered
        // child - so the parent of a renderer IS the engine's pid when the session got in. Measured on
        // three recorded sessions across two engines (docs/STAN.md), never assumed.
        var reached = new HashSet<uint>();
        foreach (var engine in engines ?? [])
        {
            reached.Add(engine.Pid);
        }

        var groups = new Dictionary<string, ImageGroup>(StringComparer.OrdinalIgnoreCase);
        var order = new List<ImageGroup>();
        var unnamed = 0;
        foreach (var child in children)
        {
            var image = child.Image ?? string.Empty;
            if (image.Length == 0)
            {
                unnamed++;
                continue;
            }

            if (!groups.TryGetValue(image, out var group))
            {
                group = new ImageGroup(image);
                groups.Add(image, group);
                order.Add(group);
            }

            // 🔴 ONLY A RENDERER. Every helper of a reached engine shares its parent - the crash handler,
            // the GPU process, the utilities - and none of them hosts a page, so "its pages were reached"
            // said of them is a claim about something they do not have. Caught on the render, where the
            // sentence appeared on all four rows while every test here passed: the tests only ever fed
            // this renderers.
            var role = child.Role ?? string.Empty;
            group.Add(role, role == RendererRole && reached.Contains(child.ParentPid));
        }

        var rows = new List<UncoveredProcessRow>();
        foreach (var group in order
            .OrderByDescending(g => g.HasRenderer)
            .ThenByDescending(g => g.Total)
            .ThenBy(g => g.Image, StringComparer.OrdinalIgnoreCase))
        {
            rows.AddRange(group.Rows());
        }

        if (unnamed > 0)
        {
            rows.Add(new UncoveredProcessRow(
                string.Empty, IsUnnamed: true, string.Empty, IsRenderer: false, PagesReached: false, unnamed));
        }

        return rows;
    }

    public object[] ConvertBack(object value, Type[] targetTypes, object? parameter, CultureInfo culture)
        => throw new NotSupportedException("The process table is read-only.");

    /// <summary>One executable's processes, counted by role while the fold walks the list.</summary>
    private sealed class ImageGroup(string image)
    {
        // Keyed by role AND by whether the session reached its pages: two renderers of one executable,
        // one reached and one not, are two different things to tell the reader and must not be counted
        // into one row. Collapsing them would put a number against a claim true of only part of it.
        private readonly Dictionary<(string Role, bool Reached), int> _byRole = [];

        public string Image { get; } = image;

        public int Total { get; private set; }

        public bool HasRenderer => _byRole.Keys.Any(k => k.Role == RendererRole);

        public void Add(string role, bool pagesReached)
        {
            var key = (Role: role, Reached: pagesReached);
            _byRole[key] = _byRole.GetValueOrDefault(key) + 1;
            Total++;
        }

        public IEnumerable<UncoveredProcessRow> Rows()
            => _byRole
                .OrderByDescending(r => r.Key.Role == RendererRole)
                // A renderer still on the real clock leads the one whose pages were reached: it is the
                // row that changes the answer, and the reader should meet it first.
                .ThenBy(r => r.Key.Reached)
                .ThenByDescending(r => r.Value)
                .ThenBy(r => r.Key.Role, StringComparer.Ordinal)
                .Select(r => new UncoveredProcessRow(
                    Image, IsUnnamed: false, r.Key.Role, r.Key.Role == RendererRole, r.Key.Reached, r.Value));
    }
}
