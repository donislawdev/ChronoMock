using Xunit.Sdk;
using Xunit.v3;

// WPF has one Application per process and thread affinity (a single STA UI thread). Serialize the whole
// assembly so the window-build host is shared, never raced.
// xunit v3 spelling: CollectionBehavior.DisableTestParallelization is obsolete here.
[assembly: Parallelization(Mode = ParallelMode.None)]
