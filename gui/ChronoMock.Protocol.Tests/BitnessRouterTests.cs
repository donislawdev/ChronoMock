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
    public void Portable_locator_routes_each_bitness_to_the_core_subdir()
    {
        // The shipped layout (Stage 5) puts the cores at <baseDir>/core/<arch>/chrono.exe. The base dir
        // need not exist - we assert only the mapping - so the built cores serve purely as x86/x64 PE
        // fixtures for the target-bitness read.
        var repo = RepoPaths.RepoRoot();
        var baseDir = Path.Combine(repo, "dist");
        var locator = CoreLocator.ForPortable(baseDir);

        Assert.Equal(
            Path.Combine(baseDir, "core", "x86", "chrono.exe"), locator.CoreForTarget(RepoPaths.X86Core(repo)));
        Assert.Equal(
            Path.Combine(baseDir, "core", "x64", "chrono.exe"), locator.CoreForTarget(RepoPaths.X64Core(repo)));
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
    public void Both_dev_core_paths_name_an_explicit_target_triple()
    {
        // R2-X6, found while fixing R2-X4: target/release/ is written ONLY by a build with no --target,
        // while the working rule is to build both triples explicitly - so that directory keeps whatever
        // someone last built without the flag. Measured on this checkout: it was a day old while the triple
        // directory was current, so every dev-GUI session ran a core nobody had just built. That is exactly
        // the trap R2-X3 sprang on the harness. CalcClient.ForRepo carries the same path and the same fix.
        var repo = RepoPaths.RepoRoot();
        var locator = CoreLocator.ForRepo(repo);

        Assert.Contains(
            "x86_64-pc-windows-msvc", locator.CoreForTarget(RepoPaths.X64Core(repo)), StringComparison.Ordinal);
        Assert.Contains(
            "i686-pc-windows-msvc", locator.CoreForTarget(RepoPaths.X86Core(repo)), StringComparison.Ordinal);
    }

    /// <summary>
    /// R2-X4. A .NET Framework AnyCPU executable declares IMAGE_FILE_MACHINE_I386 and starts 64-bit on
    /// 64-bit Windows, so routing by the file header alone sent the GUI to the x86 core for a target that
    /// came up x64 - the core's own gate then refused the session with a bitness mismatch. The fixtures are
    /// synthetic PE32 images with a real CLI header, so every flag combination is exercised without
    /// shipping a .NET Framework binary. Sources: ECMA-335 II.25.3.3 for the header, CorFlags for the flags.
    /// </summary>
    [Theory]
    // IL-only, neither 32-bit flag: AnyCPU - the loader picks the host bitness.
    [InlineData(0x1u, 5, true)]
    // Requires32Bit: pinned to 32-bit even on 64-bit Windows.
    [InlineData(0x1u | 0x2u, 5, false)]
    // "AnyCPU, 32-bit preferred" - how the flag pair is really encoded - runs 32-bit.
    [InlineData(0x1u | 0x2u | 0x2_0000u, 5, false)]
    // Mixed mode (IL plus native): really an x86 image.
    [InlineData(0x0u, 5, false)]
    // A legacy CLR header (minor runtime 0) is always executed under WOW64.
    [InlineData(0x1u, 0, false)]
    public void A_managed_32_bit_header_routes_by_what_it_will_actually_run_as(
        uint corFlags, int minorRuntime, bool anyCpu)
    {
        var expected = anyCpu && Environment.Is64BitOperatingSystem
            ? PeReader.Machine.X64
            : PeReader.Machine.X86;
        var path = WriteFixture(ManagedPe32(corFlags, minorRuntime));
        try
        {
            Assert.Equal(expected, PeReader.ReadMachine(path));
        }
        finally
        {
            File.Delete(path);
        }
    }

    [Fact]
    public void A_managed_image_whose_cli_header_points_nowhere_keeps_its_header_bitness()
    {
        // Defence in depth: an unreadable CLI header must leave the 32-bit header standing, never guess
        // upward. The directory says RVA 0x9000, which no section covers.
        var bytes = ManagedPe32(0x1u, 5);
        BitConverter.GetBytes(0x9000u).CopyTo(bytes, CliDirectoryOffset);
        var path = WriteFixture(bytes);
        try
        {
            Assert.Equal(PeReader.Machine.X86, PeReader.ReadMachine(path));
        }
        finally
        {
            File.Delete(path);
        }
    }

    // --- synthetic PE32 fixture ---------------------------------------------------------------
    //
    // MZ at 0, e_lfanew -> 0x80. PE signature, IMAGE_FILE_HEADER (i386, one section, 224-byte optional
    // header), a PE32 optional header with 16 data directories, one section mapping RVA 0x2000 to file
    // offset 0x400, and the CLI header at RVA 0x2008 (file 0x408).
    private const int PeOffset = 0x80;
    private const int OptionalOffset = PeOffset + 4 + 20;
    private const int CliDirectoryOffset = OptionalOffset + 96 + (14 * 8);
    private const int SectionOffset = OptionalOffset + 224;
    private const int CliOffset = 0x408;

    private static byte[] ManagedPe32(uint corFlags, int minorRuntime)
    {
        var b = new byte[0x600];
        b[0] = 0x4D; // 'M'
        b[1] = 0x5A; // 'Z'
        BitConverter.GetBytes(PeOffset).CopyTo(b, 0x3C);

        BitConverter.GetBytes(0x0000_4550u).CopyTo(b, PeOffset); // "PE\0\0"
        BitConverter.GetBytes((ushort)0x014C).CopyTo(b, PeOffset + 4);      // Machine = i386
        BitConverter.GetBytes((ushort)1).CopyTo(b, PeOffset + 4 + 2);       // NumberOfSections
        BitConverter.GetBytes((ushort)224).CopyTo(b, PeOffset + 4 + 16);    // SizeOfOptionalHeader

        BitConverter.GetBytes((ushort)0x010B).CopyTo(b, OptionalOffset);    // PE32 magic
        BitConverter.GetBytes(16u).CopyTo(b, OptionalOffset + 92);          // NumberOfRvaAndSizes
        BitConverter.GetBytes(0x2008u).CopyTo(b, CliDirectoryOffset);       // CLI header RVA
        BitConverter.GetBytes(72u).CopyTo(b, CliDirectoryOffset + 4);       // CLI header size

        BitConverter.GetBytes(0x1000u).CopyTo(b, SectionOffset + 8);        // VirtualSize
        BitConverter.GetBytes(0x2000u).CopyTo(b, SectionOffset + 12);       // VirtualAddress
        BitConverter.GetBytes(0x200u).CopyTo(b, SectionOffset + 16);        // SizeOfRawData
        BitConverter.GetBytes(0x400u).CopyTo(b, SectionOffset + 20);        // PointerToRawData

        BitConverter.GetBytes(72u).CopyTo(b, CliOffset);                          // cb
        BitConverter.GetBytes((ushort)2).CopyTo(b, CliOffset + 4);                // MajorRuntimeVersion
        BitConverter.GetBytes((ushort)minorRuntime).CopyTo(b, CliOffset + 6);     // MinorRuntimeVersion
        BitConverter.GetBytes(corFlags).CopyTo(b, CliOffset + 16);                // Flags
        return b;
    }

    private static string WriteFixture(byte[] bytes)
    {
        var path = Path.Combine(Path.GetTempPath(), $"chrono-pe-{Guid.NewGuid():N}.bin");
        File.WriteAllBytes(path, bytes);
        return path;
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
