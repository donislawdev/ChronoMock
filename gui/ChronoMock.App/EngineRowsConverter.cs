using System.Globalization;
using System.Windows.Data;
using ChronoMock.Protocol;

namespace ChronoMock.App;

/// <summary>
/// One row of the engine table: a web engine inside the application whose debugging port the session
/// reached, so the pages it hosts ran on the session clock. Built by <see cref="EngineRowsConverter"/>
/// from the list the session verdict names.
/// </summary>
/// <param name="Name">What the engine calls itself, as the core read it out of the engine's own
/// version endpoint - empty when it named nothing, and the template prints a word for that instead.</param>
/// <param name="IsUnnamed">True when the engine gave no name, so the row says "engine" rather than
/// leaving the cell blank. A hole in a table reads as a bug in the table.</param>
/// <param name="Port">The loopback port the engine is listening on.</param>
public sealed record EngineRow(string Name, bool IsUnnamed, int Port)
{
    /// <summary>The port as the table prints it, in invariant digits like every number on this interface -
    /// a port is an address, and a thousands separator in an address would be a number nobody can dial.</summary>
    public string PortText => Port.ToString(CultureInfo.InvariantCulture);
}

/// <summary>
/// Turns the engines the session reached into the rows of one table. Bound as a value converter over
/// <c>Engines</c>.
/// </summary>
/// <remarks>
/// A converter rather than a view model property, for the reason <see cref="AuditRowsConverter"/> and
/// <see cref="UncoveredProcessRowsConverter"/> both give: the rows are a presentation of a list the view
/// model already exposes, and that class stands on its coupling ceiling (gui/CodeMetricsConfig.txt). A
/// value that is not a list - a view rendered without a data context hands over UnsetValue - counts as an
/// empty list, so the table renders empty rather than throwing out of a binding.
/// </remarks>
public sealed class EngineRowsConverter : IValueConverter
{
    public object Convert(object? value, Type targetType, object? parameter, CultureInfo culture)
        => Fold(value as IEnumerable<ReachedEngine> ?? []);

    /// <summary>
    /// The fold itself, testable without a binding. The order the core found them in is the order they
    /// are shown in, which is the order the CLI report and the copied summary print - three views of one
    /// session disagreeing about the order of the same two rows would be three views to reconcile.
    /// </summary>
    /// <remarks>
    /// One endpoint appears once. The core already keeps a single attacher per port, so a repeat needs a
    /// listener to leave the table and the same process to take the same port again - rare, and a row
    /// printed twice would read as two engines rather than as one seen twice. The pid is part of the key
    /// because two processes CAN hold the same port number in turn, and those are two engines.
    /// </remarks>
    public static IReadOnlyList<EngineRow> Fold(IEnumerable<ReachedEngine> engines)
    {
        var seen = new HashSet<(uint Pid, int Port)>();
        var rows = new List<EngineRow>();
        foreach (var engine in engines)
        {
            if (!seen.Add((engine.Pid, engine.Port)))
            {
                continue;
            }

            // Whitespace counts as no name. The core sanitises the engine's text but does not trim it
            // (crates/cli/src/cdp/mod.rs), and the text is whatever an engine inside somebody's
            // application puts in its own version endpoint - so a name of three spaces is possible, and
            // it would have drawn a blank cell: the hole in the table this row exists to avoid.
            var browser = engine.Browser ?? string.Empty;
            var unnamed = string.IsNullOrWhiteSpace(browser);
            rows.Add(new EngineRow(unnamed ? string.Empty : browser, unnamed, engine.Port));
        }

        return rows;
    }

    public object ConvertBack(object? value, Type targetType, object? parameter, CultureInfo culture)
        => throw new NotSupportedException("The engine table is read-only.");
}
