using ChronoMock.Protocol;

namespace ChronoMock.App;

/// <summary>
/// What one session has said about itself so far: its status, its verdict, the audit of its clocks, the
/// processes and engines it reached, and how it ended. One place for all of it (R5-D62).
/// <para>
/// It exists because <see cref="SessionViewModel"/> held these as some forty fields of its own, beside the
/// setup form and the commands, and every reader of them - the screen, the copied summary, the diagnostics
/// block, the history record - had to be a member of that one class. The view model still projects them
/// onto the panel and raises the changes (it writes each one through its own setter, so a binding sees
/// exactly the notifications it saw before), while <see cref="SessionReport"/> reads them without a view
/// model at all.
/// </para>
/// <para>
/// Plain fields rather than properties, so the view model can hand each one to its change-raising setter by
/// reference. Nothing outside the view model writes them.
/// </para>
/// </summary>
internal sealed class SessionResult
{
    /// <summary>
    /// The warnings that undo a works verdict as evidence (R5-D3): a Go program reads its wall clock where
    /// no hook reaches, and a session in which no process read a substituted clock proves nothing about
    /// time. The verdict stands, the export gets the banner. Mirrors the CLI's WARNINGS_THAT_UNDO_EVIDENCE,
    /// and RustConstantMirrorTests holds the two lists together.
    /// </summary>
    internal static readonly string[] WarningsThatUndoEvidence =
        ["runtime.go_wall_clock_unreachable", "coverage.session_clock_never_read"];

    /// <summary>
    /// How an exit code is spelled - the application's own and the core's in the diagnostics. A crash ends a
    /// program with the status of the error that stopped it, which the wire carries as a negative number - an
    /// access violation reads -1073741819, which tells a tester nothing - so a negative code also gets the
    /// hexadecimal form that can be looked up (0xC0000005). The CLI report's <c>exit_code_label</c>, one
    /// spelling on both sides.
    /// </summary>
    internal static string ExitCodeLabel(int code) => code < 0
        ? string.Create(System.Globalization.CultureInfo.InvariantCulture, $"{code} (0x{unchecked((uint)code):X8})")
        : code.ToString(System.Globalization.CultureInfo.InvariantCulture);

    internal string StatusKey = "status.idle";
    internal SessionStatusKind StatusKind = SessionStatusKind.Idle;

    internal bool VerdictKnown;
    internal VerdictKind VerdictKind = VerdictKind.Unknown;
    internal string VerdictLabelKey = "verdict.unknown";
    internal string VerdictReasonKey = string.Empty;
    internal string VerdictMeaningKey = string.Empty;
    internal bool VerdictHasReason;
    internal bool VerdictHasMeaning;
    internal int ProcessCount;
    internal bool CoverageKnown;

    /// <summary>The pid of the process whose call counts the panel shows - the first one reported,
    /// which is the parent. Later events for it replace its counts - other pids never do (R2-X8).</summary>
    internal uint? ParentPid;
    internal bool IsCdp;
    internal IReadOnlyList<CoveredChannel> Covered = [];
    internal IReadOnlyList<CoveredChannel> Observed = [];

    // A native session's covered rows in two halves: the parent process's (replaced by its later
    // snapshot, R2-X8) and the pages reached inside the application (accumulated per context, docs/09).
    // Kept apart so a late parent snapshot cannot wipe the page rows, whichever arrives last.
    internal IReadOnlyList<CoveredChannel> ParentCovered = [];
    internal IReadOnlyList<CoveredChannel> PageCovered = [];
    internal IReadOnlyList<string> Uncovered = [];
    internal IReadOnlyList<string> Unobserved = [];
    internal IReadOnlyList<string> InstalledLate = [];
    internal IReadOnlyList<string> Warnings = [];

