using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using System.Xml.Linq;

namespace ChronoMock.App.Tests;

/// <summary>
/// The accessible-name guard for templated item lists (zasady/13). A ComboBox or ListBox that renders its
/// items through an ItemTemplate is bound to RECORDS, and WPF's item peer names a container from the data
/// item, not from the template's TextBlock - so without an ItemContainerStyle setting
/// AutomationProperties.Name, assistive tech hears the record's ToString().
///
/// This is not hypothetical and not a one-off: App.xaml already carries three styles written for exactly
/// this reason (LabelKeyComboItem, RecentTargetComboItem, ScenarioListItem), each with a comment saying
/// why - and both zone dropdowns were still missed, announcing
/// "ZoneOption { BiasMinutes = -120, Label = ... }" until 2026-09-07. A rule written three times in
/// comments and enforced nowhere is the guard that does not exist (untouchable rule 12).
///
/// Deliberately narrow: a ComboBox or ListBox WITHOUT a template is left alone, because its items are
/// strings whose ToString() is the label already (the calculator's "+"/"-" sign picker is the case in
/// point). The rule is "a template means a record means a name", nothing wider.
///
/// 🔴 That belief did NOT hold for the plain item lists, and this guard never looked at them: the warning
/// list holds translation KEYS, whose ToString() is the key, and the tables hold records. They are covered
/// twice now - here by shape (a view never uses the bare ItemsControl or HeaderedItemsControl, only the
/// text-named ones), and in <see cref="ItemPeerNameTests"/> by effect, on the names the rendered rows
/// report.
/// </summary>
public class ItemNameGuardTests
{
    /// <summary>A literal floor for the text-named lists, for the same reason as the one below.</summary>
    private const int TextNamedListsAtLeast = 19;

    /// <summary>
    /// Every item list in the application names its rows by what they show, because every one of them is a
    /// TextNamedList or TextNamedItems.
    /// </summary>
    /// <remarks>
    /// The effect guard cannot see the calculator's lists - its render fixture has no data behind it - so
    /// this is what keeps a new plain ItemsControl there from reading a record out again. Reversal probe:
    /// put one of the calculator's lists back to ItemsControl and this reddens on that line.
    /// </remarks>
    [Fact]
    public void No_view_lists_items_through_a_control_that_names_rows_by_their_data()
    {
        var appDir = TestPaths.AppDirectory();
        var bare = new List<string>();
        int textNamed = 0;

        foreach (var file in AppXaml(appDir))
        {
            var rel = Path.GetRelativePath(appDir, file).Replace('\\', '/');
            foreach (var element in XDocument.Load(file, LoadOptions.SetLineInfo).Descendants())
            {
                switch (element.Name.LocalName)
                {
                    case "ItemsControl" or "HeaderedItemsControl":
                        bare.Add($"  {rel}:{((System.Xml.IXmlLineInfo)element).LineNumber}: <{element.Name.LocalName}>");
                        break;
                    case "TextNamedList" or "TextNamedItems":
                        textNamed++;
                        break;
                }
            }
        }

        Assert.True(
            textNamed >= TextNamedListsAtLeast,
            $"only {textNamed} text-named lists were found, expected at least {TextNamedListsAtLeast} - the scan "
                + "stopped reading the views, so this guard was about to pass over nothing");
        Assert.True(
            bare.Count == 0,
            "these lists would name each row by its data item's ToString() - a translation key or a record - "
                + $"instead of what it shows. Use controls:TextNamedList or controls:TextNamedItems:{Environment.NewLine}"
                + string.Join(Environment.NewLine, bare));
    }

    /// <summary>Every XAML file of the application, build output left out.</summary>
    private static IEnumerable<string> AppXaml(string appDir)
        => Directory.EnumerateFiles(appDir, "*.xaml", SearchOption.AllDirectories)
            .Where(file => !IsBuildOutput(Path.GetRelativePath(appDir, file).Replace('\\', '/')));

    private static bool IsBuildOutput(string rel)
        => rel.Contains("/bin/", StringComparison.OrdinalIgnoreCase)
            || rel.Contains("/obj/", StringComparison.OrdinalIgnoreCase)
            || rel.StartsWith("bin/", StringComparison.OrdinalIgnoreCase)
            || rel.StartsWith("obj/", StringComparison.OrdinalIgnoreCase);

    /// <summary>The floor is a LITERAL, not a count derived from the same scan. A guard that counts what it
    /// found and then checks its own count passes just as happily over an empty directory - which is how a
    /// path change turns a guard into decoration without ever going red.</summary>
    private const int TemplatedListsAtLeast = 9;

    [Fact]
    public void Every_templated_item_list_names_its_items_for_assistive_tech()
    {
        var (templated, unnamed) = ScanViews();

        Assert.True(
            templated >= TemplatedListsAtLeast,
            $"only {templated} templated item lists were scanned, expected at least {TemplatedListsAtLeast} - "
                + "the views moved or the scan stopped reading them, so this guard was about to pass over nothing");

        Assert.True(
            unnamed.Count == 0,
            "these item lists render records through an ItemTemplate but set no ItemContainerStyle, so "
                + "assistive tech would read the record's ToString(). Add a style with "
                + $"AutomationProperties.Name (see App.xaml):{Environment.NewLine}"
                + string.Join(Environment.NewLine, unnamed));
    }

    /// <summary>Count the templated lists and collect the ones with no container style. XML rather than a
    /// regex because the answer depends on element BOUNDARIES: an ItemTemplate belongs to the list that
    /// encloses it, and a nested list inside a template is a different element with its own answer.</summary>
    private static (int Templated, IReadOnlyList<string> Unnamed) ScanViews()
    {
        var appDir = TestPaths.AppDirectory();
        var unnamed = new List<string>();
        int templated = 0;

        foreach (var file in AppXaml(appDir))
        {
            var rel = Path.GetRelativePath(appDir, file).Replace('\\', '/');
            foreach (var element in XDocument.Load(file).Descendants())
            {
                var name = element.Name.LocalName;
                if (name is not ("ComboBox" or "ListBox") || element.Attribute("ItemsSource") is null)
                {
                    continue;
                }

                bool hasTemplate = element.Attribute("ItemTemplate") is not null
                    || element.Elements().Any(c => c.Name.LocalName == $"{name}.ItemTemplate");
                if (!hasTemplate)
                {
                    continue; // items are plain strings - ToString() already IS the label
                }

                templated++;
                if (element.Attribute("ItemContainerStyle") is null)
                {
                    unnamed.Add($"  {rel}: <{name} ItemsSource=\"{element.Attribute("ItemsSource")?.Value}\">");
                }
            }
        }

        return (templated, unnamed);
    }
}
