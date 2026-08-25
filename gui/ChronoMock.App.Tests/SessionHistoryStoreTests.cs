using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using ChronoMock.App;

namespace ChronoMock.App.Tests;

/// <summary>
/// The local session history store (docs/04 section 6): append and load round-trip, a missing file is
/// empty, and a corrupt file is empty but never deleted (a broken history must not crash or be lost).
/// </summary>
public sealed class SessionHistoryStoreTests : IDisposable
{
    private readonly string _dir =
        Path.Combine(Path.GetTempPath(), "chrono-hist-tests", Guid.NewGuid().ToString("N"));

    public void Dispose()
    {
        if (Directory.Exists(_dir))
        {
            Directory.Delete(_dir, recursive: true);
        }
    }

    private static SessionRecord Record(string name, string verdict = "works") => new()
    {
        TargetPath = $@"C:\apps\{name}.exe",
        MomentLocal = "2038-01-19T03:14:07",
        TzBiasMin = -120,
        Mode = "multiplier",
        Multiplier = 60,
        Verdict = verdict,
        EndedAtUtc = "2026-08-25T09:00:00Z",
    };

    [Fact]
    public void Append_then_load_round_trips_the_records_in_order()
    {
        var store = new FileSessionHistoryStore(_dir);
        store.Append(Record("Alpha"));
        store.Append(Record("Beta", "partial"));

        var loaded = store.Load();

        Assert.Equal(2, loaded.Count);
        Assert.Equal("Alpha.exe", loaded[0].TargetName); // oldest first, as written
        Assert.Equal("Beta.exe", loaded[1].TargetName);
        Assert.Equal("partial", loaded[1].Verdict);
        Assert.Equal(60, loaded[1].Multiplier);
        Assert.Equal(-120, loaded[1].TzBiasMin);
    }

    [Fact]
    public void Load_is_empty_when_no_file_exists()
        => Assert.Empty(new FileSessionHistoryStore(_dir).Load());

    [Fact]
    public void Load_is_empty_and_keeps_the_file_when_it_is_corrupt()
    {
        Directory.CreateDirectory(_dir);
        var path = Path.Combine(_dir, "sessions.json");
        File.WriteAllText(path, "{ this is not valid json");

        var loaded = new FileSessionHistoryStore(_dir).Load();

        Assert.Empty(loaded);
        Assert.True(File.Exists(path)); // a broken history is never deleted
    }

    [Fact]
    public void In_memory_store_appends_and_loads()
    {
        var store = new InMemorySessionHistoryStore();
        store.Append(Record("Gamma"));
        Assert.Equal("Gamma.exe", Assert.Single(store.Load()).TargetName);
    }
}
