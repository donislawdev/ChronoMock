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

    /// <summary>The store for the running app: a history folder next to the executable (portable).</summary>
    public static FileSessionHistoryStore ForApp()
        => new(Path.Combine(AppContext.BaseDirectory, "history"));

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
        catch (JsonException)
        {
            // A corrupt history must not crash the app or be deleted - start empty and leave the file be.
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
        File.WriteAllText(FilePath, JsonSerializer.Serialize(file, Options));
    }

    private sealed record HistoryFile
    {
        [JsonPropertyName("schema")] public int Schema { get; init; }

        [JsonPropertyName("stability")] public string Stability { get; init; } = "unstable";

        [JsonPropertyName("sessions")] public IReadOnlyList<SessionRecord> Sessions { get; init; } = [];
    }
}
