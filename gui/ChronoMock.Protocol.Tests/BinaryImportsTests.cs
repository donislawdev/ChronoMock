using System.Buffers.Binary;
using System.Text;

namespace ChronoMock.Protocol.Tests;

/// <summary>
/// The second layer of the promise that this tool never reaches the network, and the first one that
/// looks at what was BUILT rather than at what was written.
/// <para>
/// <c>crates/cli/tests/network.rs</c> reads source. Its own header names the gap this file closes: a
/// dependency that linked WinHTTP would not appear there, because nothing in our source would spell it.
/// The import table is where it would appear anyway - the list of DLLs the Windows loader resolves before
/// the binary executes an instruction, written by the linker from what the code actually calls, not from
/// what anybody claims about it.
/// </para>
/// <para>
/// The measurement that makes this worth pinning: six binaries, five to seven imports each. A register at
/// that size is one somebody can read, so every new import costs an entry with a reason rather than
/// disappearing into a long list. Two of the entries carry the whole point:
/// </para>
/// <list type="bullet">
/// <item><description>
/// <c>chrono.exe</c> imports <c>WS2_32.dll</c> and nothing else that touches a network. That is the
/// Chromium mode's loopback debug port and it is the only socket in the product.
/// </description></item>
/// <item><description>
/// <c>chrono_hook.dll</c> imports no networking DLL AT ALL - not even <c>ws2_32</c>, which it hooks. It
/// resolves that module with <c>GetModuleHandleA</c> and only when the target has already loaded it, so
/// the library we inject into somebody else's process has nothing to reach the network WITH. That is the
/// strongest security property this product has, and the source layer cannot state it.
/// </description></item>
/// </list>
/// <para>
/// <b>Why this lives in C# rather than beside <c>network.rs</c>.</b> Not preference - ordering. A binary
/// guard is worth exactly as much as the freshness of the binaries it reads, and only here are they
/// guaranteed fresh: CI and <c>tools/gates.ps1</c> both run <c>cargo test</c> BEFORE the two release
/// builds and <c>dotnet test</c> after them, so a Rust-side test would read whatever release binaries
/// happened to be lying on the disk. That is the trap this repository has already sprung twice (R2-X3 and
/// R2-X6, both times a stale <c>target/release/</c>), and here it would be worse than stale data: adding a
/// networking dependency changes the binary only after a rebuild, so the guard would pass on the evidence
/// from before the change. Green on old bytes is the one failure a guard must not have.
/// </para>
///
/// <para><b>What this canNOT prove, said plainly.</b></para>
/// <list type="bullet">
/// <item><description>
/// <b>Runtime resolution is invisible to it.</b> <c>chrono_hook.dll</c> is the proof: it uses
/// <c>ws2_32</c> and imports nothing from it. A <c>LoadLibraryA("winhttp.dll")</c> added on purpose would
/// pass here exactly as the hook's own lookup does. That is what the source layer is for, and neither
/// layer answers it alone - which is the reason there are two.
/// </description></item>
/// <item><description>
/// <b>It says nothing about the managed GUI.</b> Measured, not assumed: <c>ChronoMock.exe</c> is a .NET
/// apphost whose import table is the C runtime plus <c>kernel32</c>, <c>user32</c>, <c>shell32</c> and
/// <c>advapi32</c> - the shim, not the program. Managed calls resolve through the runtime and never reach
/// a PE import table. Scanning the self-contained publish instead would be worse than useless: it is 251
/// files and includes the whole <c>System.Net.*</c> family of the shared runtime, which ships with every
/// self-contained .NET application whether or not a line of it is ever called. The C# half's answer stays
/// the source scan in <c>network.rs</c>.
/// </description></item>
/// <item><description>
/// <b>A registered module can still be misused.</b> Nothing here proves WHERE <c>ws2_32</c> connects. The
/// endpoint check - loopback host, and the port the tool was given - lives in
/// <c>crates/cli/src/cdp/mod.rs</c> and has its own tests.
/// </description></item>
/// <item><description>
/// <b>It reads the build, not the download.</b> Whether the bytes a user gets are these bytes is a
/// signing question, and <c>SECURITY.md</c> says plainly that downloads are not signed yet.
/// </description></item>
/// </list>
/// </summary>
public class BinaryImportsTests
{
    private const string X64 = "x86_64-pc-windows-msvc";
    private const string X86 = "i686-pc-windows-msvc";

