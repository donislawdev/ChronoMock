//! A bounded, read-only look inside the target's own executable, for the facts the file names around
//! it cannot give: whether the Go toolchain linked it, and whether it carries the .NET runtime inside
//! itself rather than in DLLs beside it.
//!
//! Every read is capped and every offset goes through `get`, so a truncated or hostile file answers
//! "no" rather than panicking, and a half-gigabyte target costs a few kilobytes to ask. The file is
//! chosen by whoever runs the tool. The direction of error is deliberate: a missed fingerprint costs
//! a caution that was not raised, while a false one accuses a target of something it does not do, and
//! an audit that invents a gap is no better than one that hides it (untouchable rule 4).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// How much of the PE headers to read. The section table sits right behind the optional header, and
/// four kilobytes covers it with room to spare on every binary we have measured.
const HEADER_WINDOW: u64 = 4096;

/// The size of one section header, and of one data directory entry, as the PE format fixes them.
const SECTION_HEADER: usize = 40;
const DATA_DIRECTORY_ENTRY: usize = 8;

/// The export directory is entry zero of the data directory table.
const EXPORT_DIRECTORY: usize = 0;

/// How much of the export data to read. The directory's own size field covers the directory, the
/// three tables behind it and the name strings, because the linker lays them out as one block (the
/// loader relies on that range to tell a forwarder from code). Measured on .NET 10 builds: 88 bytes
/// for a NativeAOT executable, 1 652 for a self-contained single-file one with 44 exports. Sixty-four
/// kilobytes is forty times the larger of the two, and a name past it simply does not match.
const EXPORT_WINDOW: usize = 64 * 1024;

/// The magic the Go toolchain stamps on its build information, bytes as the linker writes them.
/// Matched whole rather than by the readable tail alone, so a binary that merely MENTIONS the
/// phrase (this file, for one) is not mistaken for one the Go linker produced.
const GO_BUILDINFO_MAGIC: &[u8] = b"\xff Go buildinf:";

/// The whole header the Go linker writes: the magic, then version-dependent fields. Go's own
/// `debug/buildinfo` puts that header at 32 bytes.
///
/// We require it to be COMPLETE and deliberately do not READ the fields. Parsing them would trade a
/// yes-or-no answer for a version-dependent one - the layout changed with Go 1.18 - and buy nothing,
/// because the fourteen magic bytes already identify the linker that wrote them. Requiring the full
/// header costs nothing and refuses a magic sitting at the very tail of what we read.
const GO_BUILDINFO_HEADER: usize = 32;

/// How far into `.data` to look. Bounded on purpose: the alternative is reading a half-gigabyte
/// target to answer one yes-or-no.
///
/// Measured on FOUR binaries - x64 and x86, stripped and not, one with a four-megabyte global array -
/// and the blob sits at offset ZERO of the section in every one, with `.data` itself between 36 and
/// 51 KiB. So this window reads the whole section on everything measured. (An earlier comment here
/// said the blob starts TWO bytes in. That was wrong and it was mine: I had searched for the
/// readable `Go buildinf:`, which begins two bytes inside the fourteen-byte magic.)
const GO_DATA_WINDOW: usize = 64 * 1024;

/// Export names that only a .NET runtime linked INTO the executable puts there. Read from the
/// runtime's own build files, not remembered:
///
/// - NativeAOT exports `DotNetRuntimeDebugHeader` from .NET 7 to .NET 10, and its development branch
///   has already replaced it with `DotNetRuntimeContractDescriptor`. Both are added by
///   `Microsoft.NETCore.Native.targets` only while `DebuggerSupport` is on, so an application built
///   with it off exports neither and stays unrecognised - a miss, which is the safe direction.
/// - The self-contained single-file host exports `DotNetRuntimeInfo` from .NET 6 onwards and
///   `DotNetRuntimeContractDescriptor` from .NET 9, unconditionally (`singlefilehost.def`).
///
/// Measured on .NET 10 builds of our own probe: the NativeAOT executable exports exactly one name,
/// the first above, and the single-file one exports 44 including the other two. All three names are
/// kept, because a list that only knew today's would go quiet on the next release.
///
/// What this does NOT reach: a framework-dependent single-file executable exports nothing at all
/// (measured), and a .NET Framework executable carries no runtime of its own.
const DOTNET_RUNTIME_EXPORTS: &[&[u8]] = &[
    b"DotNetRuntimeDebugHeader",
    b"DotNetRuntimeContractDescriptor",
    b"DotNetRuntimeInfo",
];

