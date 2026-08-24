using System.ComponentModel;
using System.Runtime.CompilerServices;

namespace ChronoMock.App;

/// <summary>
/// Minimal hand-rolled <see cref="INotifyPropertyChanged"/>. No MVVM package is pulled in for one screen -
/// a controls/MVVM dependency would be a rule-8 licence-sieve event, not worth it here (gui-and-cli-constraints).
/// </summary>
public abstract class ObservableObject : INotifyPropertyChanged
{
    public event PropertyChangedEventHandler? PropertyChanged;

    /// <summary>Set a backing field and raise <see cref="PropertyChanged"/> only when the value actually changes.</summary>
    protected bool Set<T>(ref T field, T value, [CallerMemberName] string? name = null)
    {
        if (EqualityComparer<T>.Default.Equals(field, value))
        {
            return false;
        }

        field = value;
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
        return true;
    }
}