    // The processes the family spawned and the hook did not follow into, as the session verdict names them
    // (docs/zasady/SLOWNIK uncoveredChild). They are the reason a family verdict is partial, and until slice
    // A2 the model dropped them: the screen said "the ones this application started without it ran on the
    // real clock" and never said which (untouchable rule 4). The wire caps the list, so the total travels
    // separately and is the number the screen prints.
    internal IReadOnlyList<UncoveredChild> UncoveredChildren = [];
    internal int UncoveredChildrenTotal;

    // The web engines inside the application whose debugging port the session reached, so the pages they
    // host ran on the session clock. The port is the actionable half and the reason this list is on the
    // screen at all: the session raises a warning that a debugging port stands open to other programs for
    // as long as the engine runs, and a warning whose subject the reader cannot identify is one they
    // cannot act on. The CLI report has named the port since slice C - this is the same fact, one
    // interface over.
    internal IReadOnlyList<ReachedEngine> Engines = [];
    internal bool EnginePagesReached;

    internal bool HasTiming;

    // Whether the core closed the session with `ended` (R4-W3). See IsCutShort.
    internal bool EndedSeen;
    internal long ElapsedRealMs;
    internal long ElapsedFakeMs;
    internal string FakeEndWall = string.Empty;
    internal string VanishReasonKey = string.Empty;
    internal long LivedMs;

    // Final-state facts the core reports in `ended` (RELEASE audit): the target's own exit code (native
    // sessions where the app exited on its own - null for Stop/end and for CDP) and any cleanup residue it
    // could not remove (today only a CDP temp profile). Surfaced honestly, never dropped (rule 6).
    internal int? TargetExitCode;
    internal IReadOnlyList<string> FollowedRows = [];
    internal IReadOnlyList<string> CopiesOutsideRows = [];
    internal IReadOnlyList<string> LeftRunningRows = [];
    internal bool RefusalIncomplete;
    internal IReadOnlyList<string> ResidueKeys = [];

    /// <summary>The warning the core raises when the machine slept during the session, as a list so the panel
    /// can add it without a branch (<c>Except</c> keeps it from appearing twice).</summary>
    internal static readonly string[] HostSleptWarning = ["time.host_slept"];

    // What the copied summary says and the panel's own projection does not (R5-D71): every error the core
    // sent, in order, how long the machine slept, and the browser a Chromium session drove (R5-D44). The CLI
    // report has said all three since R5/2 and R5/5a, and the window's summary was the same session told
    // with less.
    internal IReadOnlyList<string> Errors = [];
    internal long HostSleptMs;
    internal string Browser = string.Empty;

    // Which core the session asked for and what became of it, and what failed in the window, for the
    // diagnostics block (R5/6b, D2). A block about a failed session named neither the core nor the error, so
    // "it does not start" reached support as a status key and a message with no stack.
    internal string CorePath = string.Empty;
    internal bool CoreStarted;
    internal string CoreVersion = string.Empty;
    internal string CoreBitness = string.Empty;
    internal Exception? Fault;

    /// <summary>The core is running: note what its handshake said about it, nothing when it never said ready.</summary>
    internal void HeardFrom(CoreSession session)
    {
        CoreStarted = true;
        CoreVersion = session.Ready?.CoreVersion ?? string.Empty;
        CoreBitness = session.Ready?.Bitness ?? string.Empty;
    }

    /// <summary>
    /// Fold what an event says for the report beyond what the panel projects (R5-D71): an error of any kind and
    /// at any point - one the session went on after, one it ended on, one that came after the end - joins the
    /// errors list. A sleep raises the slept time, the session verdict settles it and names the browser.
    /// Returns true for the core's sleep notice, the one fact the panel shows the moment it arrives.
    /// </summary>
    internal bool Absorb(ChronoEvent evt)
    {
        switch (evt)
        {
            case ErrorEvent error:
                Errors = [.. Errors, error.Key];
                return false;
            case HostSleptEvent slept:
                // The running total, never the one nap: the core sends both, and the total is what the
                // verdict carries at the end.
                HostSleptMs = Math.Max(HostSleptMs, slept.TotalSleptMs);
                return true;
            case SessionVerdictEvent verdict:
                HostSleptMs = Math.Max(HostSleptMs, verdict.HostSleptMs);
                Browser = verdict.Browser ?? string.Empty;
                return false;
            default:
                return false;
        }
    }

