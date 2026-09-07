using System.Buffers.Binary;

namespace ChronoMock.Protocol.Tests;

/// <summary>
/// Reads a PE image's import directory - the list of DLLs the Windows loader must resolve before the
/// file can run a single instruction. <see cref="PeReader"/> reads the same file format and stops at the
/// machine type, because that is all the bitness router needs. This goes on to the import table, which
/// nothing shipped has any use for, so it lives in the test project rather than beside
/// <see cref="PeReader"/>: a public member in <c>ChronoMock.Protocol</c> that only a test calls is the
/// exact shape the hygiene ratchet refuses.
/// <para>
/// Every malformed input is a thrown <see cref="InvalidDataException"/> naming the file and what was
/// wrong with it. That is the opposite of <see cref="PeReader"/>'s contract, and deliberately: the router
/// is handed whatever executable a user picked and must answer "not a PE" calmly, while this is handed
/// our own build output and a file it cannot parse means the guard did not run.
/// </para>
/// </summary>
internal sealed class PeImportTable
{
    private const int ImportDirectory = 1;
    private const int BoundImportDirectory = 11;
    private const int DelayImportDirectory = 13;

    /// <summary>Size of one IMAGE_IMPORT_DESCRIPTOR, and the offset of its Name field within it.</summary>
    private const int DescriptorSize = 20;
    private const int NameField = 12;

    /// <summary>A descriptor list this long is a parse that has lost its way, not a real binary.</summary>
    private const int MaxModules = 512;

    private PeImportTable(
        string label,
        int dataDirectoryOffset,
        IReadOnlyList<ModuleImport> entries,
        DataDirectory boundImports,
        DataDirectory delayImports)
    {
        Label = label;
        DataDirectoryOffset = dataDirectoryOffset;
        Entries = entries;
        BoundImports = boundImports;
        DelayImports = delayImports;
    }

    /// <summary>One DLL named by the import directory, with the file offset its name is stored at.</summary>
    /// <remarks>
    /// The offset is what lets the canary rewrite a name inside a real binary and read it back through
    /// this same parser, instead of asserting against a hand-built fixture that proves only that the
    /// fixture matches the parser.
    /// </remarks>
    public readonly record struct ModuleImport(string Name, int NameOffset);

    /// <summary>One entry of the optional header's data directory: where a table is, and how big.</summary>
    public readonly record struct DataDirectory(uint Rva, uint Size)
    {
        public bool IsEmpty => Rva == 0 && Size == 0;
    }

    /// <summary>What this table was read from, for the message when something is wrong with it.</summary>
    public string Label { get; }

    /// <summary>File offset of data directory entry 0, so a canary can reach entry 11 or 13 by index.</summary>
    public int DataDirectoryOffset { get; }

    /// <summary>Every descriptor in file order, including a name repeated with different capitalisation.</summary>
    public IReadOnlyList<ModuleImport> Entries { get; }

    public DataDirectory BoundImports { get; }

    public DataDirectory DelayImports { get; }

    /// <summary>
    /// The distinct DLL names, compared without case. MSVC emits <c>kernel32.dll</c> and
    /// <c>KERNEL32.dll</c> as two descriptors in the same binary - measured in all six of ours - so a
    /// case-sensitive set would report one module as two and a register written from one build would
    /// disagree with the next.
    /// </summary>
    public IReadOnlyList<string> Modules => Entries
        .Select(e => e.Name)
        .Distinct(StringComparer.OrdinalIgnoreCase)
        .OrderBy(n => n, StringComparer.OrdinalIgnoreCase)
        .ToList();

    public static PeImportTable Read(string path) => Read(File.ReadAllBytes(path), Path.GetFileName(path));

    /// <summary>Parse an image already in memory, which is how the canary reads a deliberately broken copy.</summary>
    public static PeImportTable Read(byte[] image, string label)
    {
        var layout = ReadLayout(image, label);
        var sections = ReadSections(image, layout);
        var imports = ReadDirectory(image, layout, ImportDirectory, label);
        return new PeImportTable(
            label,
            layout.DataDirectory,
            ReadModules(image, sections, imports, label),
            ReadDirectory(image, layout, BoundImportDirectory, label),
            ReadDirectory(image, layout, DelayImportDirectory, label));
    }

    /// <summary>Whether this binary imports a module whose name contains <paramref name="fragment"/>.</summary>
    public bool ImportsAnythingNamed(string fragment) =>
        Modules.Any(m => m.Contains(fragment, StringComparison.OrdinalIgnoreCase));

