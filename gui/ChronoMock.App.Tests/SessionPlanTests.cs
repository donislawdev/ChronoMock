using System.IO;
using ChronoMock.App;
using ChronoMock.Protocol;

namespace ChronoMock.App.Tests;

/// <summary>
/// SessionPlan.Build's failure classification (RELEASE-007). The exception TYPE distinguishes a target that
/// is not a supported executable (a non-PE file, InvalidOperationException) from one that is missing or
/// unreadable (a file-access error). SessionViewModel.StartAsync maps each to its own status, so the panel
/// no longer reports every setup problem as a generic "core missing - build the solution first".
/// </summary>
public class SessionPlanTests
{
    private static TimeSpec AnyTime() => new()
    {
        Moment = new MomentSpec { Kind = "absolute", Local = "2038-01-19T03:14:07", TzBiasMin = 0 },
        Mode = "flow",
    };

    [Fact]
    public void A_non_PE_file_is_an_unsupported_executable()
    {
        var path = Path.Combine(Path.GetTempPath(), $"chrono-test-{Guid.NewGuid():N}.txt");
        File.WriteAllText(path, "not an executable");
        try
        {
            // Not a PE -> PeReader returns Unknown -> Build cannot determine the bitness. The panel reads
            // this as "not a supported executable", never as a broken core install.
            Assert.Throws<InvalidOperationException>(() => SessionPlan.Build(path, AnyTime()));
        }
        finally
        {
            File.Delete(path);
        }
    }

    [Fact]
    public void A_missing_target_is_a_file_access_error_not_unsupported()
    {
        var path = Path.Combine(Path.GetTempPath(), $"chrono-missing-{Guid.NewGuid():N}.exe");
        // A missing file propagates from PeReader as a file-access error (PeReader swallows only a malformed
        // PE, not an access failure), so the panel says "could not be read", not "not a supported executable".
        Assert.Throws<FileNotFoundException>(() => SessionPlan.Build(path, AnyTime()));
    }
}
