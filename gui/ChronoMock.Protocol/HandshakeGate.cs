namespace ChronoMock.Protocol;

/// <summary>
/// The client-side handshake gate (docs/08 section 3). Before the client lets the core launch the target,
/// it checks the core's <c>ready</c> against what it expects - the protocol version it speaks, and that the
/// core's self-reported bitness matches the bitness the client launched it for (which <see cref="CoreLocator"/>
/// picked from the target's PE). A mismatch is a usage error surfaced BEFORE the target is ever started,
/// never a guessed default - "declare a known impossibility before the attempt, not after" (zasady/13 section 11).
/// <para>
/// The ready-first ordering (fixed in 97eae17) makes this safe: the core emits <c>ready</c> before it reads
/// its first command, so a client can await <c>ready</c> and decide, without risking a deadlock.
/// </para>
/// Pure and free of I/O, so the decision is unit-testable without a core process.
/// </summary>
public static class HandshakeGate
{
    public enum GateOutcome
    {
        Ok,
        ProtocolMismatch,
        BitnessMismatch,
    }

    /// <summary>The gate decision. <see cref="ReasonKey"/> is a stable translation key on a mismatch, null on Ok.</summary>
    public sealed record Result(GateOutcome Outcome, string? ReasonKey)
    {
        public bool IsOk => Outcome == GateOutcome.Ok;
    }

    // Translation keys (untouchable rules 15/16): the client renders these, prose never travels on the wire.
    public const string ProtocolMismatchKey = "handshake.protocol_mismatch";
    public const string BitnessMismatchKey = "handshake.bitness_mismatch";

    /// <summary>
    /// Check a <c>ready</c> event against the protocol version the client speaks and the bitness it launched
    /// the core for. The caller must NOT send <c>start</c> unless the result <see cref="Result.IsOk"/>.
    /// </summary>
    public static Result Check(ReadyEvent ready, int expectedProtocol, PeReader.Machine expectedMachine)
    {
        ArgumentNullException.ThrowIfNull(ready);

        if (ready.Protocol != expectedProtocol)
        {
            return new Result(GateOutcome.ProtocolMismatch, ProtocolMismatchKey);
        }

        if (!BitnessMatches(ready.Bitness, expectedMachine))
        {
            return new Result(GateOutcome.BitnessMismatch, BitnessMismatchKey);
        }

        return new Result(GateOutcome.Ok, null);
    }

    private static bool BitnessMatches(string reported, PeReader.Machine expected) => expected switch
    {
        PeReader.Machine.X64 => reported == "x64",
        PeReader.Machine.X86 => reported == "x86",
        _ => false,
    };
}
