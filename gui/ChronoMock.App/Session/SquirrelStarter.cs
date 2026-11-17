using System.Globalization;
using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).

namespace ChronoMock.App;

/// <summary>
/// The program an installer's starter would start, found the same way the CLI finds it (crate::squirrel).
/// An application installed by Squirrel.Windows is started through a starter beside Update.exe, which only
/// starts the program of its own name in the newest finished app-&lt;version&gt; folder beside it and closes.
/// The core starts that program in the starter's place, so the panel picks the core by that program's
/// bitness and the mechanism by that program's folder. Both sides read the version cases in
/// tests/squirrel/newest-version.txt.
/// </summary>
internal static class SquirrelStarter
{
    /// <summary>The program the installer keeps beside its starters.</summary>
    private const string Updater = "Update.exe";

    /// <summary>The start of the name of a folder holding one version of the application.</summary>
    private const string VersionFolderPrefix = "app-";

    /// <summary>What a version folder holds while the installer is still writing it.</summary>
    private const string NotFinished = ".not-finished";

    /// <summary>The program a session on <paramref name="targetPath"/> starts: the program behind an
    /// installer's starter, or the target itself.</summary>
    public static string SessionProgram(string targetPath) => ProgramBehind(targetPath) ?? targetPath;

    /// <summary>The program the starter at <paramref name="targetPath"/> would start, if it is one: Update.exe
    /// beside it, and the program of the same name in the newest finished version folder beside it. Null when
    /// there is no such program - the starter itself would fail then, and the session starts the starter.</summary>
    public static string? ProgramBehind(string targetPath)
    {
        var dir = Path.GetDirectoryName(targetPath);
        var name = Path.GetFileName(targetPath);
        if (string.IsNullOrEmpty(dir) || string.IsNullOrEmpty(name)
            || string.Equals(name, Updater, StringComparison.OrdinalIgnoreCase)
            || !File.Exists(Path.Combine(dir, Updater)))
        {
            return null;
        }

        var newest = NewestVersion(FinishedVersionFolders(dir));
        if (newest is null)
        {
            return null;
        }

        var program = Path.Combine(dir, newest, name);
        return File.Exists(program) ? program : null;
    }

    /// <summary>The names of the version folders in <paramref name="dir"/> that the installer finished
    /// writing. A folder that cannot be read has none.</summary>
    private static List<string> FinishedVersionFolders(string dir)
    {
        try
        {
            return Directory.EnumerateDirectories(dir)
                .Where(folder => !Path.Exists(Path.Combine(folder, NotFinished)))
                .Select(Path.GetFileName)
                .OfType<string>()
                .Where(folder => VersionText(folder) is not null)
                .ToList();
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            return [];
        }
    }

    /// <summary>The version part of a version folder's name, its prefix matched without regard to case as
    /// Windows matches the starter's app-*.</summary>
    private static string? VersionText(string name) =>
        name.StartsWith(VersionFolderPrefix, StringComparison.OrdinalIgnoreCase) ? name[VersionFolderPrefix.Length..] : null;

    /// <summary>Of the version folders named, the one the starter starts: the highest version above 0.0.0,
    /// the first named of equal ones. A name whose version does not read as Semantic Versioning 2.0.0 is
    /// passed over.</summary>
    internal static string? NewestVersion(IEnumerable<string> names)
    {
        var best = SemanticVersion.Zero;
        string? bestName = null;
        foreach (var name in names)
        {
            var version = VersionText(name) is { } text ? SemanticVersion.Parse(text) : null;
            if (version is not null && version.CompareTo(best) > 0)
            {
                best = version;
                bestName = name;
            }
        }

        return bestName;
    }

    /// <summary>A Semantic Versioning 2.0.0 version, held for its precedence: the build part is read and
    /// then left out, as precedence leaves it out.</summary>
    private sealed class SemanticVersion : IComparable<SemanticVersion>
    {
        public static readonly SemanticVersion Zero = new([0, 0, 0], []);

        private readonly ulong[] _core;
        private readonly string[] _pre;

        private SemanticVersion(ulong[] core, string[] pre)
        {
            _core = core;
            _pre = pre;
        }

        /// <summary>major.minor.patch, then - and dot-separated pre-release identifiers, then + and
        /// dot-separated build identifiers. Numbers have no leading zero, identifiers are ASCII letters,
        /// digits and hyphens.</summary>
        public static SemanticVersion? Parse(string text)
        {
            var plus = text.IndexOf('+', StringComparison.Ordinal);
            if (plus >= 0 && !text[(plus + 1)..].Split('.').All(IsIdentifier))
            {
                return null;
            }

            var rest = plus >= 0 ? text[..plus] : text;
            var dash = rest.IndexOf('-', StringComparison.Ordinal);
            var parts = (dash >= 0 ? rest[..dash] : rest).Split('.');
            if (parts.Length != 3)
            {
                return null;
            }

            var core = new ulong[3];
            for (var i = 0; i < core.Length; i++)
            {
                if (Number(parts[i]) is not { } value)
                {
                    return null;
                }

                core[i] = value;
            }

            var pre = dash >= 0 ? rest[(dash + 1)..].Split('.') : [];
            return pre.All(id => IsIdentifier(id) && (!IsDigits(id) || Number(id) is not null)) ? new SemanticVersion(core, pre) : null;
        }

        public int CompareTo(SemanticVersion? other)
        {
            if (other is null)
            {
                return 1;
            }

            for (var i = 0; i < _core.Length; i++)
            {
                var byPart = _core[i].CompareTo(other._core[i]);
                if (byPart != 0)
                {
                    return byPart;
                }
            }

            if (_pre.Length == 0 || other._pre.Length == 0)
            {
                // A version without a pre-release is above one with it, and two without are equal.
                return (_pre.Length == 0).CompareTo(other._pre.Length == 0);
            }

            for (var i = 0; i < Math.Min(_pre.Length, other._pre.Length); i++)
            {
                var byIdentifier = IdentifierOrder(_pre[i], other._pre[i]);
                if (byIdentifier != 0)
                {
                    return byIdentifier;
                }
            }

            return _pre.Length.CompareTo(other._pre.Length);
        }

        /// <summary>Numbers by value and below every other identifier, the rest by their ASCII text.</summary>
        private static int IdentifierOrder(string a, string b) => (Number(a), Number(b)) switch
        {
            ({ } x, { } y) => x.CompareTo(y),
            ({ }, null) => -1,
            (null, { }) => 1,
            _ => string.CompareOrdinal(a, b),
        };

        /// <summary>A number without a leading zero that fits 64 bits.</summary>
        private static ulong? Number(string text) =>
            IsDigits(text) && !(text.Length > 1 && text[0] == '0')
            && ulong.TryParse(text, NumberStyles.None, CultureInfo.InvariantCulture, out var value)
                ? value
                : null;

        private static bool IsDigits(string text) => text.Length > 0 && text.All(char.IsAsciiDigit);

        private static bool IsIdentifier(string text) => text.Length > 0 && text.All(c => char.IsAsciiLetterOrDigit(c) || c == '-');
    }
}
