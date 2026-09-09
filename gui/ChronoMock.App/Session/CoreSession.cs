using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using System.Threading.Channels;
using ChronoMock.Protocol;

namespace ChronoMock.App;

/// <summary>
/// One connected core process and everything the protocol asks of a client: the handshake, sending
/// commands to a core that may already be gone, pumping events under an idle watchdog, and shutdown.
/// <para>
/// It exists because <see cref="SessionViewModel"/> was doing all of that itself, in among projecting
/// state onto the panel. The split is the same one the Rust half already makes and proves - a driver
/// that runs the session, a collector that records it, a renderer that reports it - so this is the
/// shape of the product rather than a new idea. What the view model keeps is the part that is about
/// the VIEW: which translation key a refusal maps to, and what each event does to the two clocks.
/// </para>
/// <para>
/// The concrete thing it fixed: the guard around an in-flight send existed THREE times, in
/// <c>SendMultiplier</c>, <c>SendJump</c> and <c>SendJumpAbsolute</c>, with the same four lines of
/// comment word for word. Code written three times gets corrected once. It lives in
/// <see cref="SendInFlight"/> now.
/// </para>
/// </summary>
internal sealed class CoreSession : IAsyncDisposable
{
    private readonly CoreClient _client;

    private CoreSession(CoreClient client) => _client = client;

    /// <summary>The core's event stream. Pump it with <see cref="PumpAsync"/>.</summary>
    internal ChannelReader<ChronoEvent> Events => _client.Events;

    /// <summary>The core's stderr, drained by dispose and captured for support (RELEASE-012).</summary>
    internal IReadOnlyCollection<string> Diagnostics => _client.Diagnostics;

    /// <summary>How many stderr lines were dropped to stay under the client's cap - so a captured block
    /// can say it is a tail rather than the whole of it (rule 6).</summary>
    internal int DiagnosticsDropped => _client.DiagnosticsDropped;

    /// <summary>
    /// Connect to the core and complete the handshake. The returned session is ALWAYS non-null once the
    /// core process exists, even when the handshake refuses - the caller owns disposal either way, and a
    /// refused session still has stderr worth capturing (RELEASE-012). <c>RefusalKey</c> is the
    /// translation key to show, or null when the handshake passed.
    /// <para>
    /// Failing to spawn the core at all throws, exactly as <see cref="CoreClient.Connect"/> does, so the
    /// caller can still tell a broken core install from a rejected handshake (RELEASE-007).
    /// </para>
    /// </summary>
    internal static async Task<CoreSessionOpen> OpenAsync(SessionPlan plan, TimeSpan readyTimeout)
    {
        ArgumentNullException.ThrowIfNull(plan);
        var session = new CoreSession(CoreClient.Connect(plan.CorePath));

        var ready = await ReadReadyAsync(session._client, readyTimeout);
        return new CoreSessionOpen(session, RefusalKeyFor(ready, plan.Machine, plan.IsCdp));
    }

    /// <summary>
    /// The translation key a handshake refuses with, or null when it passed. A core that never said
    /// <c>ready</c> is its own refusal - the caller cannot tell the difference from the key alone and does
    /// not need to, because both refuse before the target is ever launched.
    /// <para>
    /// Pure and separate from <see cref="OpenAsync"/> so the wiring is unit tested without a core process.
    /// <see cref="HandshakeGate"/> has its own tests, but nothing tested that this call passes
    /// <c>checkBitness: false</c> for a Chromium session - and that is not cosmetic: a CDP session does not
    /// inject, so the core's bitness genuinely does not apply, and checking it would refuse a combination
    /// that works (docs/08 section 3, ADR-9).
    /// </para>
    /// </summary>
    internal static string? RefusalKeyFor(ReadyEvent? ready, PeReader.Machine machine, bool isCdp)
    {
        if (ready is null)
        {
            return "status.no_ready";
        }

        var gate = HandshakeGate.Check(ready, ProtocolJson.ProtocolVersion, machine, checkBitness: !isCdp);
        return gate.IsOk ? null : gate.ReasonKey;
    }

    /// <summary>
    /// Send a command whose failure is the caller's problem - the start command, whose loss would leave
    /// the session waiting for a target that was never launched.
    /// </summary>
    internal void Send(Command command) => _client.Send(command);

