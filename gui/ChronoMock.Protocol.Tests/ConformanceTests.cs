using ChronoMock.Protocol;

namespace ChronoMock.Protocol.Tests;

/// <summary>
/// End-to-end conformance of the C# client against the REAL core (x64), driving the REAL test target
/// with real injection on the host (an own target, allowed there). Proves the client handshakes, relays
/// the two clocks, reports the verdict, and its exit code equals the session verdict - the whole 3.1
/// walking skeleton, measured not assumed. Reads are bounded by a timeout so a hung core fails loudly.
/// <para>
/// Environmentally coupled (RELEASE-010): needs a release-built x64 core, the test target, and injection
/// allowed (Defender off on the host). Tagged Category=Integration so the hermetic gate
/// (<c>dotnet test --filter "Category!=Integration"</c>) can run everywhere while these run only on a
/// prepared Windows host, alongside the native harness (tools/probes/run-targets.ps1).
/// </para>
/// </summary>
[Trait("Category", "Integration")]
public class ConformanceTests
{
    private static readonly TimeSpan ReadTimeout = TimeSpan.FromSeconds(20);
    private const string Moment = "2038-01-19T03:14:07";

    [Fact]
    public async Task Frozen_holds_the_fake_clock_at_the_requested_moment()
    {
        var (core, target) = Fixture();
        var start = StartAt(target, mode: "frozen", multiplier: null);

        await using var client = CoreClient.Launch(core, start);
        var events = await ReadUntilAsync(client, e => e.OfType<StateEvent>().Count() >= 2, ReadTimeout);

        var ready = events.OfType<ReadyEvent>().First();
        Assert.Equal("x64", ready.Bitness);
        Assert.Equal(ProtocolJson.ProtocolVersion, ready.Protocol);

        // Frozen: both heartbeats read exactly the requested moment - the core's own clock does not
        // advance. This is independent of what the target reads, so it is a real oracle, not a tautology.
        var states = events.OfType<StateEvent>().Take(2).ToList();
        Assert.All(states, s => Assert.Equal(Moment, s.Fake.Wall));

        client.Send(new EndCommand { Id = 99 });
        _ = await ReadUntilAsync(client, e => e.Any(x => x is EndedEvent), ReadTimeout);
    }

    [Fact]
    public async Task X60_advances_the_fake_clock_faster_and_reports_works_with_exit_zero()
    {
        var (core, target) = Fixture();
        var start = StartAt(target, mode: "multiplier", multiplier: 60);

        await using var client = CoreClient.Launch(core, start);

        var beforeEnd = await ReadUntilAsync(client, e => e.OfType<StateEvent>().Count() >= 2, ReadTimeout);
        var states = beforeEnd.OfType<StateEvent>().Take(2).ToList();

        // Cumulative elapsed from the anchor: fake advances ~60x real. Generous band for timing jitter.
        var second = states[1];
        Assert.True(second.ElapsedRealMs > 0, "real elapsed must be positive");
        var ratio = (double)second.ElapsedFakeMs / second.ElapsedRealMs;
        Assert.InRange(ratio, 30.0, 120.0);
        Assert.NotEqual(states[0].Fake.Wall, states[1].Fake.Wall); // the fake clock moved

        client.Send(new EndCommand { Id = 99 });
        var tail = await ReadUntilAsync(client, e => e.Any(x => x is EndedEvent), ReadTimeout);

        // The .NET target reads a covered wall channel, so the family verdict is `works`.
        var verdict = tail.OfType<SessionVerdictEvent>().Single();
        Assert.Equal("works", verdict.Verdict);

        var exit = await client.WaitForExitAsync();
        Assert.Equal(0, exit); // works -> exit 0 (docs/08 section 8)
    }

    [Fact]
    public async Task Gated_connect_checks_ready_before_it_launches_then_drives_the_session()
    {
        var (core, target) = Fixture();

        // Connect (spawn only, no start), gate on ready, THEN commit to launching the target - the safety
        // the ready-first fix unlocked (docs/08 section 3), proven end to end against the real core.
        await using var client = CoreClient.Connect(core);

        var untilReady = await ReadUntilAsync(client, e => e.Any(x => x is ReadyEvent), ReadTimeout);
        var ready = untilReady.OfType<ReadyEvent>().Single();
        var gate = HandshakeGate.Check(ready, ProtocolJson.ProtocolVersion, PeReader.Machine.X64);
        Assert.True(gate.IsOk);

        client.Send(StartAt(target, mode: "multiplier", multiplier: 60));
        var running = await ReadUntilAsync(client, e => e.OfType<StateEvent>().Count() >= 2, ReadTimeout);
        Assert.True(running.OfType<StateEvent>().Count() >= 2);

        client.Send(new EndCommand { Id = 99 });
        _ = await ReadUntilAsync(client, e => e.Any(x => x is EndedEvent), ReadTimeout);
    }

