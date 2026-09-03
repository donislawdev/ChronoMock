namespace ChronoMock.Protocol;

/// <summary>
/// Resolves which core executable to launch for a given target, by the target's PE bitness. The
/// base-directory strategy is pluggable so a dev checkout (cargo target dirs) and a shipped portable
/// layout (both cores side by side) differ only here.
/// </summary>
public sealed class CoreLocator
{
    private readonly Func<PeReader.Machine, string> _resolveCorePath;

    public CoreLocator(Func<PeReader.Machine, string> resolveCorePath)
        => _resolveCorePath = resolveCorePath;

    /// <summary>
    /// Path to the core executable whose bitness matches the target at <paramref name="targetExePath"/>.
    /// Throws when the target's bitness cannot be read - an honest error, never a guessed default.
    /// </summary>
    public string CoreForTarget(string targetExePath)
    {
        var machine = PeReader.ReadMachine(targetExePath);
        if (machine is PeReader.Machine.Unknown)
        {
            throw new InvalidOperationException($"cannot determine the bitness of '{targetExePath}'");
        }

        return _resolveCorePath(machine);
    }

    /// <summary>Dev-checkout factory: the cores are the cargo build outputs under <paramref name="repoRoot"/>.
    /// Both are named by their explicit target triple, because <c>target/release/</c> is written only by a
    /// build with NO <c>--target</c> - and the working rule is to build both triples explicitly, so that
    /// directory holds whatever binary someone last built without the flag. The run-targets harness read
    /// x64 from there and spent a session testing a day-old core (R2-X3); this is the same trap in the
    /// GUI's own path.</summary>
    public static CoreLocator ForRepo(string repoRoot) => new(machine => machine switch
    {
        PeReader.Machine.X64 =>
            Path.Combine(repoRoot, "target", "x86_64-pc-windows-msvc", "release", "chrono.exe"),
        PeReader.Machine.X86 => Path.Combine(repoRoot, "target", "i686-pc-windows-msvc", "release", "chrono.exe"),
        _ => throw new InvalidOperationException($"unsupported bitness {machine}"),
    });

    /// <summary>Portable-install factory (the shipped layout, Stage 5): the cores sit under
    /// <paramref name="baseDir"/>/core/&lt;arch&gt;/, each beside its matching-bitness chrono_hook.dll.</summary>
    public static CoreLocator ForPortable(string baseDir) => new(machine => machine switch
    {
        PeReader.Machine.X64 => Path.Combine(baseDir, "core", "x64", "chrono.exe"),
        PeReader.Machine.X86 => Path.Combine(baseDir, "core", "x86", "chrono.exe"),
        _ => throw new InvalidOperationException($"unsupported bitness {machine}"),
    });
}
