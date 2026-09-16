using System.Windows.Input;
using ChronoMock.App;

namespace ChronoMock.App.Tests;

/// <summary>
/// The hand-rolled command family. Execute is an explicit ICommand member, so the tests reach it through the
/// interface exactly as the WPF binding does - which is also why the dead-code guard does not see it as a
/// public method.
/// </summary>
public class RelayCommandTests
{
    [Fact]
    public void A_relay_command_runs_its_action_and_reports_its_gate()
    {
        var ran = 0;
        var allowed = false;
        ICommand command = new RelayCommand(() => ran++, () => allowed);

        Assert.False(command.CanExecute(null));
        allowed = true;
        Assert.True(command.CanExecute(null));

        command.Execute(null);
        Assert.Equal(1, ran);
    }

    [Fact]
    public void A_relay_command_with_no_gate_can_always_execute()
    {
        ICommand command = new RelayCommand(() => { });
        Assert.True(command.CanExecute(null));
    }

    [Fact]
    public void Raising_the_change_notifies_listeners()
    {
        var command = new RelayCommand(() => { });
        var fired = 0;
        command.CanExecuteChanged += (_, _) => fired++;

        command.RaiseCanExecuteChanged();

        Assert.Equal(1, fired);
    }

    [Fact]
    public void A_typed_command_passes_its_parameter_through()
    {
        long seen = 0;
        ICommand command = new RelayCommand<long>(value => seen = value);

        command.Execute(60L);

        Assert.Equal(60L, seen);
    }

    [Fact]
    public void A_typed_command_refuses_a_parameter_of_the_wrong_type()
    {
        var ran = 0;
        ICommand command = new RelayCommand<long>(_ => ran++);

        // A string where a long is expected, and null for the value type: both read as "cannot execute"
        // and Execute is a no-op rather than a throw, so a mis-bound parameter fails quietly to disabled.
        Assert.False(command.CanExecute("not a long"));
        Assert.False(command.CanExecute(null));
        command.Execute("not a long");
        command.Execute(null);

        Assert.Equal(0, ran);
    }

    [Fact]
    public void An_async_command_will_not_run_twice_at_once()
    {
        var gate = new TaskCompletionSource();
        var runs = 0;
        var command = new AsyncRelayCommand(async () =>
        {
            runs++;
            await gate.Task;
        });
        ICommand asCommand = command;

        asCommand.Execute(null); // enters the action, then awaits the gate
        Assert.Equal(1, runs);
        Assert.False(asCommand.CanExecute(null)); // gated for the whole run

        asCommand.Execute(null); // a second press while the first is in flight is a no-op
        Assert.Equal(1, runs);

        gate.SetResult();
    }

    [Fact]
    public void An_async_command_respects_its_own_gate()
    {
        var allowed = false;
        var command = new AsyncRelayCommand(() => Task.CompletedTask, () => allowed);
        ICommand asCommand = command;

        Assert.False(asCommand.CanExecute(null));
        allowed = true;
        Assert.True(asCommand.CanExecute(null));
    }
}