    /// <summary>Data directory indices, from the PE format: 1 is the import table, 13 the delay-load one.</summary>
    private const int ImportDirectoryIndex = 1;
    private const int DelayDirectoryIndex = 13;

    /// <summary>
    /// The binaries under guard. The first two are what the packaging script copies into both
    /// distributions (<c>packaging/build-dist.ps1</c>) - the product itself. <c>chrono-site.exe</c> is
    /// built by the same command and shipped to nobody. It is here anyway, because generating the public
    /// website is the one job in this workspace where fetching something would look reasonable, and
    /// <c>network.rs</c> already lists <c>crates/site</c> among the crates that must open nothing.
    /// </summary>
    private static readonly string[] GuardedBinaries =
    [
        "chrono.exe",
        "chrono_hook.dll",
        "chrono-site.exe",
    ];

    private static readonly string[] Triples = [X64, X86];

    /// <summary>
    /// Register 1: module names with no legitimate place in ANY binary this workspace builds. Their
    /// presence is the finding, and no register entry may grant one - the test below refuses the overlap.
    /// </summary>
    /// <remarks>
    /// Matched as a fragment of the file name, without case, so <c>WINHTTP.dll</c> and <c>winhttp.dll</c>
    /// are the same finding. <c>ws2_32</c> is deliberately absent: it is registered, once, with a reason.
    /// </remarks>
    private static readonly (string Fragment, string What)[] Forbidden =
    [
        ("winhttp", "WinHTTP, the Windows HTTP stack"),
        ("wininet", "WinINet, the other Windows HTTP stack"),
        // The one-call downloader's own name is deliberately not spelled out here. The source layer's
        // forbidden register has no exemption mechanism, on purpose - a name with no legitimate place
        // may not appear in any file, and that includes this one.
        ("urlmon", "the URL moniker library, home of the one-call file downloader"),
        ("httpapi", "the kernel-mode HTTP stack"),
        ("dnsapi", "name resolution, which is a network round trip"),
        ("iphlpapi", "the IP helper API"),
        ("mswsock", "the Winsock service provider"),
        ("wsock32", "the legacy Winsock"),
        ("netapi32", "the network management API"),
        ("wldap32", "LDAP"),
        ("rasapi32", "dial-up and VPN"),
        ("secur32", "SSPI, which is how a TLS stack authenticates"),
        ("schannel", "the Windows TLS provider"),
        ("webio", "the WinHTTP transport"),
    ];

    /// <summary>
    /// Register 2: every module each binary may import, and why it is there. Measured on both bitnesses
    /// 2026-09-07, and the reasons are read out of the imported FUNCTION names rather than guessed from
    /// the module name - which is how a register stays true after the person who wrote it has left.
    /// </summary>
    private static readonly (string Binary, string Module, string Why)[] Registered =
    [
        (
            "chrono.exe", "kernel32.dll",
            "the Windows base API. It appears TWICE in every one of these binaries, once as kernel32.dll " +
            "and once as KERNEL32.dll, which is why every comparison here ignores case: the linker emits " +
            "one descriptor for this workspace's own calls (CreateProcessW, VirtualAllocEx, " +
            "CreateRemoteThread, MapViewOfFile, GetTickCount64, IsWow64Process2) and another for the C " +
            "runtime's (console, files, locale)"
        ),
        (
            "chrono.exe", "ntdll.dll",
            "the native API beneath it. Measured: NtOpenFile, NtReadFile, NtWriteFile, " +
            "NtCreateNamedPipeFile and RtlNtStatusToDosError - the standard library's file and pipe " +
            "plumbing, which is how the core reads what the child process writes"
        ),
        (
            "chrono.exe", "api-ms-win-core-synch-l1-2-0.dll",
            "an API set rather than a library. Measured: WaitOnAddress, WakeByAddressSingle and " +
            "WakeByAddressAll, which are the standard library's locks"
        ),
        (
            "chrono.exe", "oleaut32.dll",
            "SysFreeString and SysStringLen. BSTR handling, which arrives with the windows crate"
        ),
        (
            "chrono.exe", "bcryptprimitives.dll",
            "ProcessPrng, and nothing else. The system random number generator the standard library " +
            "seeds its hash maps from - local, and the only thing about it that sounds like a network is " +
            "the word crypto in the name"
        ),
        (
            "chrono.exe", "ws2_32.dll",
            "Winsock, and the ONE place this workspace opens a socket. Chromium mode does not inject: it " +
            "launches the browser with a debug port on the loopback interface and drives it over a local " +
            "WebSocket, and the endpoint is checked to be that host and that port before anything is " +
            "sent. Measured: 12 imports, of which WSASocketW, WSADuplicateSocketW, getaddrinfo and " +
            "freeaddrinfo are by name and the rest by ordinal - the shape of a client, with no listener " +
            "and no server"
        ),

        (
            "chrono_hook.dll", "kernel32.dll",
            "the base API again, and the same two descriptors. This is the injected library, so the list " +
            "reads like one: CreateToolhelp32Snapshot, SuspendThread, SetThreadContext, VirtualProtect " +
            "and FlushInstructionCache are the hook installing itself"
        ),
        (
            "chrono_hook.dll", "ntdll.dll",
            "two functions only, NtWriteFile and RtlNtStatusToDosError - writing the diagnostics line"
        ),
        (
            "chrono_hook.dll", "api-ms-win-core-synch-l1-2-0.dll",
            "the same three lock primitives as the core"
        ),
        (
            "chrono_hook.dll", "oleaut32.dll",
            "the same two BSTR functions as the core"
        ),

        (
            "chrono-site.exe", "kernel32.dll",
            "the base API. The site generator reads templates and writes HTML, and its import list is " +
            "three modules long"
        ),
        (
            "chrono-site.exe", "ntdll.dll",
            "the file plumbing beneath that"
        ),
        (
            "chrono-site.exe", "api-ms-win-core-synch-l1-2-0.dll",
            "the standard library's locks"
        ),
    ];

