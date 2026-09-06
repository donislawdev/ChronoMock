using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using ChronoMock.Protocol;

namespace ChronoMock.App.Tests;

/// <summary>
/// The two decisions <see cref="CoreSession"/> makes that a core process would otherwise be needed to
/// reach, and that nothing tested before the driving moved out of <see cref="SessionViewModel"/>.
/// <para>
/// Measured, not assumed: with 299 tests passing, the suite named neither <c>status.no_ready</c>, nor
/// the handshake wiring, nor the exception filter around an in-flight send. A refactor that moves
/// untested behaviour leaves it untested somewhere else, so these were written with it.
/// </para>
/// </summary>
public class CoreSessionTests
{
    private static ReadyEvent Ready(int protocol, string bitness) =>
        new() { Protocol = protocol, CoreVersion = "0.1.0", Bitness = bitness };

    /// <summary>A core that never announced itself refuses on its own, before any gate runs.</summary>
    [Fact]
    public void A_core_that_never_said_ready_refuses_with_its_own_key()
    {
        Assert.Equal(
            "status.no_ready",
            CoreSession.RefusalKeyFor(null, PeReader.Machine.X64, isCdp: false));
    }

    [Fact]
    public void A_matching_handshake_does_not_refuse()
    {
        Assert.Null(CoreSession.RefusalKeyFor(
            Ready(ProtocolJson.ProtocolVersion, "x64"), PeReader.Machine.X64, isCdp: false));
    }

    [Fact]
    public void A_protocol_mismatch_refuses_with_the_gate_s_key()
    {
        Assert.Equal(
            HandshakeGate.ProtocolMismatchKey,
            CoreSession.RefusalKeyFor(
                Ready(ProtocolJson.ProtocolVersion + 1, "x64"), PeReader.Machine.X64, isCdp: false));
    }

    /// <summary>
    /// The half that had no test at all, and the one that would fail silently in the useful direction:
    /// a Chromium session must NOT be refused for a bitness that does not apply to it, because nothing
    /// is injected there (docs/08 section 3, ADR-9). The same ready event is checked both ways, so the
    /// only difference between the two assertions is the flag under test.
    /// </summary>
    [Fact]
    public void A_chromium_session_ignores_a_bitness_a_native_session_would_refuse()
    {
        var ready = Ready(ProtocolJson.ProtocolVersion, "x64");

        Assert.Equal(
            HandshakeGate.BitnessMismatchKey,
            CoreSession.RefusalKeyFor(ready, PeReader.Machine.X86, isCdp: false));
        Assert.Null(CoreSession.RefusalKeyFor(ready, PeReader.Machine.X86, isCdp: true));
    }

    /// <summary>
    /// The filter around an in-flight send. This list was once wrong in a way no test could see:
    /// <see cref="ObjectDisposedException"/> derives from <see cref="InvalidOperationException"/> and NOT
    /// from <see cref="IOException"/>, so catching IOException alone let an ordinary Stop reach the
    /// dispatcher and pop a message box.
    /// </summary>
    [Fact]
    public void A_core_that_has_gone_is_recognised_by_all_three_of_its_shapes()
    {
        Assert.True(CoreSession.IsGoneAway(new IOException()));
        Assert.True(CoreSession.IsGoneAway(new ObjectDisposedException("core")));
        Assert.True(CoreSession.IsGoneAway(new InvalidOperationException()));
    }

    /// <summary>
    /// The other half, and it is not decoration: a predicate that answered true to everything would
    /// satisfy the test above perfectly while swallowing the bug the send is meant to surface.
    /// </summary>
    [Fact]
    public void Anything_else_is_not_a_core_that_has_gone()
    {
        Assert.False(CoreSession.IsGoneAway(new NotSupportedException()));
        Assert.False(CoreSession.IsGoneAway(new FormatException()));
    }
}
