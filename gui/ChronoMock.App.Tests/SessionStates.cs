using ChronoMock.App;
using ChronoMock.Protocol;

namespace ChronoMock.App.Tests;

/// <summary>
/// The panel in each of the states it actually spends its life in, built by applying protocol events to
/// a view model rather than by running a session.
/// </summary>
/// <remarks>
/// 🔴 WHY THIS EXISTS. The panel and the calculator carry about seventy regions switched by a binding -
/// 31 and 22 on Visibility, 13 and 1 on IsEnabled, plus style triggers. Reaching any of them in a live
/// window means driving a real session to that point: spawning the core, waiting for a verdict,
/// provoking a failure. In practice that means exactly one state ever gets looked at, the empty one at
/// startup, and every other is judged from source. That is how an interface ends up correct in the
/// states nobody sees and wrong in the states users live in.
///
/// The seam this stands on was already here: <c>new SessionViewModel()</c> takes no core, and
/// <c>Apply</c> takes one protocol event. The session view model tests have driven states this way all
/// along. Nothing new was added to the app to make the sheet possible, which is the point - a state that
/// needs a special production hook is a state the production code does not really have.
/// </remarks>
internal static class SessionStates
{
    /// <summary>A session under way at sixty times real speed, both clocks live.</summary>
    public static SessionViewModel Running()
    {
        var vm = new SessionViewModel();
        vm.Apply(State());
        return vm;
    }

    /// <summary>Running, with coverage reported and two warnings the reader has to be able to act on.</summary>
    public static SessionViewModel RunningWithCoverageWarnings()
    {
        var vm = Running();
        vm.Apply(new CoverageEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Pid = 4242,
            Covered =
            [
                new CoveredChannel { Channel = "GetSystemTimeAsFileTime", Calls = 128_004 },
                new CoveredChannel { Channel = "GetLocalTime", Calls = 3_517 },
                new CoveredChannel { Channel = "GetTickCount64", Calls = 44_910 },
            ],
            Uncovered = ["QueryPerformanceCounter"],
            Unobserved = ["NtQuerySystemTime"],
            WarningKeys = ["wait.timeout_collapsed", "coverage.channel_installed_late"],
        });
        return vm;
    }

    /// <summary>A finished session that worked, which is the state a report gets copied from.</summary>
    public static SessionViewModel Ended()
    {
        var vm = RunningWithCoverageWarnings();
        vm.Apply(new SessionVerdictEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Verdict = "works",
            ReasonKey = "verdict.time_channels_covered",
            ProcessCount = 2,
            WarningKeys = ["coverage.pid_registry_full"],
        });
        vm.Apply(new EndedEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Clean = true,
            TargetExitCode = 0,
            ElapsedRealMs = 61_000,
            ElapsedFakeMs = 3_660_000,
            FakeEndWall = "2038-01-19T04:15:07",
        });
        return vm;
    }

    /// <summary>The core refusing to start, which is the loudest thing the panel ever has to say.</summary>
    public static SessionViewModel Refused()
    {
        var vm = new SessionViewModel();
        vm.Apply(new VerdictEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Verdict = "fails",
            ReasonKey = "coverage.time_channels_uncovered",
            RefuseStart = true,
        });
        return vm;
    }

    /// <summary>Running, with one command rejected: the session lives, the refusal still has to show.</summary>
    public static SessionViewModel InFlightError()
    {
        var vm = Running();
        vm.Apply(new ErrorEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Id = 11,
            Code = 1,
            Key = "moment.invalid",
            Origin = "core",
        });
        return vm;
    }

    /// <summary>The target gone before the hook could take, which is a failure with a named reason.</summary>
    public static SessionViewModel TargetVanished()
    {
        var vm = Running();
        vm.Apply(new VanishedEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Pid = 4242,
            ReasonKey = "target.single_instance_handoff",
            LivedMs = 180,
        });
        return vm;
    }

    /// <summary>A start that never got off the ground, reported as itself rather than as a stall.</summary>
    public static SessionViewModel StartError()
    {
        var vm = new SessionViewModel();
        vm.Apply(new ErrorEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Id = 1,
            Code = 3,
            Key = "target.inject_failed",
            Origin = "core",
        });
        return vm;
    }

    /// <summary>Both clocks and the rate, in the shape the session view model tests use.</summary>
    private static StateEvent State() => new()
    {
        V = ProtocolJson.ProtocolVersion,
        Fake = new Clock { Wall = "2038-01-19T03:14:07", ZoneBiasMin = -120 },
        Real = new Clock { Wall = "2026-09-09T20:30:00", ZoneBiasMin = -120 },
        Multiplier = 60,
        ElapsedFakeMs = 3_600_000,
        ElapsedRealMs = 60_000,
    };
}
