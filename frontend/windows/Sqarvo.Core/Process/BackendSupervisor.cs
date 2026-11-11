using System.Diagnostics;
using System.Globalization;
using Sqarvo.Core.Clients;
using Sqarvo.Core.Errors;
using Sqarvo.Core.Ipc;
using Sqarvo.Core.Model;
using Sqarvo.Core.Platform;
using Sqarvo.Core.Protocol;

namespace Sqarvo.Core.Process;

/// <summary>
/// Owns the lifecycle of the local backend process: reuse a running one, otherwise start it, wait until
/// it is reachable, and stop it again when the client exits.
/// </summary>
/// <remarks>
/// <para>Rules that this class implements, and why:</para>
/// <list type="bullet">
/// <item><description>
/// <b>Only ever manage a backend this instance started.</b> If the endpoint file belongs to a process
/// somebody else started (another client, a developer running the backend by hand), the endpoint is
/// reused and shutdown does nothing: no signal, no file deletion.
/// </description></item>
/// <item><description>
/// <b>Protocol mismatch is fatal and never starts a second backend.</b> Starting another process cannot
/// fix a version mismatch and would compete for the same endpoint file.
/// </description></item>
/// <item><description>
/// <b>A dead pid means a stale endpoint.</b> The file is left behind by a crashed backend; a new backend
/// overwrites it.
/// </description></item>
/// <item><description>
/// <b>The endpoint is only trusted when its pid is our child.</b> While waiting, a file written by an
/// older run must not be mistaken for the new backend being ready.
/// </description></item>
/// <item><description>
/// <b>Shutdown never lies.</b> Graceful request, grace period, forced termination, then an error if the
/// process still runs.
/// </description></item>
/// </list>
/// <para>
/// The supervisor is not thread safe for concurrent <see cref="AcquireAsync(System.Threading.CancellationToken)"/> calls; create one per
/// client instance and call <see cref="AcquireAsync(System.Threading.CancellationToken)"/> once.
/// </para>
/// </remarks>
public sealed class BackendSupervisor : IAsyncDisposable
{
    /// <summary>Recommended interval for the optional health poll driven by the caller.</summary>
    public static TimeSpan HealthPollInterval => TimeSpan.FromSeconds(5);

    private readonly BackendSupervisorOptions _options;
    private readonly IProcessLauncher _launcher;
    private readonly IPidLiveness _liveness;
    private readonly Func<EndpointInfo, BackendConnection> _connectionFactory;
    private readonly object _gate = new();

    private IProcessHandle? _process;
    private EndpointInfo? _endpoint;
    private BackendConnection? _connection;

    /// <summary>Creates a supervisor. Nothing is started until <see cref="AcquireAsync(System.Threading.CancellationToken)"/> is called.</summary>
    /// <param name="options">Locations, timeouts and injectable collaborators.</param>
    public BackendSupervisor(BackendSupervisorOptions options)
    {
        ArgumentNullException.ThrowIfNull(options);

        _options = options;
        _launcher = options.ProcessLauncher ?? new DefaultProcessLauncher();
        _liveness = options.PidLiveness ?? new DefaultPidLiveness();
        _connectionFactory = options.ConnectionFactory ?? (endpoint => BackendConnection.Create(endpoint));
    }

    /// <summary>Endpoint file this supervisor reads and (for its own backend) deletes on shutdown.</summary>
    public string EndpointFilePath => _options.PathLayout.EndpointFilePath;

    /// <summary>Endpoint in use, or <see langword="null"/> before a successful <see cref="AcquireAsync(System.Threading.CancellationToken)"/>.</summary>
    public EndpointInfo? Endpoint => _endpoint;

    /// <summary>Whether the backend in use was started by this supervisor.</summary>
    public bool OwnsBackend => _process is not null;

    /// <summary>
    /// Makes a backend available: reuses a live one, otherwise starts one and waits for its endpoint.
    /// </summary>
    /// <param name="cancellationToken">Abandons the handshake (a backend started here is then stopped).</param>
    /// <returns>The endpoint to connect to.</returns>
    public Task<EndpointInfo> AcquireAsync(CancellationToken cancellationToken = default) =>
        AcquireAsync(null, cancellationToken);

