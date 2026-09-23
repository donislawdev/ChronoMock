using System.Text.RegularExpressions;
using System.Windows;
using System.Windows.Automation.Peers;
using ChronoMock.App.Views;

namespace ChronoMock.App.Tests;

/// <summary>
/// The name assistive tech reads for every row of every list, measured on the automation peers of rendered
/// screens rather than read from the markup.
/// </summary>
/// <remarks>
/// 🔴 THE MARKUP GUARD NEXT DOOR (<see cref="ItemNameGuardTests"/>) HAD TWO BLIND SPOTS, AND THE WARNINGS LIST
/// SAT IN BOTH. It reads ComboBox and ListBox only, so an ItemsControl whose template comes from a style was
/// never looked at. And it leaves a list with no template alone on the belief that its items are strings
/// whose ToString() is already the label - which is false for a list of translation KEYS, where ToString()
/// is the key. So a screen reader on a warning row read "runtime.dotnet_stopwatch_qpc" while the box on
/// screen said a sentence, and every guard was green.
///
/// This one asks the question the user's tools ask: the automation peer of each item, and the name it
/// reports. A row fails when that name is empty, a raw translation key, a record's printout
/// ("AuditRow { ... }"), or a type name - four ways of reading out something the screen never shows.
///
/// Measured before the fix: 87 rows on nine lists - the audit, process and engine tables and the speed
/// and jump buttons read a record's printout, the warnings and the cleanup list a raw key. The fix is
/// <see cref="Controls.TextNamedRow"/>, a row container named by what the row shows.
///
/// Reversal probe: make TextNamedList hand out a plain ContentPresenter again and this reddens on every
/// table and note list, the catalogue's included.
/// </remarks>
public class ItemPeerNameTests
{
    /// <summary>A literal floor, so a scan that stopped reaching the rows cannot pass over nothing.</summary>
    private const int ItemsAtLeast = 60;

    private const int CatalogueWidth = 900;

    private const int CatalogueHeight = 7168;

    [Fact]
    public void Every_row_of_every_list_is_named_by_what_it_says()
    {
        var (items, offenders) = WpfTestHost.InvokeSettled(() =>
        {
            int items = 0;
            var offenders = new List<string>();
            foreach (var (screen, view, width, height) in Screens())
            {
                LayoutProbe.Settle(view, width, height);
                var root = UIElementAutomationPeer.CreatePeerForElement(view)
                    ?? throw new InvalidOperationException($"{screen} has no automation peer");
                foreach (var (item, list) in ItemPeers(root, "(no list)"))
                {
                    items++;
                    string name = item.GetName() ?? string.Empty;
                    if (Unreadable(name) is { } why)
                    {
                        offenders.Add($"  {screen} / {list}: \"{name}\" ({why})");
                    }
                }
            }

            return (items, offenders);
        });

        Assert.True(
            items >= ItemsAtLeast,
            $"only {items} list rows were reached, expected at least {ItemsAtLeast} - the screens stopped "
                + "rendering their lists, so this guard was about to pass over nothing");
        Assert.True(
            offenders.Count == 0,
            "these rows read out something the screen does not show. Make the list a controls:TextNamedList or "
                + "controls:TextNamedItems, whose TextNamedRow names each row by its rendered text - or, for a "
                + "ListBox or ComboBox, set AutomationProperties.Name in its ItemContainerStyle:"
                + Environment.NewLine
                + string.Join(Environment.NewLine, offenders.Distinct()));
    }

    /// <summary>Why a name would mislead a listener, or null when it is a name. On the UI thread, because a
    /// key is whatever the loaded string dictionaries know.</summary>
    /// <remarks>
    /// A key is asked of the dictionaries rather than matched by shape. The first version matched
    /// "word.word" and called the history row "target.exe" a key - a file name, which is exactly what that
    /// row should say.
    /// </remarks>
    private static string? Unreadable(string name)
    {
        if (string.IsNullOrWhiteSpace(name))
        {
            return "empty";
        }

        if (TranslationKeyConverter.Resolve(name) != name)
        {
            return "a translation key";
        }

        if (RecordPrintout.IsMatch(name))
        {
            return "a record's ToString()";
        }

        return TypeName.IsMatch(name) ? "a type name" : null;
    }

