using System.Runtime.CompilerServices;

// The diagnostics buffer's trimming rule is worth a unit test and cannot have one from outside: the
// buffer is private to a client that only exists around a running core process. Opening the assembly
// to its own test project is the same seam ChronoMock.App already uses, and it keeps the alternative
// off the table - widening a method to public purely so a test can reach it, which puts a shape in
// the API that nothing outside the test wants.
[assembly: InternalsVisibleTo("ChronoMock.Protocol.Tests")]
