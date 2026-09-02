namespace ChronoMock.App;

/// <summary>
/// The session's lifecycle state, as the panel shows it. The meaning is carried by the translated status
/// LABEL (zasady/13 section 9 - never colour alone); this enum lets the view guard against a late heartbeat
/// resurrecting a finished session, and could later drive a status glyph or colour without changing the VM.
/// </summary>
public enum SessionStatusKind
{
    /// <summary>Nothing started yet.</summary>
    Idle,

    /// <summary>The core is spawning and the handshake is being checked, before the target is launched.</summary>
    Connecting,

    /// <summary>The session is live and the two clocks are ticking off the state heartbeat.</summary>
    Running,

    /// <summary>The session ended normally (the target exited or the user ended it).</summary>
    Ended,

    /// <summary>The target vanished before the swap took hold (ADR-4, single-instance suspected).</summary>
    DidNotTakeEffect,

    /// <summary>The core closed its stream mid-session - the target's time is now frozen (docs/08 section 7).</summary>
    CoreStopped,

    /// <summary>The opening verdict said the substitution did not take effect, so the core stopped the
    /// target instead of handing back a session whose evidence would be about the real clock. The tester
    /// can re-run with "run even if it does not work" to override.</summary>
    Refused,

    /// <summary>Stop was pressed and the core is being shut down - the session is no longer live, but its
    /// end has not been recorded yet. A distinct state because the shutdown is not instant (the core gets a
    /// grace period), and during it the in-flight controls must already be gone: sending on a closing
    /// stream throws, and "Running" with dead buttons reads as a hang.</summary>
    Stopping,

    /// <summary>The user stopped the session (the Stop control). The core was ended, its hook self-detached,
    /// and the target returned to real time (chrono-mock 7.2, M-10).</summary>
    Stopped,

    /// <summary>The idle watchdog fired - the core stopped emitting its ~1 s heartbeat, so it was stopped as
    /// unresponsive and the target returned to real time (M-10).</summary>
    CoreUnresponsive,

    /// <summary>Setup or the protocol failed - the reason is in the status label and the error detail.</summary>
    Error,
}
