using System.IO;
using System.Windows;
using System.Windows.Markup;

namespace ChronoMock.App.Localization;

/// <summary>
/// Loads interface strings from loose XAML files next to the app, so a language is added by dropping a
/// Strings.&lt;culture&gt;.xaml file in the Localization folder - no recompile (untouchable rule 15).
/// The set of languages is discovered by scanning that folder, never a hardcoded list.
/// </summary>
public static class LocalizationService
{
    public const string DefaultCulture = "en";

    private const string FolderName = "Localization";
    private const string FilePrefix = "Strings.";
    private const string FileSuffix = ".xaml";
    private const string MarkerKey = "__chrono_strings_culture";

    private static string FolderPath => Path.Combine(AppContext.BaseDirectory, FolderName);

    /// <summary>The culture whose strings are currently applied (read from the marker), or the default
    /// when none has been applied yet. Used to pick the language of DATA - preset name/explains carry their
    /// own {en, pl} values, unlike the interface keys (rules 15/16). </summary>
    public static string CurrentCulture
    {
        get
        {
            if (Application.Current?.Resources.MergedDictionaries is { } merged)
            {
                foreach (var dictionary in merged)
                {
                    if (dictionary.Contains(MarkerKey) && dictionary[MarkerKey] is string culture)
                    {
                        return culture;
                    }
                }
            }

            return DefaultCulture;
        }
    }

    /// <summary>The cultures that have a strings file present, discovered by scanning the folder.</summary>
    public static IReadOnlyList<string> AvailableCultures()
    {
        if (!Directory.Exists(FolderPath))
        {
            return [];
        }

        var cultures = new List<string>();
        foreach (var file in Directory.EnumerateFiles(FolderPath, $"{FilePrefix}*{FileSuffix}"))
        {
            var name = Path.GetFileName(file);
            var culture = name[FilePrefix.Length..^FileSuffix.Length];
            if (culture.Length > 0)
            {
                cultures.Add(culture);
            }
        }

        return cultures;
    }

    /// <summary>Load the strings dictionary for a culture from its loose XAML file.</summary>
    public static ResourceDictionary Load(string culture)
    {
        var path = Path.Combine(FolderPath, $"{FilePrefix}{culture}{FileSuffix}");
        if (!File.Exists(path))
        {
            throw new FileNotFoundException($"no strings file for culture '{culture}'", path);
        }

        using var stream = File.OpenRead(path);
        if (XamlReader.Load(stream) is not ResourceDictionary dictionary)
        {
            throw new InvalidOperationException($"strings file '{path}' is not a ResourceDictionary");
        }

        return dictionary;
    }

    /// <summary>
    /// Merge a culture's strings into the application's resources, replacing any previously applied set
    /// (so a language swap does not stack dictionaries).
    /// </summary>
    public static void Apply(Application app, string culture)
    {
        ArgumentNullException.ThrowIfNull(app);

        var dictionary = Load(culture);
        RemovePrevious(app.Resources.MergedDictionaries);
        dictionary[MarkerKey] = culture;
        app.Resources.MergedDictionaries.Add(dictionary);
    }

    private static void RemovePrevious(ICollection<ResourceDictionary> merged)
    {
        var existing = merged.FirstOrDefault(d => d.Contains(MarkerKey));
        if (existing is not null)
        {
            merged.Remove(existing);
        }
    }
}
