namespace ChronoMock.Protocol;

/// <summary>
/// Reads a PE executable's machine type so the client can launch the matching-bitness core
/// (ADR-6: the core matches the target's bitness, the GUI itself stays AnyCPU). Stage 3.1 handles
/// the <c>.exe</c> case; resolving <c>.lnk</c>/<c>.bat</c>/<c>.msi</c> to a bitness is a later UI slice (9.6).
/// </summary>
public static class PeReader
{
    public enum Machine
    {
        Unknown,
        X86,
        X64,
    }

    /// <summary>
    /// Read the bitness a PE file will actually RUN at. Returns <see cref="Machine.Unknown"/> for anything
    /// that is not a well-formed PE - the caller turns that into a loud error, never a guessed default.
    /// <para>
    /// <c>IMAGE_FILE_HEADER.Machine</c> alone is not that answer (R2-X4). A .NET Framework AnyCPU
    /// executable carries <c>IMAGE_FILE_MACHINE_I386</c> and starts 64-bit on 64-bit Windows, so reading the
    /// header sent the GUI to the x86 core for a target that came up x64. The core's own bitness gate
    /// (R2-S1, <c>IsWow64Process2</c> on the suspended process) catches that, which is why this reads as
    /// "the tool refuses with a bitness mismatch" rather than something worse - but a second line of
    /// defence is not a reason to aim wrong. For a 32-bit header we therefore read the CLI header's runtime
    /// flags and answer with the bitness the loader will pick.
    /// </para>
    /// </summary>
    public static Machine ReadMachine(string exePath)
    {
        try
        {
            using var stream = File.OpenRead(exePath);
            using var reader = new BinaryReader(stream);

            if (stream.Length < 0x40)
            {
                return Machine.Unknown;
            }

            // DOS header: 'MZ' magic at 0, e_lfanew (PE header offset) at 0x3C.
            if (reader.ReadUInt16() != 0x5A4D)
            {
                return Machine.Unknown;
            }

            stream.Seek(0x3C, SeekOrigin.Begin);
            int peOffset = reader.ReadInt32();
            // Compute the bound in long: peOffset is read straight from the file, so `peOffset + 6` in
            // 32-bit arithmetic overflows to a NEGATIVE value for a hostile e_lfanew near int.MaxValue,
            // slipping past this guard and throwing EndOfStreamException on the seek+read below (H-3).
            if (peOffset <= 0 || (long)peOffset + 6 > stream.Length)
            {
                return Machine.Unknown;
            }

            // PE signature "PE\0\0", then IMAGE_FILE_HEADER.Machine (u16, little-endian).
            stream.Seek(peOffset, SeekOrigin.Begin);
            if (reader.ReadUInt32() != 0x0000_4550)
            {
                return Machine.Unknown;
            }

            var machine = reader.ReadUInt16() switch
            {
                0x8664 => Machine.X64, // IMAGE_FILE_MACHINE_AMD64
                0x014C => Machine.X86, // IMAGE_FILE_MACHINE_I386
                _ => Machine.Unknown,
            };

            // A 32-bit header is not a 32-bit process for a managed AnyCPU image - ask the CLI header.
            return machine == Machine.X86 && RunsAtHostBitness(stream, reader, peOffset) ? HostMachine : machine;
        }
        catch (EndOfStreamException)
        {
            // A truncated or malformed PE is Unknown, per the contract - never a thrown exception (H-3
            // safety net). A file-ACCESS failure (UnauthorizedAccessException, other IOException) is not
            // caught here: it propagates so the caller reports it as an access error, not "not a PE".
            return Machine.Unknown;
        }
    }

    /// <summary>The bitness an AnyCPU image starts at here - the operating system's, not this process's.</summary>
    private static Machine HostMachine => Environment.Is64BitOperatingSystem ? Machine.X64 : Machine.X86;

