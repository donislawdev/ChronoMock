using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using System.Text.Json;
using System.Text.Json.Serialization;

namespace ChronoMock.App;

/// <summary>Limits on the local history - it is a convenience log, not an archive.</summary>
public static class SessionHistoryLimits
{
    /// <summary>The most recent sessions kept; older ones are dropped on append.</summary>
    public const int Max = 50;
}

/// <summary>
/// Reads, appends, and prunes the local session history (docs/04 section 6). Local-only, never exported.
/// Injected into <see cref="SessionViewModel"/> so unit tests use an in-memory store and touch no files.
/// </summary>
public interface ISessionHistoryStore
{
    /// <summary>Load the recorded sessions in the order they were written (oldest first). Returns empty for
    /// a missing or corrupt file - a broken history must never crash the app or be silently deleted.</summary>
    IReadOnlyList<SessionRecord> Load();

    /// <summary>Append one session and persist, keeping only the most recent <see cref="SessionHistoryLimits.Max"/>.
    /// Throws <see cref="IOException"/> or <see cref="UnauthorizedAccessException"/> when the location cannot
    /// be written (e.g. a read-only drive) so the caller can say so out loud, never swallow it (rule 6).</summary>
    void Append(SessionRecord record);

    /// <summary>Remove one recorded session (matched by value). Same write-failure contract as Append.</summary>
    void Remove(SessionRecord record);

    /// <summary>Remove every recorded session. Same write-failure contract as Append.</summary>
    void Clear();
}

/// <summary>In-memory store: the default for a bare view-model and for unit tests, so they touch no files.</summary>
public sealed class InMemorySessionHistoryStore : ISessionHistoryStore
{
    private readonly List<SessionRecord> _records = [];

    public IReadOnlyList<SessionRecord> Load() => [.. _records];

    public void Append(SessionRecord record)
    {
        _records.Add(record);
        if (_records.Count > SessionHistoryLimits.Max)
        {
            _records.RemoveAt(0); // drop the oldest
        }
    }

    public void Remove(SessionRecord record) => _records.Remove(record);

    public void Clear() => _records.Clear();
}

/// <summary>
/// File store: one JSON file per the portable layout (history/sessions.json next to the executable,
/// docs/04 section 7). The file wraps the records with a schema and an "unstable" marker while the shape
/// is not frozen; history is local-only, so its shape is not an exchange contract (docs/04 row 27).
/// </summary>
public sealed class FileSessionHistoryStore : ISessionHistoryStore
{
    private const int Schema = 1;
    private const string FileName = "sessions.json";

    private static readonly JsonSerializerOptions Options = new()
    {
        WriteIndented = true,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
    };

    private readonly string _directory;

    public FileSessionHistoryStore(string directory) => _directory = directory;

    /// <summary>The store for the running app: a history folder next to the executable (portable). When that
    /// location is read-only - a USB stick, or Program Files without admin - fall back to a per-user
    /// writable folder so the log still saves instead of every session reporting a write error.</summary>
    public static FileSessionHistoryStore ForApp()
    {
        var exeHistory = Path.Combine(AppContext.BaseDirectory, "history");
        var perUser = Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "ChronoMock", "history");
        return new FileSessionHistoryStore(ChooseWritableDir(exeHistory, perUser, IsWritable));
    }

    /// <summary>Pick <paramref name="preferred"/> when it is writable, else <paramref name="fallback"/>. The
    /// writability check is injected so the choice is unit-tested without a real read-only medium.</summary>
    internal static string ChooseWritableDir(string preferred, string fallback, Func<string, bool> isWritable)
        => isWritable(preferred) ? preferred : fallback;

    /// <summary>Whether a directory can be created and written to, by actually probing it (a directory's
    /// read-only attribute does not stop file creation on Windows, so a real write is the only honest test).</summary>
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

    private string FilePath => Path.Combine(_directory, FileName);

    public IReadOnlyList<SessionRecord> Load()
    {
        if (!File.Exists(FilePath))
        {
            return [];
        }

        try
        {
            var file = JsonSerializer.Deserialize<HistoryFile>(File.ReadAllText(FilePath), Options);
            return file?.Sessions ?? [];
        }
        catch (Exception e) when (e is JsonException or IOException or UnauthorizedAccessException)
        {
            // A corrupt OR unreadable history must not crash the app or be deleted - start empty and leave
            // the file be (the interface contract, rule 6). IOException covers a file locked by a second
            // portable instance; UnauthorizedAccessException a read-denied location.
            return [];
        }
    }

    public void Append(SessionRecord record)
    {
        var sessions = new List<SessionRecord>(Load()) { record };
        if (sessions.Count > SessionHistoryLimits.Max)
        {
            sessions.RemoveRange(0, sessions.Count - SessionHistoryLimits.Max); // drop the oldest
        }

        Write(sessions);
    }

    public void Remove(SessionRecord record)
    {
        var sessions = new List<SessionRecord>(Load());
        if (sessions.Remove(record))
        {
            Write(sessions);
        }
    }

    public void Clear()
    {
        if (File.Exists(FilePath))
        {
            File.Delete(FilePath);
        }
    }

    private void Write(IReadOnlyList<SessionRecord> sessions)
    {
        Directory.CreateDirectory(_directory); // no-op when it exists; throws only on a real failure
        var file = new HistoryFile { Schema = Schema, Stability = "unstable", Sessions = sessions };
        var json = JsonSerializer.Serialize(file, Options);

        // Write to a sibling temp file, then move it into place (L-13). A crash mid-write then leaves the
        // PREVIOUS history intact rather than a truncated file that the next Load reads as empty and the
        // next Append overwrites - losing the whole log, not just the in-flight entry. File.Move(overwrite)
        // is atomic on one volume, and the temp is beside the target so it always is.
        var temp = FilePath + ".tmp";
        File.WriteAllText(temp, json);
        File.Move(temp, FilePath, overwrite: true);
    }

    private sealed record HistoryFile
    {
        [JsonPropertyName("schema")] public int Schema { get; init; }

        [JsonPropertyName("stability")] public string Stability { get; init; } = "unstable";

        [JsonPropertyName("sessions")] public IReadOnlyList<SessionRecord> Sessions { get; init; } = [];
    }
}
