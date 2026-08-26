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

    private void OnRemoveStepClick(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { Tag: StepViewModel step })
        {
            ViewModel?.RemoveStep(step);
        }
    }

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