    [Fact]
    public async Task Set_multiplier_in_flight_changes_the_rate()
    {
        var (core, target) = Fixture();
        await using var client = CoreClient.Launch(core, StartAt(target, mode: "multiplier", multiplier: 60));

        // Let the fast rate run, then slow it to real time in flight (the control the GUI's buttons send).
        _ = await ReadUntilAsync(client, e => e.OfType<StateEvent>().Count() >= 2, ReadTimeout);
        client.Send(new SetMultiplierCommand { Id = 50, Multiplier = 1 });

        // The core acks and re-emits state, so a later state reports the new multiplier.
        var after = await ReadUntilAsync(
            client, e => e.OfType<StateEvent>().Any(s => s.Multiplier == 1), ReadTimeout);
        Assert.Contains(after.OfType<StateEvent>(), s => s.Multiplier == 1);

        client.Send(new EndCommand { Id = 99 });
        _ = await ReadUntilAsync(client, e => e.Any(x => x is EndedEvent), ReadTimeout);
    }

    [Fact]
    public async Task Jump_moves_the_fake_clock_by_a_relative_delta()
    {
        var (core, target) = Fixture();
        await using var client = CoreClient.Launch(core, StartAt(target, mode: "frozen", multiplier: null));

        // Frozen holds the fake clock at the moment, so a jump is unambiguous to observe.
        var before = await ReadUntilAsync(client, e => e.OfType<StateEvent>().Any(), ReadTimeout);
        Assert.Equal(Moment, before.OfType<StateEvent>().Last().Fake.Wall);

        client.Send(new JumpCommand { Id = 60, To = new MomentSpec { Kind = "relative", Delta = "+1d" } });

        // The fake wall advances one day (2038-01-19 -> 2038-01-20), the control the GUI's jump buttons send.
        var after = await ReadUntilAsync(
            client,
            e => e.OfType<StateEvent>().Any(s => s.Fake.Wall.StartsWith("2038-01-20", StringComparison.Ordinal)),
            ReadTimeout);
        Assert.Contains(
            after.OfType<StateEvent>(), s => s.Fake.Wall.StartsWith("2038-01-20", StringComparison.Ordinal));

        client.Send(new EndCommand { Id = 99 });
        _ = await ReadUntilAsync(client, e => e.Any(x => x is EndedEvent), ReadTimeout);
    }

    private static (string core, string target) Fixture()
    {
        var repo = RepoPaths.RepoRoot();
        return (RepoPaths.X64Core(repo), RepoPaths.TestTargetExe(repo));
    }

    private static StartCommand StartAt(string target, string mode, long? multiplier) => new()
    {
        Id = 1,
        Target = new TargetSpec { Path = target },
        Time = new TimeSpec
        {
            Moment = new MomentSpec { Kind = "absolute", Local = Moment, TzBiasMin = 0 },
            Mode = mode,
            Multiplier = multiplier,
        },
    };

    /// <summary>
    /// Read events until <paramref name="done"/> holds, then return everything collected so far. A
    /// timeout (hung core) or an early end of stream (core exited without the expected events) both throw
    /// loudly, with the collected event names and the core's diagnostics attached.
    /// </summary>
    private static async Task<List<ChronoEvent>> ReadUntilAsync(
        CoreClient client, Func<IReadOnlyList<ChronoEvent>, bool> done, TimeSpan timeout)
    {
        var collected = new List<ChronoEvent>();
        using var cts = new CancellationTokenSource(timeout);
        try
        {
            await foreach (var evt in client.Events.ReadAllAsync(cts.Token))
            {
                collected.Add(evt);
                if (done(collected))
                {
                    return collected;
                }
            }
        }
        catch (OperationCanceledException)
        {
            throw new TimeoutException(
                $"core did not produce the expected events within {timeout.TotalSeconds:0}s. "
                + Describe(collected, client));
        }

        throw new InvalidOperationException(
            "core stream ended before the expected events. " + Describe(collected, client));
    }

    private static string Describe(IReadOnlyList<ChronoEvent> collected, CoreClient client)
        => $"Got {collected.Count} event(s): [{string.Join(", ", collected.Select(e => e.GetType().Name))}]. "
           + $"Diagnostics: [{string.Join(" | ", client.Diagnostics)}]";
}
