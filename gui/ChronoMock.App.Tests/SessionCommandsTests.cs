using System.Windows.Input;
using ChronoMock.App;

namespace ChronoMock.App.Tests;

/// <summary>
/// The panel's actions as commands. These check the GATES and the wiring, not the work behind each action -
/// that work is SessionViewModel's and is tested there. A command that runs the core (Start, Speed, Jump)
/// is checked by its CanExecute only, so no test here spawns a process.
/// </summary>
public class SessionCommandsTests
{
    private static SessionRecord Record() => new()
    {
        TargetPath = "app.exe",
        MomentLocal = "2038-01-19T03:14:07",
        TzBiasMin = 0,
        Mode = "flow",
        Verdict = "works",
        EndedAtUtc = "2026-09-14T00:00:00Z",
    };

    [Fact]
    public void Start_is_disabled_without_a_target_and_enabled_with_one()
    {
        var vm = new SessionViewModel();
        Assert.False(vm.Commands.Start.CanExecute(null)); // the shipped default moment is valid, so only the target is missing

        vm.SetTarget("app.exe");

        Assert.True(vm.Commands.Start.CanExecute(null));
    }

    [Fact]
    public void Stop_is_enabled_only_while_running()
    {
        Assert.False(new SessionViewModel().Commands.Stop.CanExecute(null));
        Assert.True(SessionStates.Running().Commands.Stop.CanExecute(null));
    }

    [Fact]
    public void The_in_flight_controls_are_enabled_only_while_running()
    {
        var idle = new SessionViewModel();
        var running = SessionStates.Running();

        Assert.False(idle.Commands.Speed.CanExecute(60L));
        Assert.False(idle.Commands.Jump.CanExecute("+1d"));
        Assert.False(idle.Commands.SetCustomSpeed.CanExecute("500"));

        Assert.True(running.Commands.Speed.CanExecute(60L));
        Assert.True(running.Commands.Jump.CanExecute("+1d"));
        Assert.True(running.Commands.SetCustomSpeed.CanExecute("500"));
    }

    [Fact]
    public void Jump_to_is_enabled_only_while_running_and_only_with_a_valid_moment_in_its_field()
    {
        var idle = new SessionViewModel();
        var running = SessionStates.Running();

        // Idle: never, whatever the field holds - the running clock is what a jump moves.
        idle.JumpMoment.DateText = "2038-01-19";
        Assert.False(idle.Commands.JumpToEntered.CanExecute(null));

        // Running but the field is empty - it starts empty, unlike the pre-filled start moment, so the
        // button waits for a target rather than offering to jump to a default nobody typed.
        Assert.False(running.Commands.JumpToEntered.CanExecute(null));

        // Running with a valid moment typed into the jump field: enabled.
        running.JumpMoment.DateText = "2038-01-19";
        Assert.True(running.Commands.JumpToEntered.CanExecute(null));

        // A malformed moment disables it again - a jump to a date that does not exist is refused at the
        // button, not sent and rejected downstream.
        running.JumpMoment.DateText = "2038-13-45";
        Assert.False(running.Commands.JumpToEntered.CanExecute(null));
    }

    [Fact]
    public void Jump_to_refreshes_its_gate_when_the_jump_field_is_typed_into()
    {
        // JumpMoment raises its OWN PropertyChanged, which does not travel through the view model's, so the
        // command hooks the field directly. Reversal probe: drop that hook in SessionCommands and this stays
        // false, so the button would not light up as a moment is typed.
        var vm = SessionStates.Running();
        var fired = false;
        vm.Commands.JumpToEntered.CanExecuteChanged += (_, _) => fired = true;

        vm.JumpMoment.DateText = "2038-01-19";

        Assert.True(fired);
    }

    [Fact]
    public void Typing_a_jump_target_never_touches_the_start_moment()
    {
        // The separate field is the whole design: a jump target written into the start Moment would become
        // the next session's Starts-at, because New session keeps the form filled (ResetSession leaves Moment
        // alone). Typing a target here leaves the start moment exactly as it was.
        var vm = SessionStates.Running();
        var startBefore = vm.Moment.Canonical;

        vm.JumpMoment.DateText = "2050-06-15";
        vm.JumpMoment.TimeText = "12:00:00";

        Assert.Equal(startBefore, vm.Moment.Canonical);
    }

    [Fact]
    public void New_session_is_enabled_only_after_the_session_has_ended()
    {
        Assert.False(new SessionViewModel().Commands.NewSession.CanExecute(null));
        Assert.True(SessionStates.Ended().Commands.NewSession.CanExecute(null));
    }

    [Fact]
    public void New_session_returns_a_finished_session_to_idle()
    {
        // A finished session already has IsIdle true (the form unlocks so a moment can be fixed), so what
        // tells "result" from "fresh setup" is the terminal status, which is exactly what New session clears.
        var vm = SessionStates.Ended();
        Assert.True(vm.VerdictKnown);
        Assert.True(vm.Commands.NewSession.CanExecute(null)); // terminal

        vm.Commands.NewSession.Execute(null);

        Assert.False(vm.VerdictKnown); // the result is cleared for the fresh form
        Assert.False(vm.Commands.NewSession.CanExecute(null)); // no longer terminal - back to setup
    }