    // -----------------------------------------------------------------------------------------------
    // B1. Nothing is imported that the register does not name
    // -----------------------------------------------------------------------------------------------

    [Fact]
    public void Every_import_of_every_built_binary_is_registered()
    {
        var offenders = new List<string>();
        var read = 0;
        foreach (var entry in EveryGuardedBinary())
        {
            read++;
            // The triple is not part of a finding, because the register is written per binary - but it is
            // part of the MESSAGE, so a failure says which of the two builds disagreed.
            offenders.AddRange(Findings(entry.Table, entry.Binary).Select(f => $"{entry.Triple}: {f}"));

            // A table that parsed to nothing is the same silence as a scan that read no files, one level
            // down. The smallest of the six imports three modules.
            Assert.True(
                entry.Table.Modules.Count >= 3,
                $"{entry.Binary} ({entry.Triple}) parsed to {entry.Table.Modules.Count} imports, which is " +
                "not a binary that runs - the parse, not the product, is what went wrong");
        }

        // The canary every scan needs: one that reads nothing finds nothing and looks exactly like one
        // that works. Written as a LITERAL rather than as GuardedBinaries.Length * Triples.Length - the
        // computed form passes when the list it is checking is emptied, which is a canary that finds
        // itself. Measured, not assumed: the first draft of this file had the computed form, and emptying
        // GuardedBinaries left all thirteen tests green. Three binaries, two triples.
        Assert.Equal(6, read);
        Assert.True(
            offenders.Count == 0,
            "the product promises it never reaches the network, and the linker disagrees. Either this is " +
            "a mistake, or the register at the top of this file needs an entry saying why it is not: " +
            string.Join("; ", offenders));
    }

    // -----------------------------------------------------------------------------------------------
    // B2. The register describes imports that exist
    // -----------------------------------------------------------------------------------------------

    [Fact]
    public void The_register_names_nothing_the_binaries_no_longer_import()
    {
        var stale = new List<string>();
        foreach (var (binary, triple) in EveryPair())
        {
            var modules = PeImportTable.Read(RepoPaths.ReleaseBinary(RepoPaths.RepoRoot(), triple, binary)).Modules;
            stale.AddRange(Registered
                .Where(r => r.Binary == binary)
                .Where(r => !modules.Contains(r.Module, StringComparer.OrdinalIgnoreCase))
                .Select(r => $"{binary} ({triple}) no longer imports {r.Module}, which still holds an entry"));
        }

        Assert.True(
            stale.Count == 0,
            "an entry outlived the import it was written for. A register that keeps dead entries is one " +
            "nobody has read since the code moved: " + string.Join("; ", stale));
    }

