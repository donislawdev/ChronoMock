using System.Linq;
using ChronoMock.App;

namespace ChronoMock.App.Tests;

/// <summary>
/// Which of the three phases the window shows. It is keyed to the session STATUS, never to IsIdle, and
/// exactly one phase is shown in every state.
/// </summary>
/// <remarks>
/// 🔴 THE LANDMINE THIS GUARDS. A finished session sets IsIdle true again - the setup form unlocks so the
/// tester can edit and re-run - while its status stays terminal. A selector reading IsIdle would throw the
/// reader back to the setup screen the instant a session ended, losing the verdict the whole session was run
/// to produce. The state factories reach running and terminal states through Apply, which never clears
/// _idle (only a real StartAsync does), so every view model here reads IsIdle true - which is exactly what
/// makes them the right probe for a selector that must ignore it.
/// </remarks>
public class PhaseSelectorTests
{
    [Fact]
    public void Idle_shows_the_setup_phase()
        => AssertOnlyPhase(new SessionViewModel(), setup: true, session: false, result: false);

    [Fact]
    public void A_running_session_shows_the_session_phase_even_though_it_reads_idle()
    {
        var vm = SessionStates.Running();
        Assert.True(vm.IsIdle); // a selector on IsIdle would show setup over a live session - it must not
        AssertOnlyPhase(vm, setup: false, session: true, result: false);
    }

    [Fact]
    public void A_running_session_with_an_in_flight_error_stays_the_session_phase()
        => AssertOnlyPhase(SessionStates.InFlightError(), setup: false, session: true, result: false);

    [Fact]
    public void A_finished_session_shows_the_result_phase_even_though_it_reads_idle()
    {
        var vm = SessionStates.Ended();
        Assert.True(vm.IsIdle); // the form has unlocked to re-run, but the status stays terminal
        AssertOnlyPhase(vm, setup: false, session: false, result: true);
    }

    [Fact]
    public void Every_terminal_state_shows_the_result_phase()
    {
        AssertOnlyPhase(SessionStates.Ended(), setup: false, session: false, result: true);
        AssertOnlyPhase(SessionStates.Refused(), setup: false, session: false, result: true);
        AssertOnlyPhase(SessionStates.TargetVanished(), setup: false, session: false, result: true);
        AssertOnlyPhase(SessionStates.StartError(), setup: false, session: false, result: true);
    }

    private static void AssertOnlyPhase(SessionViewModel vm, bool setup, bool session, bool result)
    {
        Assert.Equal(setup, vm.ShowsSetupPhase);
        Assert.Equal(session, vm.ShowsSessionPhase);
        Assert.Equal(result, vm.ShowsResultPhase);

        // The three partition every status, so the host never shows two views at once or a blank cell.
        var shown = new[] { vm.ShowsSetupPhase, vm.ShowsSessionPhase, vm.ShowsResultPhase }.Count(on => on);
        Assert.Equal(1, shown);
    }
}
