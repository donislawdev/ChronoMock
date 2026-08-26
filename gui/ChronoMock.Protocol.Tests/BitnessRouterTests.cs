using ChronoMock.Protocol;

namespace ChronoMock.Protocol.Tests;

/// <summary>
/// The bitness router (PeReader + CoreLocator) proven on BOTH bitnesses without injecting into anything:
/// the built x86 and x64 cores are themselves real x86 and x64 PE fixtures. Full x86 end-to-end through
/// the client is deferred (it needs an own x86 long-lived target); the x86 injection path itself is
/// already covered by the run-targets harness and the spike.
/// </summary>
public class BitnessRouterTests
{
    [Fact]
    public void Reads_the_x64_core_as_x64()
    {
        var repo = RepoPaths.RepoRoot();
        Assert.Equal(PeReader.Machine.X64, PeReader.ReadMachine(RepoPaths.X64Core(repo)));
    }

    [Fact]
    public void Reads_the_x86_core_as_x86()
    {
        var repo = RepoPaths.RepoRoot();
        Assert.Equal(PeReader.Machine.X86, PeReader.ReadMachine(RepoPaths.X86Core(repo)));
    }

    [Fact]
    public void Locator_routes_each_bitness_to_the_matching_core()
    {
        var repo = RepoPaths.RepoRoot();
        var locator = CoreLocator.ForRepo(repo);

        Assert.Equal(RepoPaths.X86Core(repo), locator.CoreForTarget(RepoPaths.X86Core(repo)));
        Assert.Equal(RepoPaths.X64Core(repo), locator.CoreForTarget(RepoPaths.X64Core(repo)));
    }

    [Fact]
    public void Non_pe_input_is_a_loud_error_not_a_guess()
    {
        var repo = RepoPaths.RepoRoot();
        var textFile = Path.Combine(repo, "Cargo.toml"); // a real file, not a PE
        var locator = CoreLocator.ForRepo(repo);
        Assert.Throws<InvalidOperationException>(() => locator.CoreForTarget(textFile));
    }

    [Fact]
    public void A_hostile_e_lfanew_is_unknown_not_a_crash()
    {
        // e_lfanew = 0x7FFFFFFF: `peOffset + 6` in 32-bit arithmetic overflows to a negative value and
        // used to slip past the bound check, then throw EndOfStreamException on the seek+read (H-3). A
        // malformed PE must resolve to Unknown, never a thrown exception.
        var bytes = new byte[0x40];
        bytes[0] = 0x4D; // 'M'
        bytes[1] = 0x5A; // 'Z'
        BitConverter.GetBytes(0x7FFFFFFF).CopyTo(bytes, 0x3C); // e_lfanew
        var path = Path.Combine(Path.GetTempPath(), $"chrono-pe-{Guid.NewGuid():N}.bin");
        File.WriteAllBytes(path, bytes);
        try
        {
            Assert.Equal(PeReader.Machine.Unknown, PeReader.ReadMachine(path));
        }
        finally
        {
            File.Delete(path);
        }
    }
}