    // -----------------------------------------------------------------------------------------------
    // B3. The injected library has nothing to reach the network with
    // -----------------------------------------------------------------------------------------------

    [Fact]
    public void The_injected_library_imports_no_networking_module_at_all()
    {
        foreach (var triple in Triples)
        {
            var table = PeImportTable.Read(RepoPaths.ReleaseBinary(RepoPaths.RepoRoot(), triple, "chrono_hook.dll"));

            Assert.False(
                table.ImportsAnythingNamed("ws2_32"),
                $"chrono_hook.dll ({triple}) links Winsock. It hooks connect - it must never CALL it. The " +
                "module is resolved with GetModuleHandleA, and only when the target has already loaded " +
                $"it, which is what keeps this import table clean: {string.Join(", ", table.Modules)}");

            var networking = table.Modules.Where(IsNetworking).ToList();
            Assert.True(
                networking.Count == 0,
                $"chrono_hook.dll ({triple}) is loaded into a program somebody else wrote. It imports " +
                "a networking module: " + string.Join(", ", networking));
        }
    }

    // -----------------------------------------------------------------------------------------------
    // B4. Exactly one binary reaches a socket, and exactly one module gets it there
    // -----------------------------------------------------------------------------------------------

    [Fact]
    public void Only_the_core_links_winsock_and_no_binary_links_anything_else_networking()
    {
        foreach (var (binary, triple) in EveryPair())
        {
            var table = PeImportTable.Read(RepoPaths.ReleaseBinary(RepoPaths.RepoRoot(), triple, binary));
            var forbidden = table.Modules.Where(m => Forbidden.Any(f => NameContains(m, f.Fragment))).ToList();

            Assert.True(forbidden.Count == 0, $"{binary} ({triple}) imports {string.Join(", ", forbidden)}");
            Assert.Equal(binary == "chrono.exe", table.ImportsAnythingNamed("ws2_32"));
        }
    }

    // -----------------------------------------------------------------------------------------------
    // B5. No import arrives by a path this file does not read
    // -----------------------------------------------------------------------------------------------

    [Fact]
    public void No_binary_uses_a_delay_loaded_or_a_bound_import()
    {
        foreach (var entry in EveryGuardedBinary())
        {
            // Measured 2026-09-07: both directories are zero in all six. Asserted rather than enumerated,
            // and that is the whole answer to "the prototype read only the ordinary table". A delay-load
            // entry is a second list of DLLs, resolved on first call instead of at load, so a walker that
            // no binary here could exercise would be untested code inside a guard. Refusing the table
            // outright is the same promise with nothing unproven in it - and the day a linker emits one,
            // this fails and whoever wanted it writes the walk and the register for it.
            Assert.True(
                entry.Table.DelayImports.IsEmpty,
                $"{entry.Binary} carries a delay-load import directory (rva 0x{entry.Table.DelayImports.Rva:X}, " +
                $"size {entry.Table.DelayImports.Size}). Those modules are not in the register above, " +
                "because until now there were none");

            // Bound imports are a cache of the ordinary table rather than a second list, so nothing can
            // hide there. Asserted anyway, so that "every import path was looked at" is a fact and not a
            // recollection.
            Assert.True(entry.Table.BoundImports.IsEmpty, $"{entry.Binary} carries a bound import table");
        }
    }

    // -----------------------------------------------------------------------------------------------
    // B6. The two bitnesses are the same product
    // -----------------------------------------------------------------------------------------------

    [Fact]
    public void Both_bitnesses_import_the_same_modules()
    {
        var root = RepoPaths.RepoRoot();
        foreach (var binary in GuardedBinaries)
        {
            var x64 = PeImportTable.Read(RepoPaths.ReleaseBinary(root, X64, binary)).Modules;
            var x86 = PeImportTable.Read(RepoPaths.ReleaseBinary(root, X86, binary)).Modules;

            // The register is written per binary, not per triple, and this is what earns that. It also
            // catches the failure the working rules warn about most often: one target rebuilt and the
            // other left a day behind, which makes x86 look like a regression it never had.
            Assert.Equal(x64, x86, StringComparer.OrdinalIgnoreCase);
        }
    }

    // -----------------------------------------------------------------------------------------------
    // B7. The two registers cannot contradict each other
    // -----------------------------------------------------------------------------------------------

