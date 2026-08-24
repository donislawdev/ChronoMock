using ChronoMock.Protocol;

namespace ChronoMock.App;

/// <summary>
/// DEV SCAFFOLDING (slice 3.2-II): a fixed demo session so the live two-clock panel has something to show
/// before the target picker and the moment/mode inputs exist (those are later slices). It launches the
/// bundled test target at a known moment, accelerated x60, in a non-zero session zone - so the fake clock
/// visibly races the real one and both carry an explicit "UTC+02:00". Replaced whole by the real picker.
/// </summary>
internal sealed record DemoSession(string CorePath, PeReader.Machine Machine, StartCommand Start)
{
    private const string Moment = "2038-01-19T03:14:07";
    private const int SessionBiasMinutes = -120; // UTC+02:00 - a non-zero zone, so the zone label is exercised.
    private const long Multiplier = 60;

    public static DemoSession Resolve()
    {
        var repoRoot = DevPaths.RepoRoot();
        var target = DevPaths.TestTargetExe(repoRoot);

        var machine = PeReader.ReadMachine(target);
        if (machine is PeReader.Machine.Unknown)
        {
            throw new InvalidOperationException($"cannot determine the bitness of '{target}'");
        }

        var corePath = CoreLocator.ForRepo(repoRoot).CoreForTarget(target);

        var start = new StartCommand
        {
            Id = 1,
            Target = new TargetSpec { Path = target },
            Time = new TimeSpec
            {
                Moment = new MomentSpec { Kind = "absolute", Local = Moment, TzBiasMin = SessionBiasMinutes },
                Mode = "multiplier",
                Multiplier = Multiplier,
            },
        };

        return new DemoSession(corePath, machine, start);
    }
}
