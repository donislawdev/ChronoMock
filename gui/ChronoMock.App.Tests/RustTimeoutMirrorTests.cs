using System.IO; // The WPF SDK trims System.IO from implicit usings (Path collides with Shapes.Path).
using System.Text.RegularExpressions;

namespace ChronoMock.App.Tests;

/// <summary>
/// The client calls the core unresponsive after <see cref="SessionViewModel.IdleTimeout"/> without
/// an event. The core has its own internal deadlines, and every one of them is a stretch during
/// which it emits nothing - so each must be SHORTER than that watchdog, or a healthy core gets
/// reported as hung.
///
/// It had already gone wrong once, in the least likely place: the CDP call deadline was introduced
/// specifically so a command whose reply never comes could not park the session loop, and its own
/// comment names the risk as "the GUI's 15 s watchdog calling a healthy core unresponsive". The
/// value chosen was 20 s. The guard missed by five seconds, and nothing could notice, because the
/// two numbers live in different languages, different projects and different build systems (R3-3).
///
/// So they are compared here, by reading the Rust sources - the same technique as
/// <see cref="RustConstantMirrorTests"/>. This does not stop anyone changing a deadline; it stops
/// them changing it past the point where the client would misreport the result.
/// </summary>
public class RustTimeoutMirrorTests
{
    private static string ReadRustSource(params string[] relativeParts)
    {
        var path = Path.Combine(new[] { TestPaths.RepoRoot() }.Concat(relativeParts).ToArray());
        Assert.True(File.Exists(path), $"expected a Rust source at '{path}'");
        return File.ReadAllText(path);
    }

    private static double CaptureSeconds(string source, string pattern, string what, double divisor = 1)
    {
        var matches = Regex.Matches(source, pattern);
        Assert.True(
            matches.Count == 1,
            $"expected exactly one match for {what}, found {matches.Count} - the guard is reading the wrong thing");
        var raw = matches[0].Groups[1].Value.Replace("_", string.Empty);
        return double.Parse(raw, System.Globalization.CultureInfo.InvariantCulture) / divisor;
    }

    /// <summary>
    /// A CDP command that never gets a reply. This is the one that was already wrong.
    /// </summary>
    [Fact]
    public void The_cdp_call_deadline_is_shorter_than_the_client_watchdog()
    {
        var mod = ReadRustSource("crates", "cli", "src", "cdp", "mod.rs");
        var seconds = CaptureSeconds(mod, @"pub const CALL_DEADLINE_SECS: u64 = (\d+);", "CALL_DEADLINE_SECS");

        Assert.True(
            seconds < SessionViewModel.IdleTimeout.TotalSeconds,
            $"a stuck CDP call blocks the core for {seconds}s while the client calls it unresponsive after "
                + $"{SessionViewModel.IdleTimeout.TotalSeconds}s - the watchdog would report a healthy core as hung");
    }

    /// <summary>
    /// Preparing a native session: injecting the hook, then the guard window before the first
    /// coverage event. Nothing reaches the client during either.
    /// </summary>
    [Fact]
    public void The_native_prepare_budget_is_shorter_than_the_client_watchdog()
    {
        var mech = ReadRustSource("crates", "mech", "src", "lib.rs");
        var inject = CaptureSeconds(mech, @"const INJECT_TIMEOUT_MS: u32 = ([\d_]+);", "INJECT_TIMEOUT_MS", 1000);
        var guard = CaptureSeconds(mech, @"const GUARD_MS: u32 = ([\d_]+);", "GUARD_MS", 1000);

        Assert.True(
            inject + guard < SessionViewModel.IdleTimeout.TotalSeconds,
            $"a native start can take {inject + guard}s before its first event while the client gives up after "
                + $"{SessionViewModel.IdleTimeout.TotalSeconds}s");
    }

    /// <summary>
    /// Waiting for a launched Chromium to publish its debug port is the longest silence of all, and
    /// it is NOT shorter than the watchdog - deliberately, because shortening it would fail slow
    /// Electron apps that are merely unpacking. It is covered instead by the core beating its
    /// heartbeat from inside the wait, so this checks that the escape hatch is actually there: a
    /// callback that emits state, not a bare sleep loop.
    /// </summary>
    [Fact]
    public void The_port_wait_beats_the_heartbeat_because_it_outlasts_the_watchdog()
    {
        var launch = ReadRustSource("crates", "cli", "src", "cdp", "launch.rs");
        var wait = CaptureSeconds(launch, @"pub const PORT_WAIT_SECS: u64 = (\d+);", "PORT_WAIT_SECS");

        // If someone ever brings this under the watchdog, the heartbeat is no longer load-bearing
        // and this test should be revisited rather than silently kept.
        Assert.True(
            wait >= SessionViewModel.IdleTimeout.TotalSeconds,
            $"the port wait ({wait}s) is now under the watchdog ({SessionViewModel.IdleTimeout.TotalSeconds}s); "
                + "the heartbeat during the wait may no longer be needed - check before deleting this test");

        Assert.True(
            Regex.IsMatch(launch, @"on_wait\(\);"),
            "the port wait no longer calls its progress callback, so a slow launch is silent again");

        var main = ReadRustSource("crates", "cli", "src", "main.rs");
        Assert.True(
            Regex.IsMatch(main, @"launch_chromium\(&target\.path, &target\.args, \|\| \{[^}]*state_event_at", RegexOptions.Singleline),
            "the session no longer emits state while waiting for the debug port");
    }
}