    /// <summary>
    /// Makes a backend available and reports progress stages.
    /// </summary>
    /// <param name="progress">Optional progress sink, reported on the calling thread.</param>
    /// <param name="cancellationToken">Abandons the handshake (a backend started here is then stopped).</param>
    /// <returns>The endpoint to connect to.</returns>
    /// <exception cref="SqarvoEndpointException">The existing endpoint file is invalid or unreadable.</exception>
    /// <exception cref="SqarvoProtocolException">The backend speaks an incompatible protocol.</exception>
    /// <exception cref="SqarvoProcessException">
    /// The binary is missing, the spawn failed, the child exited early, or the endpoint did not appear in
    /// time. In every failure case a backend started here is stopped before the exception escapes.
    /// </exception>
    public async Task<EndpointInfo> AcquireAsync(
        IProgress<BackendStartStage>? progress,
        CancellationToken cancellationToken = default)
    {
        if (_endpoint is not null)
        {
            return _endpoint;
        }

        progress?.Report(BackendStartStage.ReadingEndpoint);
        cancellationToken.ThrowIfCancellationRequested();

        var existing = EndpointFile.Read(EndpointFilePath);

        if (existing is not null)
        {
            progress?.Report(BackendStartStage.CheckingEndpoint);

            // Fatal on purpose: a second backend cannot fix a protocol mismatch.
            ProtocolVersion.EnsureCompatible(existing.ProtocolVersion);

            if (_liveness.IsAlive(existing.Pid))
            {
                // Somebody else's backend (or a previous run of this client): reuse it, own nothing.
                _endpoint = existing;
                return existing;
            }
        }

        cancellationToken.ThrowIfCancellationRequested();
        progress?.Report(BackendStartStage.StartingBackend);

        var binary = _options.PathLayout.ResolveBackendBinary();
        var handle = _launcher.Launch(new BackendLaunchOptions
        {
            ExecutablePath = binary,
            Arguments = BuildArguments(),
            Environment = BuildChildEnvironment(),
        });

        _process = handle;

        try
        {
            progress?.Report(BackendStartStage.WaitingEndpoint);
            var endpoint = await WaitForEndpointAsync(handle, cancellationToken).ConfigureAwait(false);
            _endpoint = endpoint;
            return endpoint;
        }
        catch
        {
            // Never leave an unattended backend behind when the handshake fails.
            await StopProcessAsync().ConfigureAwait(false);
            throw;
        }
    }

    /// <summary>
    /// Calls <c>SystemService.GetHealth</c> on the acquired backend.
    /// </summary>
    /// <param name="cancellationToken">Cancels the call.</param>
    /// <returns>The health snapshot.</returns>
    /// <remarks>
    /// Polling is caller driven: call this every <see cref="HealthPollInterval"/> (5 s) from a timer, and
    /// treat repeated failures as a lost backend. It is deliberately not a background loop here, because
    /// the cadence and the reaction to failure belong to the UI (status bar, reconnect prompt).
    /// </remarks>
    /// <exception cref="SqarvoProcessException">No backend has been acquired yet.</exception>
    public Task<HealthSnapshot> InvokeHealthAsync(CancellationToken cancellationToken = default) =>
        GetConnection().System.GetHealthAsync(cancellationToken);

    /// <summary>Returns the shared connection, creating it on first use.</summary>
    /// <returns>The connection for the acquired endpoint.</returns>
    /// <exception cref="SqarvoProcessException">No backend has been acquired yet.</exception>
    public BackendConnection GetConnection()
    {
        lock (_gate)
        {
            if (_connection is not null)
            {
                return _connection;
            }

            var endpoint = _endpoint
                ?? throw new SqarvoProcessException(
                    SqarvoErrorKind.BackendNotAcquired,
                    "尚未获取后端端点：先调用 AcquireAsync");

            _connection = _connectionFactory(endpoint);
            return _connection;
        }
    }

