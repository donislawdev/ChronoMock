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
    public static SessionPlan Build(string targetPath, TimeSpec time)
    {
        ArgumentException.ThrowIfNullOrEmpty(targetPath);
        ArgumentNullException.ThrowIfNull(time);

        var machine = PeReader.ReadMachine(targetPath);
        if (machine is PeReader.Machine.Unknown)
        {
            throw new InvalidOperationException($"cannot determine the bitness of '{targetPath}'");
        }

        var corePath = CoreLocator.ForRepo(DevPaths.RepoRoot()).CoreForTarget(targetPath);
        var start = new StartCommand { Id = 1, Target = new TargetSpec { Path = targetPath }, Time = time };
        return new SessionPlan(corePath, machine, start, ChromiumTarget.IsChromium(targetPath));
    }

    /// <summary>
    /// The bundled sample target, pre-selected so the panel is usable at once in a dev checkout, or null
    /// when the solution has not been built (then the user must pick a target). Dev scaffolding.
    /// </summary>
    public static string? DefaultTargetPath()
    {
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
