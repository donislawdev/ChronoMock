using ChronoMock.App;
using ChronoMock.Protocol;

namespace ChronoMock.App.Tests;

/// <summary>
/// The event-to-panel mapping (SessionViewModel.Apply). Pure and synchronous, so it is tested without a
/// core process or a UI thread: a state heartbeat fills both clocks with explicit zones, and a terminal
/// outcome cannot be resurrected by a late heartbeat.
/// </summary>
public class SessionViewModelTests
{
    private static StateEvent State(string fakeWall, string realWall, int bias, long multiplier) => new()
    {
        V = ProtocolJson.ProtocolVersion,
        Fake = new Clock { Wall = fakeWall, ZoneBiasMin = bias },
        Real = new Clock { Wall = realWall, ZoneBiasMin = bias },
        Multiplier = multiplier,
    };

    [Fact]
    public void State_event_fills_both_clocks_with_explicit_zones_and_rate()
    {
        var vm = new SessionViewModel();

        vm.Apply(State("2038-01-19T03:14:07", "2026-08-24T20:30:00", bias: -120, multiplier: 60));

        Assert.Equal("2038-01-19T03:14:07", vm.Fake.Wall);
        Assert.Equal("2038-01-19", vm.Fake.Date); // split onto two lines so a long ISO value never wraps
        Assert.Equal("03:14:07", vm.Fake.Time);
        Assert.Equal("UTC+02:00", vm.Fake.Zone);
        Assert.Equal("2026-08-24T20:30:00", vm.Real.Wall);
        Assert.Equal("2026-08-24", vm.Real.Date);
        Assert.Equal("20:30:00", vm.Real.Time);
        Assert.Equal("UTC+02:00", vm.Real.Zone);
        Assert.Equal("x60", vm.MultiplierText);
        Assert.Equal(SessionStatusKind.Running, vm.StatusKind);
    }

    [Fact]
    public void Vanished_marks_did_not_take_effect_and_is_not_resurrected_by_a_late_state()
    {
        var vm = new SessionViewModel();

        vm.Apply(new VanishedEvent
        {
            V = ProtocolJson.ProtocolVersion,
            Pid = 1234,
            ReasonKey = "target.single_instance_suspected",
            LivedMs = 10,
        });
        Assert.Equal(SessionStatusKind.DidNotTakeEffect, vm.StatusKind);

        // A heartbeat arriving after a terminal outcome must not flip the panel back to "running".
        vm.Apply(State("x", "y", bias: 0, multiplier: 1));
        Assert.Equal(SessionStatusKind.DidNotTakeEffect, vm.StatusKind);
    }

    [Fact]
    public void Ended_marks_the_session_ended()
    {
        var vm = new SessionViewModel();

        vm.Apply(new EndedEvent { V = ProtocolJson.ProtocolVersion, Clean = true });

        Assert.Equal(SessionStatusKind.Ended, vm.StatusKind);
        Assert.Equal("status.ended", vm.StatusKey);
    }

    [Fact]
    public void Starts_idle_before_any_event()
    {
        var vm = new SessionViewModel();

        Assert.Equal(SessionStatusKind.Idle, vm.StatusKind);
        Assert.True(vm.CanStart);
        Assert.Equal("clock.fake", vm.Fake.RoleKey);
        Assert.Equal("clock.real", vm.Real.RoleKey);
    }
}
