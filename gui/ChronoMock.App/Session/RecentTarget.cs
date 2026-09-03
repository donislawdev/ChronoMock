using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).

namespace ChronoMock.App;

/// <summary>Limits on the recent-target list - a convenience picker, not a log.</summary>
public static class RecentTargetLimits
{
    /// <summary>How many distinct targets the dropdown offers. Deliberately far below the history cap:
    /// a dropdown long enough to scroll stops being faster than the file picker it replaces.</summary>
    public const int Max = 8;
}

/// <summary>
/// One entry in the recent-targets dropdown (chrono-mock 7.1 pt 1): a target the user has run before, so
/// re-running it does not mean walking the file picker again.
/// <para>
/// The list is DERIVED from the session history, which already records the target path of every session
/// (docs/04 section 6) - there is no second stored list and therefore no second data contract (rule 17).
/// </para>
/// </summary>
public sealed class RecentTarget : ObservableObject
{
    private bool _isMissing;

    public RecentTarget(string fullPath)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(fullPath);
        FullPath = fullPath;
    }

    /// <summary>The full path, as recorded. It is the identity of the entry and the accessible name.</summary>
    public string FullPath { get; }

    /// <summary>The executable's file name - what the row leads with.</summary>
    public string Name => Path.GetFileName(FullPath);

    /// <summary>The containing directory, shown dimmed beside the name. Two builds of one application are
    /// commonly both called app.exe (an x64 and an x86 output), so the name alone cannot tell them apart -
    /// picking the wrong one is exactly the mistake this list would otherwise make easy.</summary>
    public string Directory => Path.GetDirectoryName(FullPath) ?? string.Empty;

    /// <summary>True when the file is no longer there (a wiped build output, an uninstalled app). Refreshed
    /// off the UI thread when the dropdown opens - never guessed, and never claimed until measured.</summary>
    public bool IsMissing
    {
        get => _isMissing;
        internal set => Set(ref _isMissing, value);
    }

    /// <summary>Windows paths are case-insensitive, so two spellings of one path are one entry.</summary>
    internal static bool SamePath(string a, string b) => string.Equals(a, b, StringComparison.OrdinalIgnoreCase);
}