    /// <summary>
    /// Stops the backend if this supervisor started it.
    /// </summary>
    /// <param name="cancellationToken">
    /// Abandons the graceful wait; the process is still terminated afterwards. The channel is always
    /// disposed, and a process that will not die is always reported as a failure.
    /// </param>
    /// <returns>What happened.</returns>
    /// <remarks>
    /// Escalation: graceful request (Unix <c>SIGTERM</c>, so the backend deletes its socket and endpoint
    /// file) → wait <c>ShutdownGrace</c> → terminate → wait <c>ForceKillTimeout</c> → report
    /// <see cref="SqarvoErrorKind.ShutdownFailed"/> if it is still alive. Windows has no graceful signal,
    /// so it terminates immediately; the job object attached at launch remains the last-resort guarantee.
    /// </remarks>
    /// <exception cref="SqarvoProcessException">The process survived the forced termination.</exception>
    public async Task<ShutdownResult> ShutdownAsync(CancellationToken cancellationToken = default)
    {
        var stopwatch = Stopwatch.StartNew();
        DisposeConnection();

        IProcessHandle? handle;
        lock (_gate)
        {
            handle = _process;
            _process = null;
        }

        if (handle is null)
        {
            _endpoint = null;
            // Not our backend: do not signal it, do not delete its endpoint file.
            return new ShutdownResult(false, false, false, false, stopwatch.Elapsed);
        }

        var pid = handle.Id;
        try
        {
            var graceful = handle.RequestGracefulExit();
            if (graceful)
            {
                bool exited;
                try
                {
                    exited = await handle.WaitForExitAsync(_options.ShutdownGrace, cancellationToken)
                        .ConfigureAwait(false);
                }
                catch (OperationCanceledException)
                {
                    // Cancellation means "stop waiting", not "leave the process running".
                    exited = false;
                }

                if (exited)
                {
                    var removed = TryRemoveOwnEndpointFile(pid);
                    return new ShutdownResult(true, false, removed, true, stopwatch.Elapsed);
                }
            }

            handle.Kill();
            if (!await handle.WaitForExitAsync(_options.ForceKillTimeout, CancellationToken.None).ConfigureAwait(false))
            {
                throw new SqarvoProcessException(
                    SqarvoErrorKind.ShutdownFailed,
                    $"后端进程 {pid} 在强制终止后仍未退出；端口与端点文件可能残留",
                    pid,
                    handle.CapturedDiagnostics);
            }

            var cleaned = TryRemoveOwnEndpointFile(pid);
            return new ShutdownResult(graceful, true, cleaned, true, stopwatch.Elapsed);
        }
        finally
        {
            _endpoint = null;
            handle.Dispose();
        }
    }

    /// <summary>
    /// Stops the backend without throwing when it cannot be stopped. Intended for application exit, where
    /// there is nobody left to handle an exception; call <see cref="ShutdownAsync"/> when the outcome
    /// matters.
    /// </summary>
    public async ValueTask DisposeAsync()
    {
        try
        {
            await ShutdownAsync(CancellationToken.None).ConfigureAwait(false);
        }
        catch (SqarvoException)
        {
            // Documented: disposal swallows; ShutdownAsync is the reporting API.
        }
    }

