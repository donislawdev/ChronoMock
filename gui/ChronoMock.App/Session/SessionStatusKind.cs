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

    /// <summary>Setup or the protocol failed - the reason is in the status label and the error detail.</summary>
    Error,
}