    private static Layout ReadLayout(byte[] image, string label)
    {
        Require(image.Length >= 0x40, label, "shorter than a DOS header");
        Require(ReadU16(image, 0) == 0x5A4D, label, "no MZ signature - this is not a PE file");

        int pe = (int)ReadU32(image, 0x3C);
        Require(pe > 0 && (long)pe + 24 <= image.Length, label, "e_lfanew does not point into the file");
        Require(ReadU32(image, pe) == 0x0000_4550, label, "no PE signature at e_lfanew");

        int sectionCount = ReadU16(image, pe + 6);
        int optionalSize = ReadU16(image, pe + 20);
        int optional = pe + 24;
        long sectionTable = (long)optional + optionalSize;
        Require(
            sectionTable + ((long)sectionCount * 40) <= image.Length,
            label,
            "the section table runs past the end of the file");

        ushort magic = ReadU16(image, optional);
        Require(magic is 0x10B or 0x20B, label, $"unknown optional header magic 0x{magic:X4}");

        bool plus = magic == 0x20B;
        return new Layout(optional + (plus ? 112 : 96), (int)sectionTable, sectionCount);
    }

    private static List<Section> ReadSections(byte[] image, Layout layout)
    {
        var sections = new List<Section>(layout.SectionCount);
        for (var i = 0; i < layout.SectionCount; i++)
        {
            int header = layout.SectionTable + (i * 40);
            sections.Add(new Section(
                Rva: ReadU32(image, header + 12),
                VirtualSize: ReadU32(image, header + 8),
                RawOffset: ReadU32(image, header + 20),
                RawSize: ReadU32(image, header + 16)));
        }

        return sections;
    }

    /// <summary>One data directory entry, or an empty one when the optional header is too short to carry it.</summary>
    private static DataDirectory ReadDirectory(byte[] image, Layout layout, int index, string label)
    {
        int entry = layout.DataDirectory + (index * 8);
        // A short optional header simply carries no such directory - that is "empty", not "malformed",
        // so this is the one read here that answers with a value instead of throwing.
        Require(entry > 0, label, "the data directory is not inside the file");
        return (long)entry + 8 > image.Length
            ? default
            : new DataDirectory(ReadU32(image, entry), ReadU32(image, entry + 4));
    }

    private static List<ModuleImport> ReadModules(
        byte[] image, IReadOnlyList<Section> sections, DataDirectory imports, string label)
    {
        var entries = new List<ModuleImport>();
        if (imports.Rva == 0)
        {
            return entries;
        }

        int offset = RvaToOffset(sections, imports.Rva);
        Require(offset >= 0, label, "the import directory is not inside any section");

        while (entries.Count < MaxModules)
        {
            Require(
                (long)offset + DescriptorSize <= image.Length,
                label,
                "an import descriptor runs past the end of the file");
            if (IsTerminator(image, offset))
            {
                return entries;
            }

            uint nameRva = ReadU32(image, offset + NameField);
            int nameOffset = RvaToOffset(sections, nameRva);
            Require(nameOffset >= 0, label, $"a module name at RVA 0x{nameRva:X} is outside every section");
            entries.Add(new ModuleImport(ReadCString(image, nameOffset, label), nameOffset));
            offset += DescriptorSize;
        }

        throw new InvalidDataException($"{label}: more than {MaxModules} import descriptors - this is not a parse");
    }

    /// <summary>The import directory ends at an all-zero descriptor, which is the only terminator there is.</summary>
    private static bool IsTerminator(byte[] image, int offset) =>
        image.AsSpan(offset, DescriptorSize).IndexOfAnyExcept((byte)0) < 0;

    private static int RvaToOffset(IReadOnlyList<Section> sections, uint rva)
    {
        foreach (var section in sections)
        {
            // Widths in long, not uint: a section header carrying 0xFFFFFFFF would otherwise wrap and
            // claim every RVA in the file.
            long span = Math.Max(section.VirtualSize, section.RawSize);
            if (rva >= section.Rva && rva < section.Rva + span)
            {
                return (int)(section.RawOffset + (rva - section.Rva));
            }
        }

        return -1;
    }

    private static string ReadCString(byte[] image, int offset, string label)
    {
        int end = Array.IndexOf(image, (byte)0, offset);
        Require(end >= 0, label, "a module name is not terminated before the end of the file");
        return System.Text.Encoding.ASCII.GetString(image, offset, end - offset);
    }

    private static ushort ReadU16(byte[] image, int offset) =>
        BinaryPrimitives.ReadUInt16LittleEndian(image.AsSpan(offset));

    private static uint ReadU32(byte[] image, int offset) =>
        BinaryPrimitives.ReadUInt32LittleEndian(image.AsSpan(offset));

    private static void Require(bool condition, string label, string what)
    {
        if (!condition)
        {
            throw new InvalidDataException($"{label}: {what}");
        }
    }

    private readonly record struct Layout(int DataDirectory, int SectionTable, int SectionCount);

    private readonly record struct Section(uint Rva, uint VirtualSize, uint RawOffset, uint RawSize);
}
