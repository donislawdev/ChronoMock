using ChronoMock.Protocol;

namespace ChronoMock.App;

/// <summary>
/// Everything a session was started with: the application, how it is launched, the moment, the zone, the
/// rate and the options. Taken once at Start (R5-D62), so the history record, the copied summary and the
/// diagnostics block report what was REQUESTED (rule 4) - neither an in-flight change nor an edit made after
/// the session ended, while the form is unlocked and the summary is still copyable.
/// <para>
/// 🔴 The target and the zone are in here for a reason that the moment and the mode do not have. Those two
/// can change IN FLIGHT (the moment becomes a jump, the mode a set_multiplier), which is the case a start
/// snapshot was first written for. The target and the zone cannot - they are start-only. What they CAN do is
/// change AFTER the session ends, because the form unlocks while the summary is still copyable. Reading them
/// live produced a report that carried one session's verdict under another session's target name and zone -
/// evidence naming the wrong application, in the one artifact that leaves this tool and lands in somebody
/// else's ticket.
/// </para>
/// <para>
/// It replaced thirteen loose fields and a flag that said whether they had been filled. The moment is the
/// canonical text of the field, the session zone's wall clock (rule 2).
/// </para>
/// </summary>
internal sealed record SessionSetup(
    string TargetPath,
    string Moment,
    ZoneOption Zone,
    ModeOption Mode,
    string TargetArgs,
    string WorkingFolder,
    bool ScaleDuration,
    bool ScaleQpc,
    bool KeepBackgroundTimers,
    bool GeckoSandboxOff,
    bool Force,
    bool ReachEmbedded,
    bool ReachElevated)
{
    /// <summary>The wire time for this setup. The moment is the local time in the session zone (rule 2,
    /// chrono-mock 9.5) - the core turns it into UTC and validates it (docs/08 section 5).</summary>
    internal TimeSpec Time() => new()
    {
        Moment = new MomentSpec { Kind = "absolute", Local = Moment, TzBiasMin = Zone.BiasMinutes },
        Mode = Mode.Mode,
        Multiplier = Mode.Multiplier,
        ScaleDuration = ScaleDuration,
        ScaleQpc = ScaleQpc,
    };

    /// <summary>The session plan for this setup. It reads the target's header and asks whether files beside
    /// it exist, which blocks for as long as a share that has gone takes to fail, so the caller builds it off
    /// the UI thread (R4-N48). The arguments are split by <see cref="TargetArguments"/>, the CLI's own rule.
    /// </summary>
    internal SessionPlan Plan() => SessionPlan.Build(
        TargetPath, Time(), Force, TargetArguments.Split(TargetArgs), WorkingFolder, ReachEmbedded, ReachElevated,
        KeepBackgroundTimers, GeckoSandboxOff);
}
