using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using ChronoMock.Protocol;

namespace ChronoMock.App;

/// <summary>
/// Resolves the core (by the target's PE bitness) and wraps a chosen target and time into a start command.
/// Both the target and the time now come from the user, so nothing here is hard-coded - only the default
/// target lookup below is dev scaffolding (a pre-selected sample so the panel is usable at once).
/// </summary>
internal sealed record SessionPlan(string CorePath, PeReader.Machine Machine, StartCommand Start, bool IsCdp)
{
    /// <summary>
    /// Build the plan for a target and a time. Throws when the target's bitness cannot be read - a loud,
    /// honest error, never a guessed default. The core validates the moment semantically (docs/08 section 5).
    /// <para>
    /// A Chromium/Electron target is driven by the core over CDP (ADR-8/ADR-9), where bitness is irrelevant
    /// (we do not inject). We still pick the matching-bitness core - it exists and drives CDP either way -
    /// but flag the plan so the handshake gate skips the bitness check.
    /// </para>
    /// </summary>
    public static SessionPlan Build(
        string targetPath,
        TimeSpec time,
        bool force = false,
        IReadOnlyList<string>? args = null,
        string? workingFolder = null,
        bool embedded = true,
        bool elevatedEmbedded = false,
        bool keepBackgroundTimers = false,
        bool geckoSandboxOff = false)
    {
        ArgumentException.ThrowIfNullOrEmpty(targetPath);
        ArgumentNullException.ThrowIfNull(time);

        var machine = PeReader.ReadMachine(targetPath);
        if (machine is PeReader.Machine.Unknown)
        {
            throw new InvalidOperationException($"cannot determine the bitness of '{targetPath}'");
        }

        var corePath = AppPaths.SubstitutionCores.CoreForTarget(targetPath);
        var start = new StartCommand
        {
            Id = 1,
            Target = new TargetSpec
            {
                Path = targetPath,
                Args = args ?? [],
                // Blank means "do not ask", which is not the same as an empty string: the core passes a
                // present-but-empty cwd straight to CreateProcessW, where it is not a valid directory.
                Cwd = string.IsNullOrWhiteSpace(workingFolder) ? null : workingFolder,
                Embedded = embedded,
                // The registry option rides on the channel, so it is never on without it: the core would
                // ignore it, and a start command that says both says something the session will not do.
                ElevatedEmbedded = elevatedEmbedded && embedded,
                // For the browser of a Chromium target and the engine inside an application alike, so it
                // does not ride on the channel the way the registry option does (R4-N28).
                KeepBackgroundTimers = keepBackgroundTimers,
                // Any program may get it: a Gecko application started through a launcher in another folder
                // is not recognised beforehand, and the variables reach it all the same (ADR-28).
                GeckoSandbox = !geckoSandboxOff,
                // A console program shows up the way it does when the tester starts it, in a console
                // window of its own (R4-D16, ADR-17). The core runs with no window, so the console it
                // could share is one nobody sees: no input, and the program's output in the diagnostics
                // block. A program with a window gets nothing from this either way.
                Console = TargetSpec.NewConsole,
            },
            Time = time,
            Force = force,
        };
        return new SessionPlan(corePath, machine, start, ChromiumTarget.IsChromium(targetPath));
    }

    /// <summary>
    /// The bundled sample target, pre-selected so the panel is usable at once in a dev checkout, or null
    /// when the solution has not been built (then the user must pick a target). Dev scaffolding: a shipped
    /// portable install has no sample and no repo root, so it returns null at once without touching DevPaths.
    /// </summary>
    public static string? DefaultTargetPath()
    {
        if (AppPaths.IsPortable)
        {
            return null;
        }

        try
        {
            return DevPaths.TestTargetExe(DevPaths.RepoRoot());
        }
        catch (Exception ex) when (ex is IOException or InvalidOperationException)
        {
            return null;
        }
    }
}
