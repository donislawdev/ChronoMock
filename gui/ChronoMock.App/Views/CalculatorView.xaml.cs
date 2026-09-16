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

    // Send the result to the substitution panel (6.3): the view model raises an event the host window
    // handles (it knows both modules), so the moment travels with its zone (rule 2).
    private void OnUseInSubstitutionClick(object sender, RoutedEventArgs e) => ViewModel?.RequestUseInSubstitution();

    // Copy one output format to the clipboard. A clipboard held by another process is ignored for a
    // convenience copy (the value stays on screen to copy by hand).
    private void OnCopyFormatClick(object sender, RoutedEventArgs e)
    {
        if (sender is Button button && button.Tag is string value && value.Length > 0)
        {
            try
            {
                Clipboard.SetText(value);
            }
            catch (ExternalException)
            {
                // Clipboard busy - say nothing rather than claim a copy that did not happen.
                return;
            }

            FlashCopied(button);
        }
    }

    // The copy affordance has to answer, or a reader cannot tell a click that worked from one that did
    // nothing. The clicked button reads "Copied" for a moment and then returns to "Copy". Only one button
    // shows it at a time - a second copy reverts the first at once, so the screen never carries two claims
    // of a value the clipboard can only hold one of.
    private Button? _copiedButton;
    private System.Windows.Threading.DispatcherTimer? _copiedTimer;

    private void FlashCopied(Button button)
    {
        RevertCopied();

        _copiedButton = button;
        button.Content = Application.Current?.TryFindResource("calc.copied") as string ?? "Copied";

        _copiedTimer = new System.Windows.Threading.DispatcherTimer { Interval = TimeSpan.FromSeconds(1.5) };
        _copiedTimer.Tick += (_, _) => RevertCopied();
        _copiedTimer.Start();
    }

    private void RevertCopied()
    {
        _copiedTimer?.Stop();
        _copiedTimer = null;

        if (_copiedButton is not null)
        {
            // Back to the DynamicResource, so a later language switch still reaches the label.
            _copiedButton.SetResourceReference(ContentControl.ContentProperty, "calc.copy");
            _copiedButton = null;
        }
    }
}