    [Theory]
    [InlineData("runtime.dotnet_stopwatch_qpc", "a translation key")]
    [InlineData("cleanup.chromium_profile_left", "a translation key")]
    [InlineData("AuditRow { Function = GetTickCount64, Count = 3 }", "a record's ToString()")]
    [InlineData("ChronoMock.App.ViewModels.AuditRow", "a type name")]
    [InlineData("", "empty")]
    [InlineData("GetTickCount64", null)]
    [InlineData("target.exe", null)]
    [InlineData("The application never read the clock this session replaces.", null)]
    [InlineData("UTC+02:00 · Central European Summer Time", null)]
    public void The_name_test_tells_a_name_from_a_leak(string name, string? expected)
        => Assert.Equal(expected, WpfTestHost.Invoke(() => Unreadable(name)));

    private static readonly Regex RecordPrintout = new(@"^\w+ \{ .* \}$", RegexOptions.CultureInvariant);

    private static readonly Regex TypeName = new(@"^([A-Z]\w*\.)+[A-Z]\w*$", RegexOptions.CultureInvariant);

    /// <summary>Every item peer under the root, with the name of the list that holds it.</summary>
    private static IEnumerable<(AutomationPeer Item, string List)> ItemPeers(AutomationPeer peer, string list)
    {
        foreach (var child in peer.GetChildren() ?? [])
        {
            string holder = child is ItemsControlAutomationPeer { Owner: FrameworkElement owner }
                ? (string.IsNullOrEmpty(owner.Name) ? owner.GetType().Name : owner.Name)
                : list;
            if (child is ItemAutomationPeer)
            {
                yield return (child, holder);
            }

            foreach (var nested in ItemPeers(child, holder))
            {
                yield return nested;
            }
        }
    }

    /// <summary>The screens with lists on them, each with the section that holds its lists opened.</summary>
    private static IEnumerable<(string Name, FrameworkElement View, int Width, int Height)> Screens()
    {
        yield return ("catalogue", new ComponentCatalogue(), CatalogueWidth, CatalogueHeight);
        yield return ("setup with the scenarios open", Opened(new SetupPhaseView { DataContext = PhaseStates.SetupStartup() }, "ScenarioSection"), LayoutProbe.WindowWidth, LayoutProbe.WindowHeight);
        yield return ("setup with every option", Opened(new SetupPhaseView { DataContext = PhaseStates.SetupWithEveryOption() }, "SpeedSection"), LayoutProbe.WindowWidth, LayoutProbe.WindowHeight);
        yield return ("session with the audit open", Opened(new SessionPhaseView { DataContext = PhaseStates.WithTarget(SessionStates.RunningWithCoverageWarnings()) }, "AuditSection"), LayoutProbe.WindowWidth, LayoutProbe.WindowHeight);
        yield return ("result with processes", Opened(new ResultPhaseView { DataContext = PhaseStates.ResultPartialWithUncoveredProcesses() }, "AuditSection"), LayoutProbe.WindowWidth, LayoutProbe.WindowHeight);
        yield return ("result with pages", Opened(new ResultPhaseView { DataContext = PhaseStates.ResultPartialWithEmbeddedPages() }, "AuditSection"), LayoutProbe.WindowWidth, LayoutProbe.WindowHeight);
        yield return ("result with the history open", Opened(new ResultPhaseView { DataContext = PhaseStates.ResultWithHistoryChosen() }, "HistorySection"), LayoutProbe.WindowWidth, LayoutProbe.WindowHeight);
    }

    private static FrameworkElement Opened(FrameworkElement view, string section)
    {
        var expander = LayoutProbe.FindNamed(view, section) as System.Windows.Controls.Expander
            ?? throw new InvalidOperationException($"{view.GetType().Name} has no section called {section}");
        expander.IsExpanded = true;
        return view;
    }
}