    /// <summary>
    /// Whether a 32-bit-header image is a managed AnyCPU one, which the loader starts at the host's bitness.
    /// Sources (rule 20): the CLI header layout is ECMA-335 II.25.3.3, the runtime flags are documented as
    /// <c>CorFlags</c> (ILOnly 0x1, Requires32Bit 0x2, Prefers32Bit 0x20000 - "run as a 32-bit process on a
    /// 64-bit operating system"), and MS documents that an assembly whose CLR header minor runtime version
    /// is 0 is a legacy image "always executed under WOW64".
    /// <para>
    /// Anything unreadable answers false, which leaves the header's own x86 standing: this only ever
    /// UPGRADES a 32-bit header to the host bitness on evidence, never downgrades or guesses.
    /// </para>
    /// </summary>
    private static bool RunsAtHostBitness(Stream stream, BinaryReader reader, int peOffset)
    {
        // IMAGE_FILE_HEADER sits right after the 4-byte PE signature: NumberOfSections at +2,
        // SizeOfOptionalHeader at +16, the optional header at +20.
        long fileHeader = (long)peOffset + 4;
        if (fileHeader + 20 > stream.Length)
        {
            return false;
        }

        stream.Seek(fileHeader + 2, SeekOrigin.Begin);
        int sections = reader.ReadUInt16();
        stream.Seek(fileHeader + 16, SeekOrigin.Begin);
        int optionalSize = reader.ReadUInt16();

        long optional = fileHeader + 20;
        // 96 bytes is where a PE32 optional header's data directories begin; a shorter one carries none.
        if (optionalSize < 96 || optional + optionalSize > stream.Length)
        {
            return false;
        }

        stream.Seek(optional, SeekOrigin.Begin);
        if (reader.ReadUInt16() != 0x010B)
        {
            return false; // not PE32 - a PE32+ image never carries a 32-bit machine anyway
        }

        stream.Seek(optional + 92, SeekOrigin.Begin); // NumberOfRvaAndSizes
        if (reader.ReadUInt32() < 15)
        {
            return false; // no CLI directory at index 14 - a native x86 image
        }

        stream.Seek(optional + 96 + (14 * 8), SeekOrigin.Begin);
        uint cliRva = reader.ReadUInt32();
        uint cliSize = reader.ReadUInt32();
        if (cliRva == 0 || cliSize < 20)
        {
            return false; // unmanaged, or too short to hold Flags at +16
        }

        long cli = ToFileOffset(stream, reader, optional + optionalSize, sections, cliRva);
        if (cli < 0 || cli + 20 > stream.Length)
        {
            return false;
        }

        stream.Seek(cli + 6, SeekOrigin.Begin); // MinorRuntimeVersion
        int minorRuntime = reader.ReadUInt16();
        stream.Seek(cli + 16, SeekOrigin.Begin); // Flags
        uint flags = reader.ReadUInt32();

        const uint ilOnly = 0x1;
        const uint requires32Bit = 0x2;
        const uint prefers32Bit = 0x2_0000;

        // Mixed-mode (IL plus native code) really is x86, and either 32-bit flag pins the process to 32-bit
        // even on 64-bit Windows. What is left - IL-only, neither flag, a non-legacy header - is AnyCPU.
        return (flags & ilOnly) != 0
               && (flags & (requires32Bit | prefers32Bit)) == 0
               && minorRuntime != 0;
    }

    /// <summary>Map a relative virtual address to a file offset through the section table, or -1.</summary>
    private static long ToFileOffset(
        Stream stream, BinaryReader reader, long sectionTable, int sections, uint rva)
    {
        for (var i = 0; i < sections; i++)
        {
            long header = sectionTable + ((long)i * 40);
            if (header + 40 > stream.Length)
            {
                return -1;
            }

            stream.Seek(header + 8, SeekOrigin.Begin);
            uint virtualSize = reader.ReadUInt32();
            uint virtualAddress = reader.ReadUInt32();
            uint rawSize = reader.ReadUInt32();
            uint rawPointer = reader.ReadUInt32();

            long span = Math.Max(virtualSize, rawSize);
            if (rva >= virtualAddress && rva < virtualAddress + span)
            {
                return rawPointer + (rva - virtualAddress);
            }
        }

        return -1;
    }
}
