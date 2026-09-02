using System.IO;
using System.Text.Json;
using System.Windows;

namespace ChronoMock.App.Localization;

/// <summary>
/// Loads interface strings from loose JSON files next to the app, so a language is added by dropping a
/// Strings.&lt;culture&gt;.json file in the Localization folder - no recompile (untouchable rule 15).
/// The set of languages is discovered by scanning that folder, never a hardcoded list.
/// <para>
/// The files are DATA and are read as data. They were XAML, parsed with <c>XamlReader.Load</c>, which is
/// a full deserializer: it instantiates whatever types the document names, so a loose file beside the exe
/// could run code in the application's context - and the documented contract invites people to add such
/// files. Validating the result afterwards would not have helped, because the code runs during parsing.
/// JSON parses to strings and nothing else, so the whole class of problem is gone rather than guarded.
/// Comments are allowed (and skipped), which keeps the grouping that makes a 250-key file translatable.
/// </para>
/// </summary>
public static class LocalizationService
{
    public const string DefaultCulture = "en";

    private const string FolderName = "Localization";
    private const string FilePrefix = "Strings.";
    private const string FileSuffix = ".json";
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
    public static IReadOnlyList<string> AvailableCultures() => AvailableCulturesIn(FolderPath);

    /// <summary>The scan itself, over an explicit folder - the shipped layout goes through
    /// <see cref="AvailableCultures"/>, and this overload lets the odd-name cases be tested directly.</summary>
    public static IReadOnlyList<string> AvailableCulturesIn(string folderPath)
    {
        if (!Directory.Exists(folderPath))
        {
            return [];
        }

        var cultures = new List<string>();
        foreach (var file in Directory.EnumerateFiles(folderPath, $"{FilePrefix}*{FileSuffix}"))
        {
            var name = Path.GetFileName(file);
            // The glob can match a name too short to slice (a bare "Strings.xaml" beside the real ones):
            // the range would then throw rather than skip an unusable file.
            if (name.Length <= FilePrefix.Length + FileSuffix.Length)
            {
                continue;
            }

            var culture = name[FilePrefix.Length..^FileSuffix.Length];
            if (culture.Length > 0)
            {
                cultures.Add(culture);
            }
        }

        return cultures;
    }

    /// <summary>Options for the strings files: comments are part of the format (they carry the grouping
    /// that makes a large file translatable), and a trailing comma is a typo not worth failing over.</summary>
    private static readonly JsonSerializerOptions StringsJson = new()
    {
        ReadCommentHandling = JsonCommentHandling.Skip,
        AllowTrailingCommas = true,
    };

    /// <summary>Load the strings dictionary for a culture from its loose JSON file. Every value is a
    /// string by construction - the file cannot describe anything else.</summary>
    public static ResourceDictionary Load(string culture)
    {
        var path = Path.Combine(FolderPath, $"{FilePrefix}{culture}{FileSuffix}");
        if (!File.Exists(path))
        {
            throw new FileNotFoundException($"no strings file for culture '{culture}'", path);
        }

        var json = File.ReadAllText(path);
        Dictionary<string, string>? entries;
        try
        {
            entries = JsonSerializer.Deserialize<Dictionary<string, string>>(json, StringsJson);
        }
        catch (JsonException e)
        {
            // A malformed or hand-broken file names itself, rather than surfacing as a missing key later.
            throw new InvalidOperationException($"strings file '{path}' is not valid JSON: {e.Message}", e);
        }

        if (entries is null)
        {
            throw new InvalidOperationException($"strings file '{path}' is empty");
        }

        var dictionary = new ResourceDictionary();
        foreach (var (key, value) in entries)
        {
            dictionary[key] = value;
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