/// Whether this executable was produced by the Go toolchain.
///
/// 🔴 Why it is worth knowing: Go does not call a time export we can detour. Measured against the
/// toolchain's own source, `runtime/time_windows.h` defines `_SYSTEM_TIME 0x7ffe0014` and
/// `time·now` reads it with a plain `MOVQ` - the same shared page our Stage 0 negative-control probe
/// reads to prove a clock we did NOT substitute. A Go target therefore runs on the real date while
/// the session reports `works`, because the hooks all installed and Go calls some of them for other
/// things (the time zone). This is the detection that lets the audit say so.
///
/// Leans to silence on every doubt: an unreadable file, a shape that is not a PE, no `.data`, a
/// short read - all answer "not Go".
pub(crate) fn is_go_binary(target_path: &Path) -> bool {
    go_buildinfo_in_data(target_path).unwrap_or(false)
}

/// Whether this executable carries the .NET runtime inside itself: NativeAOT, or a self-contained
/// single-file build. Neither leaves `coreclr.dll` or a `.deps.json` beside the executable, which is
/// all the file-name fingerprint can see, so without this such a target got no .NET caution at all.
pub(crate) fn embeds_dotnet_runtime(target_path: &Path) -> bool {
    PeFile::open(target_path)
        .and_then(|mut pe| pe.exports_any(DOTNET_RUNTIME_EXPORTS))
        .unwrap_or(false)
}

/// The walk `is_go_binary` wraps: the `.data` section header, then a window at its start.
fn go_buildinfo_in_data(target_path: &Path) -> Option<bool> {
    let mut pe = PeFile::open(target_path)?;
    let Some(data) = pe.section(b".data\0\0\0") else {
        return Some(false);
    };
    let bytes = pe.read_at(u64::from(data.raw_offset), (data.raw_size as usize).min(GO_DATA_WINDOW))?;
    let at = bytes.windows(GO_BUILDINFO_MAGIC.len()).position(|w| w == GO_BUILDINFO_MAGIC);
    Some(matches!(at, Some(start) if bytes.len() - start >= GO_BUILDINFO_HEADER))
}

/// One section header, the fields this module reads.
struct Section {
    name: [u8; 8],
    virtual_address: u32,
    raw_size: u32,
    raw_offset: u32,
}

/// The first four kilobytes of a PE file, checked for the two signatures, plus the open handle so the
/// sections and directories they describe can be read.
struct PeFile {
    file: File,
    head: Vec<u8>,
    optional: usize,
    optional_size: usize,
    section_count: usize,
}

impl PeFile {
    fn open(path: &Path) -> Option<Self> {
        let mut file = File::open(path).ok()?;
        let mut head: Vec<u8> = Vec::new();
        // `take` + `read_to_end` rather than one `read`: a single read may return fewer bytes than
        // asked for, and a short header would send the section-table offsets somewhere arbitrary.
        file.by_ref().take(HEADER_WINDOW).read_to_end(&mut head).ok()?;
        if head.get(..2)? != b"MZ" {
            return None;
        }
        let pe = u32_at(&head, 0x3C)? as usize;
        if head.get(pe..pe.checked_add(4)?)? != b"PE\0\0" {
            return None;
        }
        let section_count = u16_at(&head, pe.checked_add(6)?)? as usize;
        let optional_size = u16_at(&head, pe.checked_add(20)?)? as usize;
        let optional = pe.checked_add(24)?;
        Some(Self {
            file,
            head,
            optional,
            optional_size,
            section_count,
        })
    }

