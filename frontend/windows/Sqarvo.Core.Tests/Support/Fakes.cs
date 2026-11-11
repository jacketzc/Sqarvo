using Sqarvo.Core.Platform;

namespace Sqarvo.Core.Tests.Support;

/// <summary>Scriptable process handle: no real process is created.</summary>
internal sealed class FakeProcessHandle : IProcessHandle
{
    private readonly TaskCompletionSource<bool> _exited = new(TaskCreationOptions.RunContinuationsAsynchronously);

    internal FakeProcessHandle(int id, string diagnostics = "")
    {
        Id = id;
        CapturedDiagnostics = diagnostics;
    }

    public int Id { get; }

    public bool HasExited => _exited.Task.IsCompleted;

    public int? ExitCode { get; private set; }

    public string CapturedDiagnostics { get; set; }

    public bool Disposed { get; private set; }

    public int GracefulRequests { get; private set; }

    public int KillRequests { get; private set; }

    /// <summary>Whether <see cref="RequestGracefulExit"/> reports a delivered signal (false = Windows).</summary>
    internal bool AcceptsGracefulSignal { get; set; } = true;

    /// <summary>Whether a graceful request makes the process exit.</summary>
    internal bool ExitsOnGraceful { get; set; }

    /// <summary>Whether a forced kill makes the process exit.</summary>
    internal bool ExitsOnKill { get; set; } = true;

    internal void MarkExited(int exitCode = 0)
    {
        ExitCode = exitCode;
        _exited.TrySetResult(true);
    }

    public bool RequestGracefulExit()
    {
        GracefulRequests++;
        if (!AcceptsGracefulSignal)
        {
            return false;
        }

        if (ExitsOnGraceful)
        {
            MarkExited();
        }

        return true;
    }

    public void Kill()
    {
        KillRequests++;
        if (ExitsOnKill)
        {
            MarkExited(137);
        }
    }

    public async Task<bool> WaitForExitAsync(TimeSpan timeout, CancellationToken cancellationToken = default)
    {
        if (HasExited)
        {
            return true;
        }

        var delay = Task.Delay(timeout, cancellationToken);
        var completed = await Task.WhenAny(_exited.Task, delay).ConfigureAwait(false);
        return completed == _exited.Task && HasExited;
    }

    public void Dispose() => Disposed = true;
}

/// <summary>Launcher that records the request and hands out a scripted handle.</summary>
internal sealed class FakeProcessLauncher : IProcessLauncher
{
    private readonly Func<BackendLaunchOptions, FakeProcessHandle> _factory;

    internal FakeProcessLauncher(Func<BackendLaunchOptions, FakeProcessHandle> factory) => _factory = factory;

    internal FakeProcessLauncher(FakeProcessHandle handle)
        : this(_ => handle)
    {
    }

    internal List<BackendLaunchOptions> Launches { get; } = new();

    internal FakeProcessHandle? LastHandle { get; private set; }

    public IProcessHandle Launch(BackendLaunchOptions options)
    {
        Launches.Add(options);
        LastHandle = _factory(options);
        return LastHandle;
    }
}

/// <summary>Liveness probe with a scripted answer.</summary>
internal sealed class FakePidLiveness : IPidLiveness
{
    private readonly HashSet<int> _dead = new();
    private readonly HashSet<int> _alive = new();

    internal FakePidLiveness Alive(params int[] pids)
    {
        foreach (var pid in pids)
        {
            _alive.Add(pid);
            _dead.Remove(pid);
        }

        return this;
    }

    internal FakePidLiveness Dead(params int[] pids)
    {
        foreach (var pid in pids)
        {
            _dead.Add(pid);
            _alive.Remove(pid);
        }

        return this;
    }

    public bool IsAlive(int pid) => _alive.Contains(pid) && !_dead.Contains(pid);
}

/// <summary>Path layout pinned to a temp endpoint file and an explicit "binary".</summary>
internal sealed class TestPathLayout : IPathLayout
{
    internal TestPathLayout(string endpointFilePath, string backendBinary = "/fake/sqarvo-backend")
    {
        EndpointFilePath = endpointFilePath;
        BackendBinary = backendBinary;
    }

    public string EndpointFilePath { get; }

    internal string BackendBinary { get; }

    internal Dictionary<string, string?> ChildEnvironment { get; } = new(StringComparer.Ordinal);

    public string ResolveBackendBinary() => BackendBinary;

    public IReadOnlyDictionary<string, string?> CreateChildEnvironment() => ChildEnvironment;
}