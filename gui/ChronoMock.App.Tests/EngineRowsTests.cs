using ChronoMock.Protocol;

namespace ChronoMock.App.Tests;

/// <summary>
/// The fold behind the engine table: the endpoints a session verdict names in, the rows of one table out.
/// Pure, so it is tested without a binding or a window.
/// </summary>
public class EngineRowsTests
{
    private static ReachedEngine Engine(uint pid, int port, string browser = "Engine/153.0")
        => new() { Pid = pid, Port = port, Browser = browser };

    [Fact]
    public void An_engine_becomes_one_row_of_its_name_and_its_port()
    {
        var rows = EngineRowsConverter.Fold([Engine(4300, 61868)]);

        var row = Assert.Single(rows);
        Assert.Equal("Engine/153.0", row.Name);
        Assert.Equal(61868, row.Port);
        Assert.False(row.IsUnnamed);
    }

    [Fact]
    public void The_order_the_core_found_them_in_is_the_order_they_are_shown_in()
    {
        // The CLI report and the copied summary both print this list in wire order. A screen that sorted
        // it would be a third view of one session disagreeing with the other two about the same two rows.
        var rows = EngineRowsConverter.Fold(
        [
            Engine(4300, 61868, "Engine/153.0"),
            Engine(9100, 5123, "python/3.14"),
        ]);

        Assert.Equal([("Engine/153.0", 61868), ("python/3.14", 5123)], rows.Select(r => (r.Name, r.Port)));
    }

    [Fact]
    public void An_engine_that_named_nothing_is_marked_so_the_row_can_say_a_word_instead_of_nothing()
    {
        var rows = EngineRowsConverter.Fold([Engine(4300, 61868, string.Empty)]);

        var row = Assert.Single(rows);
        Assert.True(row.IsUnnamed);
        Assert.Equal(string.Empty, row.Name);
    }

    [Fact]
    public void One_endpoint_appears_once_even_when_the_wire_names_it_twice()
    {
        // The core keeps one attacher per port, so a repeat needs a listener to leave the socket table and
        // the same process to take the same port again. Two identical rows would read as two engines.
        var rows = EngineRowsConverter.Fold([Engine(4300, 61868), Engine(4300, 61868)]);

        Assert.Single(rows);
    }

    [Fact]
    public void Two_processes_on_one_port_number_are_two_engines()
    {
        // The pid is part of the key: a port freed by one process and taken by another is two endpoints
        // the session reached, and collapsing them would drop one from the audit (untouchable rule 4).
        var rows = EngineRowsConverter.Fold([Engine(4300, 61868), Engine(7777, 61868)]);

        Assert.Equal(2, rows.Count);
    }

    [Fact]
    public void A_port_prints_as_an_address_without_a_thousands_separator()
    {
        // The row is a port number, not a quantity: "61 868" is not a port anybody can dial, and a culture
        // that groups digits would produce exactly that.
        var previous = System.Threading.Thread.CurrentThread.CurrentCulture;
        try
        {
            System.Threading.Thread.CurrentThread.CurrentCulture = new System.Globalization.CultureInfo("pl-PL");
            Assert.Equal("61868", EngineRowsConverter.Fold([Engine(4300, 61868)])[0].PortText);
        }
        finally
        {
            System.Threading.Thread.CurrentThread.CurrentCulture = previous;
        }
    }

    [Fact]
    public void An_empty_list_folds_to_no_rows()
    {
        Assert.Empty(EngineRowsConverter.Fold([]));
    }

    [Fact]
    public void An_unset_binding_value_folds_to_no_rows_rather_than_throwing()
    {
        // A view rendered without a data context hands the converter DependencyProperty.UnsetValue.
        var converter = new EngineRowsConverter();

        var result = converter.Convert(
            System.Windows.DependencyProperty.UnsetValue,
            typeof(object), null, System.Globalization.CultureInfo.InvariantCulture);

        Assert.Empty(Assert.IsAssignableFrom<IEnumerable<EngineRow>>(result));
    }
}