    /// <summary>Forget the last session's report facts - what <see cref="Absorb"/> gathered, and its core and
    /// fault - for the next one.</summary>
    internal void ForgetReportFacts()
    {
        Errors = [];
        HostSleptMs = 0;
        Browser = string.Empty;
        CorePath = string.Empty;
        CoreStarted = false;
        CoreVersion = string.Empty;
        CoreBitness = string.Empty;
        Fault = null;
    }

    /// <summary>
    /// The core stopped before it closed the session (R4-W3): no <c>ended</c> reached the panel. A core that
    /// died or went quiet ends on <see cref="SessionStatusKind.CoreStopped"/> or
    /// <see cref="SessionStatusKind.CoreUnresponsive"/>, which are only ever set when no <c>ended</c> came -
    /// one that did would have left the session on <see cref="SessionStatusKind.Ended"/>. A Stop is the one
    /// ending that can go either way, because the core is killed when it does not end in time.
    /// </summary>
    internal bool IsCutShort => StatusKind is SessionStatusKind.CoreStopped or SessionStatusKind.CoreUnresponsive
        || (StatusKind == SessionStatusKind.Stopped && !EndedSeen);

    /// <summary>True when the session spanned more than one process (the verdict is the family aggregate).</summary>
    internal bool IsFamily => ProcessCount > 1;

    /// <summary>How many of the counted processes the report could not name - the ones past the wire's cap.
    /// Said in words under the table, so a total above the rows is never a mismatch left for the reader to
    /// notice (untouchable rule 4). Distinct from a named row that has no image: that process was seen and
    /// gone before it could be asked, this one was never listed at all.</summary>
    internal int UncoveredChildrenUnnamed
        => UncoveredChildrenTotal > UncoveredChildren.Count ? UncoveredChildrenTotal - UncoveredChildren.Count : 0;

    /// <summary>A clean "works" session is the only reliable one - anything else must carry the unreliable
    /// banner in an export (chrono-mock 8.8), mirroring the CLI's session_is_reliable.</summary>
    internal bool IsReliable => VerdictKind == VerdictKind.Works
                                && StatusKind is not (SessionStatusKind.DidNotTakeEffect or SessionStatusKind.Error)
                                && !IsCutShort
                                && !EvidenceUndoingWarnings.Any();

    /// <summary>The session's warnings that are in <see cref="WarningsThatUndoEvidence"/>, in the order the
    /// session raised them - one list behind both the copied banner and the caveats on the result card.</summary>
    internal IEnumerable<string> EvidenceUndoingWarnings => Warnings.Where(WarningsThatUndoEvidence.Contains);

    // A vanished session could not be audited, so it is recorded as "undetermined" - honest, never a faked
    // verdict (untouchable rule 4). Otherwise the per-session/family verdict kind maps to its wire string.
    internal string RecordedVerdict() => StatusKind == SessionStatusKind.DidNotTakeEffect || IsCutShort
        ? "undetermined"
        : VerdictKind switch
        {
            VerdictKind.Works => "works",
            VerdictKind.Partial => "partial",
            VerdictKind.Fails => "fails",
            _ => "undetermined",
        };

    internal static bool IsTerminal(SessionStatusKind kind) => kind
        is SessionStatusKind.Ended
        or SessionStatusKind.Refused
        or SessionStatusKind.DidNotTakeEffect
        or SessionStatusKind.CoreStopped
        or SessionStatusKind.Stopped
        or SessionStatusKind.CoreUnresponsive
        or SessionStatusKind.Error;
}
