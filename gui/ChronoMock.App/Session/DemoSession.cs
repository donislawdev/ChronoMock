using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using ChronoMock.Protocol;

namespace ChronoMock.App;

/// <summary>
/// Builds the start plan for a chosen target. The TARGET now comes from the user (the picker); the MOMENT
/// and MODE are still a fixed placeholder (x60 at a known moment in a non-zero zone) until the moment/mode
/// input slice replaces them. So this stays dev scaffolding for the time only, not for the target.
/// </summary>
internal sealed record DemoSession(string CorePath, PeReader.Machine Machine, StartCommand Start)
{
    private const string Moment = "2038-01-19T03:14:07";
    private const int SessionBiasMinutes = -120; // UTC+02:00 - a non-zero zone, so the zone label is exercised.
    private const long Multiplier = 60;

    /// <summary>
    /// Resolve the core (by the target's PE bitness) and build the start command for the given target.
    /// Throws when the bitness cannot be read - a loud, honest error, never a guessed default.
    /// </summary>
    public static DemoSession ForTarget(string targetPath)
    {
        ArgumentException.ThrowIfNullOrEmpty(targetPath);

        var machine = PeReader.ReadMachine(targetPath);
        if (machine is PeReader.Machine.Unknown)
        {
            throw new InvalidOperationException($"cannot determine the bitness of '{targetPath}'");
        }

        var corePath = CoreLocator.ForRepo(DevPaths.RepoRoot()).CoreForTarget(targetPath);

        var start = new StartCommand
        {
            Id = 1,
            Target = new TargetSpec { Path = targetPath },
            Time = new TimeSpec
            {
                Moment = new MomentSpec { Kind = "absolute", Local = Moment, TzBiasMin = SessionBiasMinutes },
                Mode = "multiplier",
                Multiplier = Multiplier,
            },
        };

        return new DemoSession(corePath, machine, start);
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
