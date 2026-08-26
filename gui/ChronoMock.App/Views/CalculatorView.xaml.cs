using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Controls;
using ChronoMock.App.Calc;

namespace ChronoMock.App.Views;

/// <summary>
/// Date-calculator screen (chrono-mock 7.3). Slice G3b: the builder and result bind to a
/// <see cref="CalculatorViewModel"/> (set by the host window). These handlers cover the actions that
/// are not plain bindings - adding and removing a step, and copying one output format.
/// </summary>
public partial class CalculatorView : UserControl
{
    public CalculatorView()
    {
        InitializeComponent();
    }

    private CalculatorViewModel? ViewModel => DataContext as CalculatorViewModel;

    private void OnAddStepClick(object sender, RoutedEventArgs e) => ViewModel?.AddStep();

    // Apply a preset: fill the builder from its moment (7.3). A parametric preset shows an honest note
    // instead (the view model does not fill a wrong moment).
    private void OnPresetClick(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { Tag: PresetItemViewModel item })
        {
            ViewModel?.ApplyPreset(item.Info);
        }
    }

    private void OnRemoveStepClick(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { Tag: StepViewModel step })
        {
            ViewModel?.RemoveStep(step);
        }
    }

    // Send the result to the substitution panel (6.3): the view model raises an event the host window
    // handles (it knows both modules), so the moment travels with its zone (rule 2).
    private void OnUseInSubstitutionClick(object sender, RoutedEventArgs e) => ViewModel?.RequestUseInSubstitution();

    // Copy one output format to the clipboard. A clipboard held by another process is ignored for a
    // convenience copy (the value stays on screen to copy by hand).
    private void OnCopyFormatClick(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { Tag: string value } && value.Length > 0)
        {
            try
            {
                Clipboard.SetText(value);
            }
            catch (ExternalException)
            {
                // Clipboard busy - nothing to do for a copy affordance.
            }
        }
    }
}
