using Xunit;

// WPF has one Application per process and thread affinity (a single STA UI thread). Serialize the whole
// assembly so the window-build host is shared, never raced.
[assembly: CollectionBehavior(DisableTestParallelization = true)]
