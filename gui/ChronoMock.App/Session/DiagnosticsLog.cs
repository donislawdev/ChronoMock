using System.Globalization;
using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).

namespace ChronoMock.App;

/// <summary>Limit on the diagnostics logs kept beside the app - a support aid, not an archive.</summary>
public static class DiagnosticsLogLimits
{
    /// <summary>The most recent diagnostics files kept; older ones are pruned on write.</summary>
    public const int Max = 50;
}

/// <summary>
/// Persists a diagnostics block when a session ends in anything other than a clean success (RELEASE-012),
/// so a QA report has a file to attach when an injection is blocked (Defender/AV), a hook is missing, or a
/// target vanishes. Injected into <see cref="SessionViewModel"/> so unit tests use a no-op and touch no
/// files. The block is composed by the view model (BuildDiagnosticsBlock); this only writes it.
/// </summary>
public interface IDiagnosticsLog
{
    /// <summary>Persist one diagnostics block and return the file path written, or null when it could not be
    /// saved (a read-only medium). Never throws - the in-memory copy behind the Copy-diagnostics button is
    /// the reliable path, and the file is a best-effort convenience.</summary>
    string? Save(string content);
}

/// <summary>No-op log: the default for a bare view-model and for unit tests, so they write no files.</summary>
public sealed class NoOpDiagnosticsLog : IDiagnosticsLog
{
    public string? Save(string content) => null;
}

/// <summary>
/// File log: writes one timestamped file per problem session into a logs/ folder beside the executable
/// (portable layout), falling back to a per-user writable folder when the exe folder is read-only - the
/// same choice the history store makes (a USB stick, or Program Files without admin). Best-effort: a write
/// failure returns null rather than throwing, because the diagnostics are also kept in memory for the
/// Copy-diagnostics button.
/// </summary>
public sealed class FileDiagnosticsLog : IDiagnosticsLog
{
    private readonly string _directory;

    internal FileDiagnosticsLog(string directory) => _directory = directory;

    /// <summary>The log for the running app: a logs/ folder next to the executable, or a per-user folder when
    /// that is read-only (mirrors <see cref="FileSessionHistoryStore.ForApp"/>).</summary>
    public static FileDiagnosticsLog ForApp()
    {
        var exeLogs = Path.Combine(AppContext.BaseDirectory, "logs");
        var perUser = Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "ChronoMock", "logs");
        return new FileDiagnosticsLog(ChooseWritableDir(exeLogs, perUser, IsWritable));
    }

    /// <summary>Pick <paramref name="preferred"/> when writable, else <paramref name="fallback"/>. The
    /// writability check is injected so the choice is unit-tested without a real read-only medium (mirrors
    /// the history store's ChooseWritableDir).</summary>
    internal static string ChooseWritableDir(string preferred, string fallback, Func<string, bool> isWritable)
        => isWritable(preferred) ? preferred : fallback;

    /// <summary>Whether a directory can be created and written to, by actually probing it (a real write is
    /// the only honest test on Windows, where a read-only attribute does not stop file creation).</summary>
    private static bool IsWritable(string dir)
    {
        try
        {
            Directory.CreateDirectory(dir);
            var probe = Path.Combine(dir, ".write-probe");
            File.WriteAllText(probe, string.Empty);
            File.Delete(probe);
            return true;
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            return false;
        }
    }

    public string? Save(string content)
    {
        try
        {
            Directory.CreateDirectory(_directory);
            // Sortable, filename-safe timestamp (no ':'); milliseconds keep two fast failures from colliding.
            var stamp = DateTime.UtcNow.ToString("yyyyMMddTHHmmssfffZ", CultureInfo.InvariantCulture);
            var path = Path.Combine(_directory, $"diagnostics-{stamp}.log");
            File.WriteAllText(path, content);
            Prune();
            return path;
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            return null; // best-effort: the in-memory copy behind the button still stands (rule 6 met there)
        }
    }

    // Keep only the most recent files so the folder does not grow without bound. Housekeeping only - a
    // failure here must not undo the save that already succeeded.
    private void Prune()
    {
        try
        {
            var files = Directory.GetFiles(_directory, "diagnostics-*.log");
            if (files.Length <= DiagnosticsLogLimits.Max)
            {
                return;
            }

            Array.Sort(files, StringComparer.Ordinal); // the ISO stamp sorts oldest first
            foreach (var stale in files.Take(files.Length - DiagnosticsLogLimits.Max))
            {
                File.Delete(stale);
            }
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            // A prune failure is ignored on purpose - the diagnostics file was already written.
        }
    }
}
