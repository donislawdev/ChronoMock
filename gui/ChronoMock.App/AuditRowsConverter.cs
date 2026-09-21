using System.Globalization;
using System.Windows.Data;
using ChronoMock.Protocol;

namespace ChronoMock.App;

/// <summary>
/// One row of the audit table: a time function the session saw, where it read its time from, and how many
/// times it read it. Built by <see cref="AuditRowsConverter"/> from the session's five lists, so the view
/// model keeps its lists and the screen gets one table.
/// </summary>
/// <param name="StatusKey">The translation key of the word in the status column.</param>
/// <param name="Status">The status the row's colour follows, one of the <see cref="AuditStatus"/> names.
/// A string rather than an enum because the template's triggers compare it, and a trigger compares text.</param>
/// <param name="Calls">The read count, or null for a channel the session could not count (the ones it did
/// not hook, and the ones nothing was hooked in).</param>
public sealed record AuditRow(string Channel, string StatusKey, string Status, long? Calls)
{
    /// <summary>The count as the table prints it: the same invariant digits the copy-summary uses, so the
    /// screen and the clipboard never disagree about a number. Empty where there is no count.</summary>
    public string CallsText => Calls?.ToString(CultureInfo.InvariantCulture) ?? string.Empty;
}

/// <summary>The status words of the audit table, in the order the table lists them: the readings that
/// decide the verdict first, the ones nobody could judge next, the good news last and longest.</summary>
public static class AuditStatus
{
    /// <summary>Read the real clock where the fake one was meant - the reading that fails a verdict.</summary>
    public const string Real = "real";

    /// <summary>Read the real clock because the session chose to leave it real (QPC unscaled, for one).</summary>
    public const string ByDesign = "design";

    /// <summary>A channel the session meant to watch and could not - nothing was hooked in.</summary>
    public const string Unwatched = "unwatched";

    /// <summary>Read the fake clock, but hooked only once its module loaded, so its count is a floor.</summary>
    public const string Late = "late";

    /// <summary>Read the fake clock from the start.</summary>
    public const string Fake = "fake";
}

/// <summary>
/// Folds the session's five audit lists into the rows of one table. Bound as a multi-value converter over
/// Covered, Uncovered, Observed, Unobserved and InstalledLate, in that order.
/// </summary>
/// <remarks>
/// A converter rather than a view model property: the rows are a presentation of lists the view model
/// already exposes, and that class stands on its coupling ceiling (gui/CodeMetricsConfig.txt), so the fold
/// lives beside the other row converters. A value that is not a list (the state sheet renders views
/// without a data context, and a binding then hands over UnsetValue) counts as an empty list, so the
/// table renders empty rather than throwing out of a binding.
/// </remarks>
public sealed class AuditRowsConverter : IMultiValueConverter
{
    public object Convert(object[]? values, Type targetType, object? parameter, CultureInfo culture)
    {
        if (values is null || values.Length < 5)
        {
            return Array.Empty<AuditRow>();
        }

        return Fold(
            values[0] as IEnumerable<CoveredChannel> ?? [],
            values[1] as IEnumerable<string> ?? [],
            values[2] as IEnumerable<CoveredChannel> ?? [],
            values[3] as IEnumerable<string> ?? [],
            values[4] as IEnumerable<string> ?? []);
    }

    /// <summary>The fold itself, testable without a binding. Order: real first (the verdict's reason),
    /// then not watched, then late, then the fake-clock rows - within a group, the order the session
    /// reported them. A covered channel named among the late ones is one row, marked late, not two.</summary>
    public static IReadOnlyList<AuditRow> Fold(
        IEnumerable<CoveredChannel> covered,
        IEnumerable<string> uncovered,
        IEnumerable<CoveredChannel> observed,
        IEnumerable<string> unobserved,
        IEnumerable<string> installedLate)
    {
        var late = new HashSet<string>(installedLate, StringComparer.Ordinal);
        var rows = new List<AuditRow>();
        rows.AddRange(uncovered.Select(name => new AuditRow(name, "audit.status_real", AuditStatus.Real, null)));
        rows.AddRange(observed.Select(c => new AuditRow(c.Channel, "audit.status_by_design", AuditStatus.ByDesign, c.Calls)));
        rows.AddRange(unobserved.Select(name => new AuditRow(name, "audit.status_unwatched", AuditStatus.Unwatched, null)));
        var coveredList = covered.ToList();
        rows.AddRange(coveredList.Where(c => late.Contains(c.Channel))
            .Select(c => new AuditRow(c.Channel, "audit.status_late", AuditStatus.Late, c.Calls)));
        // A late name the covered list does not carry is still a fact the core reported, and the audit
        // never drops one (untouchable rule 4): it gets its row, without a count.
        var counted = new HashSet<string>(coveredList.Select(c => c.Channel), StringComparer.Ordinal);
        rows.AddRange(installedLate.Where(name => !counted.Contains(name))
            .Select(name => new AuditRow(name, "audit.status_late", AuditStatus.Late, null)));
        rows.AddRange(coveredList.Where(c => !late.Contains(c.Channel))
            .Select(c => new AuditRow(c.Channel, "audit.status_fake", AuditStatus.Fake, c.Calls)));
        return rows;
    }

    public object[] ConvertBack(object value, Type[] targetTypes, object? parameter, CultureInfo culture)
        => throw new NotSupportedException("The audit table is read-only.");
}