    /// <summary>
    /// Send an in-flight command (jump, set_multiplier) and swallow the one failure that is not a fault:
    /// the core is already gone, or its stdin was disposed by a Stop racing this click. Neither is worth a
    /// dialog - the read loop surfaces the end.
    /// <para>
    /// 🔴 <see cref="ObjectDisposedException"/> derives from <see cref="InvalidOperationException"/> and NOT
    /// from <see cref="IOException"/>, so catching IOException alone let it reach the dispatcher handler and
    /// pop a message box after a perfectly ordinary Stop. That is why all three are named here.
    /// </para>
    /// </summary>
    internal void SendInFlight(Command command)
    {
        try
        {
            _client.Send(command);
        }
        catch (Exception e) when (IsGoneAway(e))
        {
        }
    }

    /// <summary>
    /// Whether a failure to send means the core has simply gone, rather than something being wrong.
    /// <para>
    /// A named predicate rather than a filter written inline, because this exact list is what was once
    /// wrong and nothing could have said so: the filter caught <see cref="IOException"/> alone, and a
    /// perfectly ordinary Stop popped a message box. Named, it can be tested directly.
    /// </para>
    /// <para>
    /// 🔴 <see cref="ObjectDisposedException"/> is REDUNDANT here and stays on purpose. It derives from
    /// <see cref="InvalidOperationException"/>, so the last term already matches it - naming it is what
    /// tells the next reader which failure this list was written for, and removing the base type would
    /// narrow the filter without looking like it had. Nothing can test the middle term on its own, and
    /// saying so is better than a test that appears to and does not.
    /// </para>
    /// </summary>
    internal static bool IsGoneAway(Exception error) =>
        error is IOException or ObjectDisposedException or InvalidOperationException;

    /// <summary>
    /// Relay events until the stream ends, resetting an idle watchdog on each one. Returns <c>true</c> if
    /// the watchdog fired - no event for <paramref name="idleTimeout"/>, i.e. the core stopped emitting its
    /// ~1 s heartbeat and is treated as hung (M-10) - and <c>false</c> if the stream completed normally (the
    /// core exited or was disposed).
    /// <para>
    /// Static and taking the reader rather than reading <see cref="Events"/>, so the watchdog is unit tested
    /// with a fake channel and a short timeout, no core process needed. No ConfigureAwait anywhere, so
    /// <paramref name="onEvent"/> stays on the caller's (UI) thread.
    /// </para>
    /// </summary>
    internal static async Task<bool> PumpAsync(
        ChannelReader<ChronoEvent> events, Action<ChronoEvent> onEvent, TimeSpan idleTimeout)
    {
        ArgumentNullException.ThrowIfNull(events);
        ArgumentNullException.ThrowIfNull(onEvent);

        using var idleCts = new CancellationTokenSource();
        try
        {
            while (true)
            {
                idleCts.CancelAfter(idleTimeout); // (re)arm the idle window before each wait
                if (!await events.WaitToReadAsync(idleCts.Token))
                {
                    return false; // the stream completed - the core exited or was disposed (e.g. by Stop)
                }

                while (events.TryRead(out var evt))
                {
                    onEvent(evt);
                }
            }
        }
        catch (OperationCanceledException)
        {
            return true; // the idle watchdog fired - no event within the window
        }
    }

    /// <summary>Read events until the <c>ready</c> handshake, or null on timeout or an early end of stream.</summary>
    private static async Task<ReadyEvent?> ReadReadyAsync(CoreClient client, TimeSpan timeout)
    {
        using var cts = new CancellationTokenSource(timeout);
        try
        {
            await foreach (var evt in client.Events.ReadAllAsync(cts.Token))
            {
                if (evt is ReadyEvent ready)
                {
                    return ready;
                }
            }
        }
        catch (OperationCanceledException)
        {
            return null; // timed out waiting for ready - the caller reports it, never hangs
        }

        return null; // the stream ended before ready arrived
    }

    /// <summary>
    /// Stop the core. It blocks briefly (up to the core's grace period, waiting for it to exit before
    /// killing it), so the wait is pushed off the calling thread - the UI thread must not stall on it.
    /// Disposing twice is safe, and happens by design: Stop disposes to end the session, and the driver
    /// disposes again in its finally.
    /// </summary>
    public async ValueTask DisposeAsync() => await Task.Run(() => _client.DisposeAsync().AsTask());
}

/// <summary>
/// The result of <see cref="CoreSession.OpenAsync"/>: the session, which exists whether or not the
/// handshake passed, and the translation key to show when it did not.
/// </summary>
/// <param name="Session">The connected core. The caller owns it and must dispose it.</param>
/// <param name="RefusalKey">Why the handshake refused, or null when it passed.</param>
internal sealed record CoreSessionOpen(CoreSession Session, string? RefusalKey);