    [Fact]
    public void A_forbidden_module_may_not_be_registered()
    {
        var granted = Registered
            .Where(r => Forbidden.Any(f => NameContains(r.Module, f.Fragment)))
            .Select(r => $"{r.Binary} was granted {r.Module}")
            .ToList();

        Assert.True(
            granted.Count == 0,
            "a module with no legitimate place here was written into the register of permitted ones. The " +
            "registers are two questions, not one list, and this is the answer to somebody quieting a " +
            "finding by registering it: " + string.Join("; ", granted));
    }

    // -----------------------------------------------------------------------------------------------
    // B8-B11. The canary, in both directions
    // -----------------------------------------------------------------------------------------------
    //
    // Each of these takes a REAL binary, changes one thing about it, and reads it back through the same
    // parser. A guard nobody has watched fail is indistinguishable from one that reads nothing, and a
    // canary built out of a hand-made fixture proves only that the fixture matches the parser.

    [Fact]
    public void A_networking_import_added_to_a_real_binary_is_caught()
    {
        var core = Rename("chrono.exe", X64, from: "WS2_32.dll", to: "URLMON.dll");
        Assert.Contains(Findings(core, "chrono.exe"), f => f.Contains("URLMON.dll", StringComparison.Ordinal));

        // The one that matters most: the injected library given a way out. oleaut32 is registered for it,
        // so this proves the FORBIDDEN register fires rather than the unregistered one.
        var hook = Rename("chrono_hook.dll", X64, from: "oleaut32.dll", to: "winhttp.dll");
        Assert.Contains(hook.Modules, m => IsNetworking(m));
        Assert.Contains(Findings(hook, "chrono_hook.dll"), f => f.Contains("winhttp.dll", StringComparison.Ordinal));
    }

    [Fact]
    public void An_unregistered_module_is_caught_even_when_it_is_not_networking()
    {
        // The register pins the whole list, not just the dangerous half. Nothing about psapi.dll is a
        // network - the finding is that it is new, and new is what has to be written down.
        var patched = Rename("chrono.exe", X64, from: "ntdll.dll", to: "psapi.dll");

        var findings = Findings(patched, "chrono.exe");
        Assert.Contains(findings, f => f.Contains("psapi.dll", StringComparison.Ordinal));
        Assert.DoesNotContain(findings, f => f.Contains("has no place", StringComparison.Ordinal));
    }

    [Fact]
    public void A_module_registered_for_one_binary_is_not_allowed_in_another()
    {
        // The register is per binary, which is the half most easily got wrong. ws2_32 is granted to the
        // core with a reason that is about the core, and that grant must not travel.
        var hook = Rename("chrono_hook.dll", X64, from: "oleaut32.dll", to: "ws2_32.dll");

        Assert.Contains(Findings(hook, "chrono_hook.dll"), f => f.Contains("ws2_32.dll", StringComparison.Ordinal));
        Assert.DoesNotContain(Findings(hook, "chrono.exe"), f => f.Contains("ws2_32.dll", StringComparison.Ordinal));
    }

    [Fact]
    public void A_delay_load_directory_added_to_a_real_binary_is_caught()
    {
        var image = File.ReadAllBytes(RepoPaths.ReleaseBinary(RepoPaths.RepoRoot(), X64, "chrono.exe"));
        var original = PeImportTable.Read(image, "chrono.exe");
        Assert.True(original.DelayImports.IsEmpty, "the unpatched binary already carries a delay directory");

        // Point directory 13 at the ordinary import table and give it a size, which is what a linker
        // emitting a delay-load section would look like from here.
        int entry = original.DataDirectoryOffset + (DelayDirectoryIndex * 8);
        uint importRva = BinaryPrimitives.ReadUInt32LittleEndian(
            image.AsSpan(original.DataDirectoryOffset + (ImportDirectoryIndex * 8)));
        BinaryPrimitives.WriteUInt32LittleEndian(image.AsSpan(entry), importRva);
        BinaryPrimitives.WriteUInt32LittleEndian(image.AsSpan(entry + 4), 32u);

        var patched = PeImportTable.Read(image, "chrono.exe");
        Assert.False(patched.DelayImports.IsEmpty);
        Assert.Contains(Findings(patched, "chrono.exe"), f => f.Contains("delay-load", StringComparison.Ordinal));
    }