    private async Task<EndpointInfo> WaitForEndpointAsync(
        IProcessHandle handle,
        CancellationToken cancellationToken)
    {
        var stopwatch = Stopwatch.StartNew();
        string? lastError = null;

        while (true)
        {
            cancellationToken.ThrowIfCancellationRequested();

            if (handle.HasExited)
            {
                throw new SqarvoProcessException(
                    SqarvoErrorKind.BackendExitedEarly,
                    $"后端进程 {handle.Id} 在写出端点文件前退出（退出码 {handle.ExitCode?.ToString(CultureInfo.InvariantCulture) ?? "未知"}）",
                    handle.Id,
                    handle.CapturedDiagnostics);
            }

            EndpointInfo? endpoint = null;
            try
            {
                endpoint = EndpointFile.Read(EndpointFilePath);
            }
            catch (SqarvoEndpointException exception)
            {
                // A partially visible or corrupt file is retried; the backend rewrites atomically so this
                // usually means an older run's leftovers.
                lastError = exception.Message;
            }

            if (endpoint is not null && endpoint.Pid == handle.Id)
            {
                // Same rule as the reuse path: incompatible protocol is fatal, never retried.
                ProtocolVersion.EnsureCompatible(endpoint.ProtocolVersion);
                return endpoint;
            }

            var elapsed = stopwatch.Elapsed;
            if (elapsed >= _options.EndpointTimeout)
            {
                var pidNote = endpoint is null
                    ? string.Empty
                    : $"；端点文件的 pid={endpoint.Pid} 与子进程 pid={handle.Id} 不一致";
                throw new SqarvoProcessException(
                    SqarvoErrorKind.BackendStartTimeout,
                    $"等待后端端点文件超时（{_options.EndpointTimeout.TotalSeconds:0.#} 秒）：{EndpointFilePath}{pidNote}"
                    + (lastError is null ? string.Empty : $"；最近一次读取错误：{lastError}"),
                    handle.Id,
                    handle.CapturedDiagnostics);
            }

            var remaining = _options.EndpointTimeout - elapsed;
            var delay = remaining < _options.EndpointPollInterval ? remaining : _options.EndpointPollInterval;
            await Task.Delay(delay, cancellationToken).ConfigureAwait(false);
        }
    }

    private List<string> BuildArguments()
    {
        // The backend installs PR_SET_PDEATHSIG / parent polling only when this flag is present, so a
        // hand-started backend (parent is a shell) is never killed when that shell exits.
        var arguments = new List<string>
        {
            "--parent-pid",
            Environment.ProcessId.ToString(CultureInfo.InvariantCulture),
        };

        arguments.AddRange(_options.AdditionalArguments);
        return arguments;
    }

    private Dictionary<string, string?> BuildChildEnvironment()
    {
        var environment = new Dictionary<string, string?>(StringComparer.Ordinal);

        foreach (var pair in _options.PathLayout.CreateChildEnvironment())
        {
            environment[pair.Key] = pair.Value;
        }

        foreach (var pair in _options.ChildEnvironment)
        {
            environment[pair.Key] = pair.Value;
        }

        return environment;
    }

    private async Task StopProcessAsync()
    {
        IProcessHandle? handle;
        lock (_gate)
        {
            handle = _process;
            _process = null;
        }

        if (handle is null)
        {
            return;
        }

        try
        {
            // Do not spend the full shutdown grace on a backend that never became ready; a short grace is
            // enough for it to clean up if it is still initialising.
            if (handle.RequestGracefulExit()
                && await handle.WaitForExitAsync(_options.ForceKillTimeout, CancellationToken.None).ConfigureAwait(false))
            {
                TryRemoveOwnEndpointFile(handle.Id);
                return;
            }

            handle.Kill();
            await handle.WaitForExitAsync(_options.ForceKillTimeout, CancellationToken.None).ConfigureAwait(false);
            TryRemoveOwnEndpointFile(handle.Id);
        }
        catch (OperationCanceledException)
        {
        }
        finally
        {
            handle.Dispose();
        }
    }

    /// <summary>
    /// Deletes the endpoint file only when it still belongs to <paramref name="pid"/>. A file written by
    /// another backend is never touched.
    /// </summary>
    private bool TryRemoveOwnEndpointFile(int pid)
    {
        try
        {
            if (!File.Exists(EndpointFilePath))
            {
                return true;
            }

            var current = EndpointFile.Read(EndpointFilePath);
            if (current is not null && current.Pid != pid)
            {
                return false;
            }

            File.Delete(EndpointFilePath);
            return true;
        }
        catch (Exception exception) when (exception is SqarvoException or IOException or UnauthorizedAccessException)
        {
            return false;
        }
    }

    private void DisposeConnection()
    {
        lock (_gate)
        {
            _connection?.Dispose();
            _connection = null;
        }
    }
}