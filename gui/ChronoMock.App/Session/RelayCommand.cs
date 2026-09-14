using System.Windows.Input;

namespace ChronoMock.App;

// Hand-rolled ICommand family, for the same reason ObservableObject is hand-rolled: no MVVM package is
// pulled in for one screen (rule 8 licence sieve, gui-and-cli-constraints). CanExecute is re-queried only
// when RaiseCanExecuteChanged is called - deliberately NOT off CommandManager.RequerySuggested, which polls
// on every UI event and would make a test's CanExecute depend on WPF's global requery rather than on the
// state the view model actually set. The commands subscribe to the view model's PropertyChanged and raise
// their own change instead (SessionCommands), so the same wiring works with or without a window.

/// <summary>A parameterless <see cref="ICommand"/> whose <see cref="CanExecute"/> is refreshed on demand.</summary>
public sealed class RelayCommand : ICommand
{
    private readonly Action _execute;
    private readonly Func<bool>? _canExecute;

    public RelayCommand(Action execute, Func<bool>? canExecute = null)
    {
        _execute = execute;
        _canExecute = canExecute;
    }

    public event EventHandler? CanExecuteChanged;

    public bool CanExecute(object? parameter) => _canExecute is null || _canExecute();

    // Explicit interface implementation on purpose: Execute is the ICommand contract that the WPF binding
    // invokes, never a method this class is called by name - so it is not part of RelayCommand's own public
    // surface, and the dead-code guard (which reads names) correctly does not see a public "Execute".
    void ICommand.Execute(object? parameter) => _execute();

    public void RaiseCanExecuteChanged() => CanExecuteChanged?.Invoke(this, EventArgs.Empty);
}

/// <summary>
/// A typed <see cref="ICommand"/>. A parameter of the wrong type (or null for a value type) reads as "cannot
/// execute" rather than throwing, so a mis-bound CommandParameter fails quietly to disabled instead of
/// tearing down the app - the view is where the type is guaranteed, and a test can still pass the real type.
/// </summary>
public sealed class RelayCommand<T> : ICommand
{
    private readonly Action<T> _execute;
    private readonly Func<T, bool>? _canExecute;

    public RelayCommand(Action<T> execute, Func<T, bool>? canExecute = null)
    {
        _execute = execute;
        _canExecute = canExecute;
    }

    public event EventHandler? CanExecuteChanged;

    // A parameter of the wrong type (or null for a value type) cannot execute, whether or not a gate is set:
    // Execute is a no-op for it, so reporting it as executable would light a button that does nothing.
    public bool CanExecute(object? parameter)
        => parameter is T value && (_canExecute is null || _canExecute(value));

    void ICommand.Execute(object? parameter)
    {
        if (parameter is T value)
        {
            _execute(value);
        }
    }

    public void RaiseCanExecuteChanged() => CanExecuteChanged?.Invoke(this, EventArgs.Empty);
}

/// <summary>
/// An async <see cref="ICommand"/> that will not run twice at once. The second click while the first call is
/// still in flight is a no-op, and CanExecute reports false for the whole run, so a Start that spawns a core
/// out of process cannot be launched twice by a fast double click (the released panel guarded this only by
/// CanStart flipping, which has a window between the click and the state change).
/// </summary>
public sealed class AsyncRelayCommand : ICommand
{
    private readonly Func<Task> _execute;
    private readonly Func<bool>? _canExecute;
    private bool _running;

    public AsyncRelayCommand(Func<Task> execute, Func<bool>? canExecute = null)
    {
        _execute = execute;
        _canExecute = canExecute;
    }

    public event EventHandler? CanExecuteChanged;

    public bool CanExecute(object? parameter) => !_running && (_canExecute is null || _canExecute());

    async void ICommand.Execute(object? parameter)
    {
        if (!CanExecute(parameter))
        {
            return;
        }

        _running = true;
        RaiseCanExecuteChanged();
        try
        {
            await _execute();
        }
        finally
        {
            _running = false;
            RaiseCanExecuteChanged();
        }
    }

    public void RaiseCanExecuteChanged() => CanExecuteChanged?.Invoke(this, EventArgs.Empty);
}