    [Fact]
    public void Repeat_and_forget_need_a_chosen_record()
    {
        var vm = new SessionViewModel();
        Assert.False(vm.Commands.Repeat.CanExecute(null));
        Assert.False(vm.Commands.Forget.CanExecute(null));

        vm.SelectedRecord = Record();

        Assert.True(vm.Commands.Repeat.CanExecute(null));
        Assert.True(vm.Commands.Forget.CanExecute(null));
    }

    [Fact]
    public void Today_fills_the_moment_off_the_shipped_default()
    {
        var vm = new SessionViewModel();
        Assert.True(vm.MomentIsDefault);

        vm.Commands.Today.Execute(null);

        Assert.False(vm.MomentIsDefault); // the field is no longer the shipped date
    }

    [Fact]
    public void A_command_refreshes_its_gate_when_the_session_changes()
    {
        // The subscription is what keeps a button's enabled state in step with the view model. Reversal
        // probe: drop `session.PropertyChanged += OnSessionChanged` in SessionCommands and this stays false.
        var vm = new SessionViewModel();
        var fired = false;
        vm.Commands.Start.CanExecuteChanged += (_, _) => fired = true;

        vm.SetTarget("app.exe"); // raises PropertyChanged, which must re-query the command gates

        Assert.True(fired);
    }

    // --- window-dependent commands, through a fake shell (no window, no clipboard, no dialog) ---

    private sealed class FakeShell : IShellInteraction
    {
        public string? Executable;
        public string? Folder;
        public bool ClipboardOk = true;
        public bool ConfirmResult;
        public string? Copied;
        public bool WasAsked;

        public string? PickExecutable() => Executable;
        public string? PickFolder(string? initialDirectory) => Folder;
        public bool CopyToClipboard(string text) { Copied = text; return ClipboardOk; }
        public bool Confirm(string headingKey, string messageKey, string affirmativeKey)
        {
            WasAsked = true;
            return ConfirmResult;
        }
        public string Text(string key) => key;
    }

    private static SessionViewModel WithShell(IShellInteraction shell, InMemorySessionHistoryStore? store = null)
    {
        var vm = new SessionViewModel(store ?? new InMemorySessionHistoryStore());
        vm.Commands.AttachShell(shell);
        return vm;
    }

    [Fact]
    public void Choose_target_fills_the_target_from_the_picker()
    {
        var shell = new FakeShell { Executable = "picked.exe" };
        var vm = WithShell(shell);

        vm.Commands.ChooseTarget.Execute(null);

        Assert.Equal("picked.exe", vm.TargetName);
    }

    [Fact]
    public void Browse_folder_fills_the_working_folder_from_the_picker()
    {
        var shell = new FakeShell { Folder = "picked-folder" };
        var vm = WithShell(shell);

        vm.Commands.BrowseFolder.Execute(null);

        Assert.Equal("picked-folder", vm.WorkingFolder);
    }

    [Fact]
    public void A_window_command_without_a_shell_is_a_quiet_no_op()
    {
        // The state sheet and a bare unit test have no shell. The command must not throw, and must change
        // nothing - here the target set by the dev default stays whatever it was.
        var vm = new SessionViewModel();
        var before = vm.TargetName;

        vm.Commands.ChooseTarget.Execute(null);

        Assert.Equal(before, vm.TargetName);
    }

    [Fact]
    public void Copy_summary_puts_the_summary_on_the_clipboard_and_notes_it()
    {
        var shell = new FakeShell { ClipboardOk = true };
        var vm = WithShell(shell);

        vm.Commands.CopySummary.Execute(null);

        Assert.False(string.IsNullOrEmpty(shell.Copied));
        Assert.Equal("copy.done", vm.CopyFeedbackKey);
    }

    [Fact]
    public void Clear_history_confirms_before_clearing()
    {
        var store = new InMemorySessionHistoryStore();
        store.Append(Record());

        var refusing = new FakeShell { ConfirmResult = false };
        var refused = WithShell(refusing, store);
        refused.Commands.ClearHistory.Execute(null);
        Assert.True(refusing.WasAsked); // it asked before keeping - not a silent no-op
        Assert.True(refused.HasHistory); // asked, said no, kept

        var confirming = new FakeShell { ConfirmResult = true };
        var confirmed = WithShell(confirming, store);
        confirmed.Commands.ClearHistory.Execute(null);
        Assert.True(confirming.WasAsked); // it asked before clearing
        Assert.False(confirmed.HasHistory); // asked, said yes, cleared
    }

    [Fact]
    public void The_window_commands_follow_their_data_gate()
    {
        // The gate does NOT depend on the shell, so a shell-less render draws them exactly as the shipped
        // panel does. Choose is idle-only, Copy summary follows CanCopySummary, Clear history needs history.
        var idle = new SessionViewModel();
        Assert.True(idle.Commands.ChooseTarget.CanExecute(null)); // idle - a target may be chosen
        Assert.False(idle.Commands.CopySummary.CanExecute(null)); // idle has nothing to summarise
        Assert.False(idle.Commands.ClearHistory.CanExecute(null)); // no history yet

        // CopySummary follows the status, which the state factories set through Apply. (The IsIdle-gated
        // commands cannot be exercised false here: the factories reach "running" via Apply, which never
        // clears _idle - only a real StartAsync does, and that spawns a core.)
        Assert.True(SessionStates.Ended().Commands.CopySummary.CanExecute(null));
    }
}
