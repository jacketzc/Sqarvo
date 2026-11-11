using Sqarvo.Core.Clients;
using Sqarvo.Core.Ipc;
using Sqarvo.Core.Platform;

namespace Sqarvo.Core.Process;

/// <summary>
/// Configuration for <see cref="BackendSupervisor"/>. Every collaborator is injectable so the startup and
/// shutdown logic can be unit tested without real processes.
/// </summary>
public sealed class BackendSupervisorOptions
{
    /// <summary>Endpoint/binary locations. Required.</summary>
    public required IPathLayout PathLayout { get; init; }

    /// <summary>Process launcher; defaults to <see cref="DefaultProcessLauncher"/>.</summary>
    public IProcessLauncher? ProcessLauncher { get; init; }

    /// <summary>Process liveness probe; defaults to <see cref="DefaultPidLiveness"/>.</summary>
    public IPidLiveness? PidLiveness { get; init; }

    /// <summary>
    /// Factory used to build the shared <see cref="BackendConnection"/>. Defaults to
    /// <see cref="BackendConnection.Create"/>; tests
    /// replace it to avoid opening sockets.
    /// </summary>
    public Func<EndpointInfo, BackendConnection>? ConnectionFactory { get; init; }

    /// <summary>How long to wait for the endpoint file after spawning. Default: 10 s.</summary>
    public TimeSpan EndpointTimeout { get; init; } = TimeSpan.FromSeconds(10);

    /// <summary>Endpoint file polling interval. Default: 100 ms.</summary>
    public TimeSpan EndpointPollInterval { get; init; } = TimeSpan.FromMilliseconds(100);

    /// <summary>How long the backend gets to exit after the graceful request. Default: 5 s.</summary>
    public TimeSpan ShutdownGrace { get; init; } = TimeSpan.FromSeconds(5);

    /// <summary>How long to wait after a forced kill before reporting failure. Default: 2 s.</summary>
    public TimeSpan ForceKillTimeout { get; init; } = TimeSpan.FromSeconds(2);

    /// <summary>
    /// Extra command line arguments appended after <c>--parent-pid</c>. Used by tests to force loopback
    /// TCP (<c>--listen 127.0.0.1:0</c>) and by packaging to pass a fixed endpoint location.
    /// </summary>
    public IReadOnlyList<string> AdditionalArguments { get; init; } = Array.Empty<string>();

    /// <summary>
    /// Environment variables applied on top of <see cref="IPathLayout.CreateChildEnvironment"/>; a
    /// <see langword="null"/> value removes the variable.
    /// </summary>
    public IReadOnlyDictionary<string, string?> ChildEnvironment { get; init; } =
        new Dictionary<string, string?>(StringComparer.Ordinal);
}