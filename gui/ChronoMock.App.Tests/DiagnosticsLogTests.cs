using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using ChronoMock.App;

namespace ChronoMock.App.Tests;

/// <summary>
/// The diagnostics log (RELEASE-012): writes one timestamped file per problem session, prunes to a cap, and
/// falls back to a per-user directory when the exe folder is read-only - mirroring the history store.
/// </summary>
public sealed class DiagnosticsLogTests : IDisposable
{
    private readonly string _dir =
        Path.Combine(Path.GetTempPath(), "chrono-diag-tests", Guid.NewGuid().ToString("N"));

    public void Dispose()
    {
        if (Directory.Exists(_dir))
        {
            Directory.Delete(_dir, recursive: true);
        }
    }

    [Fact]
    public void Save_writes_the_block_and_returns_the_path()
    {
        var path = new FileDiagnosticsLog(_dir).Save("hello diagnostics");

        Assert.NotNull(path);
        Assert.True(File.Exists(path));
        Assert.Equal("hello diagnostics", File.ReadAllText(path));
        Assert.StartsWith("diagnostics-", Path.GetFileName(path), StringComparison.Ordinal);
        Assert.EndsWith(".log", path, StringComparison.Ordinal);
    }

    [Fact]
    public void Save_keeps_only_the_most_recent_maximum()
    {
        Directory.CreateDirectory(_dir);
        // Seed more than the cap with older-sorting names so pruning has something to drop; the just-saved
        // file (a 2026 timestamp) sorts after them and must survive.
        for (int i = 0; i < DiagnosticsLogLimits.Max + 2; i++)
        {
            File.WriteAllText(Path.Combine(_dir, $"diagnostics-{i:D6}.log"), "old");
        }

        var path = new FileDiagnosticsLog(_dir).Save("new block");

        var files = Directory.GetFiles(_dir, "diagnostics-*.log");
        Assert.Equal(DiagnosticsLogLimits.Max, files.Length);
        Assert.NotNull(path);
        Assert.True(File.Exists(path)); // the newest (just saved) is kept
    }

    [Fact]
    public void Chooses_the_preferred_logs_directory_when_it_is_writable()
        => Assert.Equal(
            @"X:\exe\logs",
            FileDiagnosticsLog.ChooseWritableDir(@"X:\exe\logs", @"Y:\user\logs", _ => true));

    [Fact]
    public void Falls_back_to_the_per_user_directory_when_the_preferred_is_read_only()
        // A read-only medium (a USB stick, Program Files without admin) cannot hold the log next to the exe,
        // so diagnostics save to a per-user location instead of being lost.
        => Assert.Equal(
            @"Y:\user\logs",
            FileDiagnosticsLog.ChooseWritableDir(@"X:\exe\logs", @"Y:\user\logs", _ => false));

    [Fact]
    public void The_no_op_log_saves_nothing()
        => Assert.Null(new NoOpDiagnosticsLog().Save("anything"));
}
