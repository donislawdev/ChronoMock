using Xunit;

// The core enforces a single active session per machine (named control block), and the conformance
// tests perform real injection into a shared machine. Serialize the whole assembly so two sessions
// never overlap - a source of flakiness, not a real failure.
[assembly: CollectionBehavior(DisableTestParallelization = true)]
