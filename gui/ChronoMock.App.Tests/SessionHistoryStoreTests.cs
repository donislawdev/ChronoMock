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

    /// <summary>
    /// S-33 regression. The scratch file used one fixed name, so two portable instances writing at the
    /// same moment collided: one threw an IOException that surfaced as a history error and lost its entry.
    /// Concurrent appends must all complete, and leave no scratch files behind.
    /// </summary>
    [Fact]
    public async Task Concurrent_appends_do_not_collide_on_the_scratch_file()
    {
        Directory.CreateDirectory(_dir);
        var writers = Enumerable.Range(0, 8)
            .Select(i => Task.Run(() => new FileSessionHistoryStore(_dir).Append(Record($"app{i}"))))
            .ToArray();

        await Task.WhenAll(writers); // a collision would surface here as an IOException

        Assert.NotEmpty(new FileSessionHistoryStore(_dir).Load());
        Assert.Empty(Directory.GetFiles(_dir, "*.tmp"));
    }

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

    [Fact]
    public void Clear_empties_the_file_store()
    {
        var store = new FileSessionHistoryStore(_dir);
        store.Append(Record("Alpha"));
        store.Append(Record("Beta"));

        store.Clear();

        Assert.Empty(store.Load());
    }

    [Fact]
    public void Remove_deletes_the_matching_record()
    {
        var store = new FileSessionHistoryStore(_dir);
        var alpha = Record("Alpha");
        store.Append(alpha);
        store.Append(Record("Beta"));

        store.Remove(alpha);

        Assert.Equal("Beta.exe", Assert.Single(store.Load()).TargetName);
    }

    [Fact]
    public void Chooses_the_preferred_history_directory_when_it_is_writable()
        => Assert.Equal(
            @"X:\exe\history",
            FileSessionHistoryStore.ChooseWritableDir(@"X:\exe\history", @"Y:\user\history", _ => true));

    [Fact]
    public void Falls_back_to_the_per_user_directory_when_the_preferred_is_read_only()
        // A read-only medium (a USB stick, Program Files without admin) cannot hold the log next to the exe,
        // so history saves to a per-user location instead of failing every session.
        => Assert.Equal(
            @"Y:\user\history",
            FileSessionHistoryStore.ChooseWritableDir(@"X:\exe\history", @"Y:\user\history", _ => false));

    [Fact]
    public void Append_keeps_only_the_most_recent_maximum()
    {
        var store = new FileSessionHistoryStore(_dir);
        for (int i = 0; i < SessionHistoryLimits.Max + 3; i++)
        {
            store.Append(Record($"App{i}"));
        }

        var loaded = store.Load();

        Assert.Equal(SessionHistoryLimits.Max, loaded.Count);
        Assert.Equal("App3.exe", loaded[0].TargetName); // App0..App2 dropped as oldest
        Assert.Equal($"App{SessionHistoryLimits.Max + 2}.exe", loaded[^1].TargetName); // newest kept
    }
}
