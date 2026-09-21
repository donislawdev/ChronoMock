using ChronoMock.Protocol;

namespace ChronoMock.App.Tests;

/// <summary>
/// The fold behind the audit table: five lists in, one ordered list of rows out. Pure, so it is tested
/// without a binding or a window.
/// </summary>
public class AuditRowsTests
{
    private static CoveredChannel Channel(string name, long calls) => new() { Channel = name, Calls = calls };

    [Fact]
    public void Rows_come_in_the_order_that_matters_real_first_good_news_last()
    {
        var rows = AuditRowsConverter.Fold(
            covered: [Channel("GetSystemTimeAsFileTime", 128004), Channel("timeGetTime", 7)],
            uncovered: ["GetLocalTime"],
            observed: [Channel("QueryPerformanceCounter", 3)],
            unobserved: ["NtQuerySystemTime"],
            installedLate: ["timeGetTime"]);

        Assert.Equal(
            ["GetLocalTime", "QueryPerformanceCounter", "NtQuerySystemTime", "timeGetTime", "GetSystemTimeAsFileTime"],
            rows.Select(r => r.Channel).ToArray());
        Assert.Equal(
            [AuditStatus.Real, AuditStatus.ByDesign, AuditStatus.Unwatched, AuditStatus.Late, AuditStatus.Fake],
            rows.Select(r => r.Status).ToArray());
    }

    [Fact]
    public void A_late_channel_is_one_row_with_its_count_not_two()
    {
        // The late list names channels that are also counted in the covered list. One function, one row:
        // marked late, carrying the count that is a floor. Reversal probe: fold covered without the late
        // filter and timeGetTime appears twice.
        var rows = AuditRowsConverter.Fold(
            covered: [Channel("timeGetTime", 7)],
            uncovered: [],
            observed: [],
            unobserved: [],
            installedLate: ["timeGetTime"]);

        var row = Assert.Single(rows);
        Assert.Equal(AuditStatus.Late, row.Status);
        Assert.Equal("7", row.CallsText);
    }

    [Fact]
    public void A_late_channel_left_real_by_design_is_one_row_with_its_count_and_both_facts()
    {
        // The late list can name an observed channel as well as a covered one. It used to fold to two rows
        // - the observed one with its count and a second, uncounted late one - because the deduplication
        // looked at the covered list alone. Reversal probe: build the counted set from covered only.
        var rows = AuditRowsConverter.Fold(
            covered: [],
            uncovered: [],
            observed: [Channel("QueryPerformanceCounter", 3)],
            unobserved: [],
            installedLate: ["QueryPerformanceCounter"]);

        var row = Assert.Single(rows);
        Assert.Equal(AuditStatus.ByDesignLate, row.Status);
        Assert.Equal("audit.status_by_design_late", row.StatusKey);
        Assert.Equal("3", row.CallsText);
    }

    [Fact]
    public void A_late_channel_the_covered_list_does_not_carry_still_gets_a_row()
    {
        // The audit never drops a fact the core reported (untouchable rule 4): a late name with no count
        // behind it is a row without a count, not a missing row.
        var rows = AuditRowsConverter.Fold(
            covered: [],
            uncovered: [],
            observed: [],
            unobserved: [],
            installedLate: ["timeGetTime"]);

        var row = Assert.Single(rows);
        Assert.Equal(AuditStatus.Late, row.Status);
        Assert.Equal(string.Empty, row.CallsText);
    }

    [Fact]
    public void Counts_print_in_the_invariant_digits_the_copied_summary_uses()
    {
        var rows = AuditRowsConverter.Fold(
            covered: [Channel("GetSystemTimeAsFileTime", 128004)],
            uncovered: [], observed: [], unobserved: [], installedLate: []);

        Assert.Equal("128004", rows[0].CallsText);
    }

    [Fact]
    public void An_unset_binding_value_folds_to_no_rows_rather_than_throwing()
    {
        // The state sheet renders views without a data context, and a multi-binding then hands the
        // converter DependencyProperty.UnsetValue in every slot.
        var converter = new AuditRowsConverter();

        var result = converter.Convert(
            [System.Windows.DependencyProperty.UnsetValue, System.Windows.DependencyProperty.UnsetValue,
             System.Windows.DependencyProperty.UnsetValue, System.Windows.DependencyProperty.UnsetValue,
             System.Windows.DependencyProperty.UnsetValue],
            typeof(object), null, System.Globalization.CultureInfo.InvariantCulture);

        Assert.Empty(Assert.IsAssignableFrom<IEnumerable<AuditRow>>(result));
    }
}