    /// The section headers in table order, up to the first one the header window does not hold.
    fn sections(&self) -> impl Iterator<Item = Section> + '_ {
        let table = self.optional.saturating_add(self.optional_size);
        (0..self.section_count).map_while(move |index| {
            let entry = table.checked_add(index.checked_mul(SECTION_HEADER)?)?;
            Some(Section {
                name: self.head.get(entry..entry.checked_add(8)?)?.try_into().ok()?,
                virtual_address: u32_at(&self.head, entry.checked_add(12)?)?,
                raw_size: u32_at(&self.head, entry.checked_add(16)?)?,
                raw_offset: u32_at(&self.head, entry.checked_add(20)?)?,
            })
        })
    }

    /// The first section with this name. A name shorter than eight bytes is zero-padded, which is why
    /// the caller passes the padding too.
    fn section(&self, name: &[u8; 8]) -> Option<Section> {
        self.sections().find(|section| &section.name == name)
    }

    /// Where a data directory points, as (RVA, size), or `None` when the file has no such entry.
    ///
    /// PE32 (x86) and PE32+ (x64) differ here only in where the table starts: the image base and the
    /// four stack and heap sizes before it are eight bytes wide in PE32+. Any other magic is a shape
    /// this module does not know, so it has no directories.
    fn directory(&self, index: usize) -> Option<(u32, u32)> {
        let (count_at, table_at) = match u16_at(&self.head, self.optional)? {
            0x10B => (92, 96),
            0x20B => (108, 112),
            _ => return None,
        };
        if index >= u32_at(&self.head, self.optional.checked_add(count_at)?)? as usize {
            return None;
        }
        let entry = self
            .optional
            .checked_add(table_at)?
            .checked_add(index.checked_mul(DATA_DIRECTORY_ENTRY)?)?;
        // The table lives inside the optional header, so an entry past its declared size is not one.
        if entry.checked_add(DATA_DIRECTORY_ENTRY)? > self.optional.checked_add(self.optional_size)? {
            return None;
        }
        Some((u32_at(&self.head, entry)?, u32_at(&self.head, entry.checked_add(4)?)?))
    }

    /// The file offset behind a relative virtual address, through the section that holds it. Only
    /// bytes that are really in the file count: the zero-filled tail a section has in memory has no
    /// place on disk to read.
    fn offset_of(&self, rva: u32) -> Option<u64> {
        self.sections().find_map(|section| {
            let delta = rva.checked_sub(section.virtual_address)?;
            (delta < section.raw_size).then(|| u64::from(section.raw_offset) + u64::from(delta))
        })
    }

    fn read_at(&mut self, offset: u64, limit: usize) -> Option<Vec<u8>> {
        let mut bytes: Vec<u8> = Vec::new();
        self.file.seek(SeekFrom::Start(offset)).ok()?;
        self.file.by_ref().take(limit as u64).read_to_end(&mut bytes).ok()?;
        Some(bytes)
    }

    /// Whether the export name table holds any of `wanted`, compared whole and case-sensitively, the
    /// way the loader compares them. One read of the export block, then everything is resolved inside
    /// it: a name pointing outside the block does not match, and a pointer table running out of it
    /// ends the search.
    fn exports_any(&mut self, wanted: &[&[u8]]) -> Option<bool> {
        let Some((rva, size)) = self.directory(EXPORT_DIRECTORY).filter(|&(rva, _)| rva != 0) else {
            return Some(false);
        };
        let at = self.offset_of(rva)?;
        let block = self.read_at(at, (size as usize).min(EXPORT_WINDOW))?;
        let count = u32_at(&block, 24)? as usize;
        let names = u32_at(&block, 32)?.checked_sub(rva)? as usize;
        for index in 0..count {
            let pointer = u32_at(&block, names.checked_add(index.checked_mul(4)?)?)?;
            let name = pointer.checked_sub(rva).and_then(|start| c_string(&block, start as usize));
            if name.is_some_and(|name| wanted.contains(&name)) {
                return Some(true);
            }
        }
        Some(false)
    }
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at.checked_add(2)?)?.try_into().ok()?))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

