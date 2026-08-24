using ChronoMock.Protocol;

namespace ChronoMock.Protocol.Tests;

/// <summary>
/// End-to-end conformance of the C# client against the REAL core (x64), driving the REAL test target
/// with real injection on the host (an own target, allowed there). Proves the client handshakes, relays
/// the two clocks, reports the verdict, and its exit code equals the session verdict - the whole 3.1
/// walking skeleton, measured not assumed. Reads are bounded by a timeout so a hung core fails loudly.
/// </summary>
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
