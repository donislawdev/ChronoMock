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
    /// Read <c>IMAGE_FILE_HEADER.Machine</c> of a PE file. Returns <see cref="Machine.Unknown"/> for
    /// anything that is not a well-formed PE - the caller turns that into a loud error, never a guessed default.
    /// </summary>
    public static Machine ReadMachine(string exePath)
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
        if (peOffset <= 0 || peOffset + 6 > stream.Length)
        {
            return Machine.Unknown;
        }

        // PE signature "PE\0\0", then IMAGE_FILE_HEADER.Machine (u16, little-endian).
        stream.Seek(peOffset, SeekOrigin.Begin);
        if (reader.ReadUInt32() != 0x0000_4550)
        {
            return Machine.Unknown;
        }

        return reader.ReadUInt16() switch
        {
            0x8664 => Machine.X64, // IMAGE_FILE_MACHINE_AMD64
            0x014C => Machine.X86, // IMAGE_FILE_MACHINE_I386
            _ => Machine.Unknown,
        };
    }
}