/// The zero-terminated string starting at `start`, without its terminator. A string whose terminator
/// is not inside `bytes` is not a string we can vouch for.
fn c_string(bytes: &[u8], start: usize) -> Option<&[u8]> {
    let tail = bytes.get(start..)?;
    let end = tail.iter().position(|&b| b == 0)?;
    Some(&tail[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Where the synthetic images below put their one section, in memory and on disk.
    const RVA: u32 = 0x1000;
    const RAW: usize = 0x400;

    /// A PE just real enough to be walked: DOS stub with the offset at 0x3C, the signature, a COFF
    /// header naming one section, an optional header of the requested kind, and a section whose raw
    /// bytes are ours to fill. `export` points data directory zero at a range of that section.
    ///
    /// Built here rather than pointing at a Go or .NET binary on this machine, and that is the whole
    /// point: the probe binaries live outside the repository, so a test that read one would pass here
    /// and fail on every clean runner - the exact shape that kept CI red for four pushes once already.
    fn synthetic_pe(magic: u16, section_name: &[u8; 8], data: &[u8], export: Option<(u32, u32)>) -> Vec<u8> {
        let pe_at: usize = 0x80;
        let optional_size: usize = if magic == 0x20B { 240 } else { 224 };
        let (count_at, table_at) = if magic == 0x20B { (108, 112) } else { (92, 96) };
        let optional = pe_at + 24;
        let table = optional + optional_size;
        let mut bytes = vec![0u8; RAW + data.len().max(1)];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[0x3C..0x40].copy_from_slice(&(pe_at as u32).to_le_bytes());
        bytes[pe_at..pe_at + 4].copy_from_slice(b"PE\0\0");
        bytes[pe_at + 6..pe_at + 8].copy_from_slice(&1u16.to_le_bytes());
        bytes[pe_at + 20..pe_at + 22].copy_from_slice(&(optional_size as u16).to_le_bytes());
        bytes[optional..optional + 2].copy_from_slice(&magic.to_le_bytes());
        bytes[optional + count_at..optional + count_at + 4].copy_from_slice(&16u32.to_le_bytes());
        if let Some((rva, size)) = export {
            bytes[optional + table_at..optional + table_at + 4].copy_from_slice(&rva.to_le_bytes());
            bytes[optional + table_at + 4..optional + table_at + 8].copy_from_slice(&size.to_le_bytes());
        }
        bytes[table..table + 8].copy_from_slice(section_name);
        bytes[table + 8..table + 12].copy_from_slice(&(data.len() as u32).to_le_bytes());
        bytes[table + 12..table + 16].copy_from_slice(&RVA.to_le_bytes());
        bytes[table + 16..table + 20].copy_from_slice(&(data.len() as u32).to_le_bytes());
        bytes[table + 20..table + 24].copy_from_slice(&(RAW as u32).to_le_bytes());
        bytes[RAW..RAW + data.len()].copy_from_slice(data);
        bytes
    }

    /// The export block a linker writes, starting at the section's first byte: the forty-byte
    /// directory, the name pointer table, then the names themselves. Only the two fields the reader
    /// uses are filled in, which is also a check that it needs no others.
    fn export_block(names: &[&[u8]]) -> Vec<u8> {
        let pointers = 40usize;
        let mut strings = pointers + 4 * names.len();
        let mut block = vec![0u8; strings];
        block[24..28].copy_from_slice(&(names.len() as u32).to_le_bytes());
        block[32..36].copy_from_slice(&(RVA + pointers as u32).to_le_bytes());
        for (index, name) in names.iter().enumerate() {
            let at = pointers + 4 * index;
            block[at..at + 4].copy_from_slice(&(RVA + strings as u32).to_le_bytes());
            block.extend_from_slice(name);
            block.push(0);
            strings += name.len() + 1;
        }
        block
    }

    /// An executable exporting `names`, in the requested PE flavour, with the directory's size field
    /// telling the truth about the block.
    fn exporting(magic: u16, names: &[&[u8]]) -> Vec<u8> {
        let block = export_block(names);
        synthetic_pe(magic, b".rdata\0\0", &block, Some((RVA, block.len() as u32)))
    }

    fn write_probe(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("chrono-pe-fingerprint-{name}.bin"));
        std::fs::write(&path, bytes).expect("probe file");
        path
    }

    /// A Go `.data` blob: the magic, the pointer-size and flags bytes, then the rest of the
    /// thirty-two, then whatever the version fields hold. The first draft of this fixture stopped at
    /// twenty-four bytes and the completeness check rejected it - which is the check earning its place
    /// rather than a fixture being awkward.
    fn go_blob() -> Vec<u8> {
        let mut blob = GO_BUILDINFO_MAGIC.to_vec();
        blob.extend_from_slice(&[0x08, 0x02]);
        blob.extend_from_slice(&[0u8; 16]);
        blob.extend_from_slice(b"go1.27.0");
        blob
    }

    /// The reversal probe for the Go fingerprint, in BOTH directions. A detector that only ever says
    /// yes accuses every target, and one that only ever says no is decoration - neither is caught by
    /// a test that checks a single case.
    #[test]
    fn the_go_fingerprint_answers_on_the_magic_and_not_on_the_section_alone() {
        let go = write_probe("go", &synthetic_pe(0, b".data\0\0\0", &go_blob(), None));
        assert!(is_go_binary(&go), "the magic sits in .data and this is what Go leaves there");

        // Same section, same size, no magic: a perfectly ordinary binary must not be accused.
        let plain = b"ordinary bytes, no Go linker anywhere";
        let plain = write_probe("plain", &synthetic_pe(0, b".data\0\0\0", plain, None));
        assert!(!is_go_binary(&plain), "a .data section is not evidence - the magic is");

        // The magic present but somewhere we do not look. Go puts it at the start of .data, and this
        // pins that we read the SECTION rather than trusting any occurrence anywhere in the file.
        let neighbour = write_probe("neighbour", &synthetic_pe(0, b".rdata\0\0", &go_blob(), None));
        assert!(!is_go_binary(&neighbour), "read .data, not whatever section happens to carry the bytes");

        // The magic with nothing behind it. Go's header is 32 bytes and the linker never writes a
        // truncated one, so this can only be a coincidence sitting at the tail of what we read -
        // and a coincidence is not evidence.
        let tail = write_probe("tail", &synthetic_pe(0, b".data\0\0\0", GO_BUILDINFO_MAGIC, None));
        assert!(!is_go_binary(&tail), "fourteen bytes with no header behind them prove nothing");

        for p in [go, plain, neighbour, tail] {
            let _ = std::fs::remove_file(p);
        }
    }

    /// Every doubt answers "not Go". A target we cannot read must not be accused of a gap it may not
    /// have - an audit that invents one is no better than an audit that hides one (rule 4).
    #[test]
    fn an_unreadable_or_malformed_target_is_never_called_a_go_binary() {
        assert!(!is_go_binary(Path::new("no such file anywhere.exe")));
        let truncated = write_probe("truncated", b"MZ");
        assert!(!is_go_binary(&truncated), "a two-byte file is not a PE");
        let not_pe = write_probe("not_pe", &vec![0u8; 8192]);
        assert!(!is_go_binary(&not_pe), "zeroes are not a PE either");
        let _ = std::fs::remove_file(truncated);
        let _ = std::fs::remove_file(not_pe);
    }

    /// The reversal probe for the embedded-runtime fingerprint, in both directions and in both PE
    /// flavours. PE32 matters as much as PE32+: the x86 core runs x86 targets, and the two put the
    /// directory table sixteen bytes apart, so a reader that only knew one would be blind in the other
    /// while every x64 test stayed green.
    #[test]
    fn an_embedded_dotnet_runtime_is_recognised_by_its_export_and_only_by_it() {
        for (flavour, magic) in [("pe32plus", 0x20Bu16), ("pe32", 0x10B)] {
            for &name in DOTNET_RUNTIME_EXPORTS {
                let alone = write_probe(&format!("{flavour}-alone"), &exporting(magic, &[name]));
                assert!(embeds_dotnet_runtime(&alone), "{flavour}: {} alone", String::from_utf8_lossy(name));
                let _ = std::fs::remove_file(alone);
            }

            // Among others, the way the single-file host carries it: a table of names in which ours
            // is neither first nor last.
            let crowd = exporting(magic, &[b"BrotliDecoderCreateInstance", b"DotNetRuntimeInfo", b"g_dacTable"]);
            let crowd = write_probe(&format!("{flavour}-crowd"), &crowd);
            assert!(embeds_dotnet_runtime(&crowd), "{flavour}: found in the middle of the table");

            // A different export, a prefix of ours, and ours with different case: none is the runtime.
            let other = exporting(magic, &[b"DotNetRuntime", b"dotnetruntimeinfo", b"DotNetRuntimeInfoX"]);
            let other = write_probe(&format!("{flavour}-other"), &other);
            assert!(!embeds_dotnet_runtime(&other), "{flavour}: a near miss is not a match");

            // No export directory at all, which is what a plain native executable and a
            // framework-dependent single-file one both look like.
            let none = synthetic_pe(magic, b".rdata\0\0", &export_block(&[b"DotNetRuntimeInfo"]), None);
            let none = write_probe(&format!("{flavour}-none"), &none);
            assert!(!embeds_dotnet_runtime(&none), "{flavour}: names without a directory pointing at them");

            for p in [crowd, other, none] {
                let _ = std::fs::remove_file(p);
            }
        }
    }

    /// Every doubt answers "no": a directory pointing outside every section, a size field too small to
    /// hold the directory, a name whose terminator is cut off, a truncated file, an unknown magic.
    #[test]
    fn a_damaged_export_table_is_never_read_as_a_dotnet_runtime() {
        let good = exporting(0x20B, &[b"DotNetRuntimeInfo"]);

        let mut nowhere = good.clone();
        let table_at = 0x80 + 24 + 112;
        nowhere[table_at..table_at + 4].copy_from_slice(&0x9000_0000u32.to_le_bytes());
        let nowhere = write_probe("nowhere", &nowhere);
        assert!(!embeds_dotnet_runtime(&nowhere), "a directory no section holds");

        let mut short = good.clone();
        short[table_at + 4..table_at + 8].copy_from_slice(&20u32.to_le_bytes());
        let short = write_probe("short", &short);
        assert!(!embeds_dotnet_runtime(&short), "a size that does not even cover the directory");

        let cut = write_probe("cut", &good[..good.len() - 1]);
        assert!(!embeds_dotnet_runtime(&cut), "the terminator of the only name is missing");

        let headers = write_probe("headers", &good[..0x200]);
        assert!(!embeds_dotnet_runtime(&headers), "headers without the section they describe");

        let mut unknown = good.clone();
        unknown[0x80 + 24..0x80 + 26].copy_from_slice(&0x107u16.to_le_bytes());
        let unknown = write_probe("unknown", &unknown);
        assert!(!embeds_dotnet_runtime(&unknown), "a ROM image magic has no directory table we know");

        assert!(!embeds_dotnet_runtime(Path::new("no such file anywhere.exe")));

        // And the untouched original still answers yes, so the five above failed for their damage
        // and not because the fixture never worked.
        let good = write_probe("good", &good);
        assert!(embeds_dotnet_runtime(&good));

        for p in [nowhere, short, cut, headers, unknown, good] {
            let _ = std::fs::remove_file(p);
        }
    }

    /// The fingerprint is only worth something if it reaches the report. This goes through the same
    /// entry the session uses, so removing either line in `fingerprint_target` turns it red.
    #[test]
    fn both_fingerprints_reach_the_runtime_warnings() {
        let dir = std::env::temp_dir().join(format!("chrono-pe-wiring-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("probe dir");

        let aot = dir.join("Aot.exe");
        std::fs::write(&aot, exporting(0x20B, &[b"DotNetRuntimeDebugHeader"])).expect("probe file");
        let keys = crate::report::detect_runtime_warnings(&aot, false);
        assert_eq!(keys, vec!["runtime.dotnet_stopwatch_qpc".to_string()]);
        // Under --scale-qpc the Stopwatch axis DOES scale, so the caution must give way like every
        // other member of its family.
        let keys = crate::report::detect_runtime_warnings(&aot, true);
        assert_eq!(keys, vec!["qpc.scaled_render_may_distort".to_string()]);

        let go = dir.join("Go.exe");
        std::fs::write(&go, synthetic_pe(0, b".data\0\0\0", &go_blob(), None)).expect("probe file");
        let keys = crate::report::detect_runtime_warnings(&go, false);
        assert_eq!(keys, vec!["runtime.go_wall_clock_unreachable".to_string()]);

        let _ = std::fs::remove_dir_all(dir);
    }
}
