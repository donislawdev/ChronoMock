using ChronoMock.Protocol;

namespace ChronoMock.Protocol.Tests;

/// <summary>
/// The handshake gate (docs/08 section 3): the client refuses to launch the target when the core's ready
/// handshake does not match the protocol it speaks or the bitness it launched the core for. Pure, so it is
/// tested without a process - the mismatch is a keyed usage error, never a guessed default.
/// </summary>
public class HandshakeGateTests
{
    private static ReadyEvent Ready(int protocol, string bitness) => new()
    {
        V = ProtocolJson.ProtocolVersion,
        Protocol = protocol,
        CoreVersion = "0.1.0",
        Bitness = bitness,
    };

    [Fact]
    public void Ok_when_protocol_and_bitness_match()
    {
        var result = HandshakeGate.Check(
            Ready(ProtocolJson.ProtocolVersion, "x64"), ProtocolJson.ProtocolVersion, PeReader.Machine.X64);

        Assert.True(result.IsOk);
        Assert.Null(result.ReasonKey);
    }

    [Fact]
    public void Protocol_mismatch_refuses_with_key()
    {
        var result = HandshakeGate.Check(
            Ready(ProtocolJson.ProtocolVersion + 1, "x64"), ProtocolJson.ProtocolVersion, PeReader.Machine.X64);

        Assert.False(result.IsOk);
        Assert.Equal(HandshakeGate.ProtocolMismatchKey, result.ReasonKey);
    }

    [Fact]
    public void Bitness_mismatch_refuses_with_key()
    {
        var result = HandshakeGate.Check(
            Ready(ProtocolJson.ProtocolVersion, "x86"), ProtocolJson.ProtocolVersion, PeReader.Machine.X64);

        Assert.False(result.IsOk);
        Assert.Equal(HandshakeGate.BitnessMismatchKey, result.ReasonKey);
    }

    [Theory]
    [InlineData("x64", PeReader.Machine.X64)]
    [InlineData("x86", PeReader.Machine.X86)]
    public void Matching_bitness_strings_pass(string reported, PeReader.Machine expected)
        => Assert.True(HandshakeGate
            .Check(Ready(ProtocolJson.ProtocolVersion, reported), ProtocolJson.ProtocolVersion, expected).IsOk);

    [Fact]
    public void Unknown_target_machine_never_matches()
    {
        var result = HandshakeGate.Check(
            Ready(ProtocolJson.ProtocolVersion, "x64"), ProtocolJson.ProtocolVersion, PeReader.Machine.Unknown);

        Assert.False(result.IsOk);
        Assert.Equal(HandshakeGate.BitnessMismatchKey, result.ReasonKey);
    }
}