    [Fact]
    public void The_binaries_as_built_raise_nothing()
    {
        // The negative half of the same claim. A scanner that flagged everything would satisfy every
        // canary above and be worthless, and this is the assertion that says the clean case is clean.
        foreach (var entry in EveryGuardedBinary())
        {
            Assert.Empty(Findings(entry.Table, entry.Binary));
        }
    }

    [Fact]
    public void A_file_that_is_not_a_pe_is_a_thrown_error_and_never_an_empty_list()
    {
        // Silence is the failure mode this whole file exists to avoid: a parser that answered "no
        // imports" for a file it could not read would make every test above pass for the wrong reason.
        var notAPe = Path.Combine(RepoPaths.RepoRoot(), "Cargo.toml");
        var error = Assert.Throws<InvalidDataException>(() => PeImportTable.Read(notAPe));
        Assert.Contains("MZ", error.Message, StringComparison.Ordinal);
    }

    // -----------------------------------------------------------------------------------------------
    // The scan itself, as a pure function over one parsed table
    // -----------------------------------------------------------------------------------------------

    /// <summary>
    /// Every finding in one binary. Pure, over an already-parsed table, so the canary can point it at an
    /// image that no build produced.
    /// </summary>
    private static List<string> Findings(PeImportTable table, string binary)
    {
        var findings = new List<string>();
        foreach (var module in table.Modules)
        {
            var forbidden = Forbidden.Where(f => NameContains(module, f.Fragment)).ToList();
            if (forbidden.Count > 0)
            {
                findings.Add($"{binary} imports {module} - {forbidden[0].What}, which has no place in any binary built here");
            }
            else if (!Registered.Any(r => r.Binary == binary && SameModule(r.Module, module)))
            {
                findings.Add($"{binary} imports {module}, which no entry in the register names");
            }
        }

        if (!table.DelayImports.IsEmpty)
        {
            findings.Add($"{binary} carries a delay-load import directory, and its modules are in no register");
        }

        if (!table.BoundImports.IsEmpty)
        {
            findings.Add($"{binary} carries a bound import table");
        }

        return findings;
    }

    private static bool IsNetworking(string module) =>
        NameContains(module, "ws2_32") || Forbidden.Any(f => NameContains(module, f.Fragment));

    private static bool NameContains(string module, string fragment) =>
        module.Contains(fragment, StringComparison.OrdinalIgnoreCase);

    private static bool SameModule(string left, string right) =>
        string.Equals(left, right, StringComparison.OrdinalIgnoreCase);

    // -----------------------------------------------------------------------------------------------
    // Reading the build outputs
    // -----------------------------------------------------------------------------------------------

    private static IEnumerable<(string Binary, string Triple)> EveryPair() =>
        from binary in GuardedBinaries
        from triple in Triples
        select (binary, triple);

    private static IEnumerable<(string Binary, string Triple, PeImportTable Table)> EveryGuardedBinary()
    {
        var root = RepoPaths.RepoRoot();
        return EveryPair()
            .Select(p => (p.Binary, p.Triple, PeImportTable.Read(RepoPaths.ReleaseBinary(root, p.Triple, p.Binary))))
            .ToList();
    }

    /// <summary>
    /// A real binary with one module name rewritten in place, read back through the real parser.
    /// </summary>
    /// <remarks>
    /// The replacement is written WITH its own NUL terminator inside the bytes of the old name, so every
    /// other address in the file stays where it was and the image parses exactly as it did before. That
    /// is the only constraint: the new name may be no longer than the one it replaces.
    /// </remarks>
    private static PeImportTable Rename(string binary, string triple, string from, string to)
    {
        var image = File.ReadAllBytes(RepoPaths.ReleaseBinary(RepoPaths.RepoRoot(), triple, binary));
        var original = PeImportTable.Read(image, binary);

        var matches = original.Entries.Where(e => SameModule(e.Name, from)).ToList();
        Assert.True(matches.Count > 0, $"{binary} ({triple}) does not import {from}, so this canary tests nothing");
        Assert.True(to.Length <= from.Length, $"'{to}' is longer than '{from}' and would overwrite the next name");

        Encoding.ASCII.GetBytes(to + "\0").CopyTo(image, matches[0].NameOffset);
        return PeImportTable.Read(image, binary);
    }
}
