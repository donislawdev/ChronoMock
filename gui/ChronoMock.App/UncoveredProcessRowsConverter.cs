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
/// <param name="Count">How many processes this row stands for.</param>
public sealed record UncoveredProcessRow(string Image, bool IsUnnamed, string Role, bool IsRenderer, int Count)
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
public sealed class UncoveredProcessRowsConverter : IValueConverter
{
    /// <summary>The role token web pages run in, as the core names it (crates/cli/src/embedded.rs).</summary>
    internal const string RendererRole = "renderer";

    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
        => Fold(value as IEnumerable<UncoveredChild> ?? []);

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
    public static IReadOnlyList<UncoveredProcessRow> Fold(IEnumerable<UncoveredChild> children)
    {
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

            group.Add(child.Role ?? string.Empty);
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
            rows.Add(new UncoveredProcessRow(string.Empty, IsUnnamed: true, string.Empty, IsRenderer: false, unnamed));
        }

        return rows;
    }

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotSupportedException("The process table is read-only.");

    /// <summary>One executable's processes, counted by role while the fold walks the list.</summary>
    private sealed class ImageGroup(string image)
    {
        private readonly Dictionary<string, int> _byRole = new(StringComparer.Ordinal);

        public string Image { get; } = image;

        public int Total { get; private set; }

        public bool HasRenderer => _byRole.ContainsKey(RendererRole);

        public void Add(string role)
        {
            _byRole[role] = _byRole.GetValueOrDefault(role) + 1;
            Total++;
        }

        public IEnumerable<UncoveredProcessRow> Rows()
            => _byRole
                .OrderByDescending(r => r.Key == RendererRole)
                .ThenByDescending(r => r.Value)
                .ThenBy(r => r.Key, StringComparer.Ordinal)
                .Select(r => new UncoveredProcessRow(Image, IsUnnamed: false, r.Key, r.Key == RendererRole, r.Value));
    }
}
