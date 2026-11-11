using Sqarvo.Core.Clients;
using Sqarvo.Core.Errors;
using Sqarvo.Core.Model;
using Sqarvo.Core.Process;
using Sqarvo.Windows.Connections;

namespace Sqarvo.Windows.Workspace;

/// <summary>
/// 应用级后端会话：<b>唯一</b>持有 <see cref="BackendSupervisor"/> 与 <see cref="BackendConnection"/>
/// 的地方，也是 <see cref="BackendReadiness"/> 的唯一写入者。
/// </summary>
/// <remarks>
/// <para>
/// 职责（AGENTS.md §4.1「前端负责调用 gRPC 并处理后端生命周期」）：
/// 启动握手、5 秒健康探测（由界面的 <c>DispatcherTimer</c> 驱动，这里不自己起定时器）、
/// 凭据写入、测试连接、建立/关闭会话、退出时停机。界面只读
/// <see cref="State"/>，不各自维护「是否已连接」的布尔量。
/// </para>
/// <para>
/// 线程约定：本类的方法都在任意线程上安全调用，事件 <see cref="StateChanged"/> 可能**不在**
/// UI 线程上触发（RPC 续体跑在线程池），订阅方必须自己切回 UI 线程。
/// </para>
/// <para>
/// 取消：每个 RPC 都接受 <see cref="CancellationToken"/>（对话框关闭即取消）。取消是
/// <see cref="OperationCanceledException"/>，不改变后端状态——握手期间的取消才会落到
/// <see cref="BackendReadiness.Failed"/>，因为那时本来就没有可用的后端。
/// </para>
/// </remarks>
public sealed class BackendSession
{
    private readonly BackendSupervisor _supervisor;
    private readonly object _gate = new();
    private readonly Dictionary<string, string> _open = new(StringComparer.Ordinal);
    private readonly Dictionary<string, ConnectionOpenResult> _opened = new(StringComparer.Ordinal);

    private BackendState _state = BackendState.Initial;
    private bool _started;
    private bool _shutdownRequested;

    /// <summary>用一个 supervisor 构造；supervisor 的路径与超时由调用方（App）决定。</summary>
    /// <param name="supervisor">后端进程与通道的所有者。</param>
    public BackendSession(BackendSupervisor supervisor)
    {
        _supervisor = supervisor ?? throw new ArgumentNullException(nameof(supervisor));
    }

    /// <summary>当前状态快照（不可变，可跨线程读）。</summary>
    public BackendState State => Volatile.Read(ref _state);

    /// <summary>状态变化；可能在非 UI 线程触发。</summary>
    public event EventHandler<BackendState>? StateChanged;

    /// <summary>本会话已建立的数据库连接：连接身份键 → <c>connection_id</c>。</summary>
    /// <returns>只读副本，用于重建界面时恢复「已连接」徽标。</returns>
    public IReadOnlyDictionary<string, string> OpenConnections
    {
        get
        {
            lock (_gate)
            {
                return new Dictionary<string, string>(_open, StringComparer.Ordinal);
            }
        }
    }

    /// <summary>某条已保存连接是否已经建立了数据库会话。</summary>
    /// <param name="identity">连接身份键（<see cref="ConnectionProfile.Key"/>）。</param>
    /// <returns>是否已连接。</returns>
    public bool IsConnected(string identity)
    {
        lock (_gate)
        {
            return _open.ContainsKey(identity);
        }
    }

    /// <summary>
    /// 元数据客户端（对象树 / 列 / DDL 的唯一入口）；后端未就绪时返回 <see langword="null"/>。
    /// </summary>
    /// <remarks>不抛异常：调用方据此显示「后端尚未就绪」，而不是把启动期的异常带到 UI 线程。</remarks>
    public MetadataClient? Metadata
    {
        get
        {
            if (!State.IsReady)
            {
                return null;
            }

            try
            {
                return _supervisor.GetConnection().Metadata;
            }
            catch (SqarvoException)
            {
                return null;
            }
        }
    }

    /// <summary>某条已保存连接的会话身份（`session_id` / 服务端版本 / 驱动）；未连上时为 <see langword="null"/>。</summary>
    /// <param name="identity">连接身份键（<see cref="ConnectionProfile.Key"/>）。</param>
    /// <returns>这次连接的结果，或 <see langword="null"/>。</returns>
    /// <remarks>
    /// 信息面板显示「服务端版本」时读这里，而不是在树、页面、面板各存一份连接结果。
    /// </remarks>
    public ConnectionOpenResult? OpenResult(string identity)
    {
        lock (_gate)
        {
            return _opened.TryGetValue(identity, out var result) ? result : null;
        }
    }

    /// <summary>
    /// 启动握手：<c>AcquireAsync</c> → <c>GetVersion</c> → <c>ListDrivers</c> → <c>GetHealth</c>。
    /// </summary>
    /// <param name="cancellationToken">放弃握手（自己启动的后端会被停掉）。</param>
    /// <remarks>本方法不抛异常：失败一律落到 <see cref="BackendReadiness.Failed"/> 并带上原因。</remarks>
    public async Task StartAsync(CancellationToken cancellationToken = default)
    {
        lock (_gate)
        {
            if (_started)
            {
                return;
            }

            _started = true;
        }

        Publish(BackendState.Initial);

        try
        {
            // 进度只在「仍在启动」时写回：握手完成后迟到的进度回调不能覆盖 Ready 快照。
            var progress = new Progress<BackendStartStage>(stage =>
            {
                var current = State;
                if (current.Readiness == BackendReadiness.Starting)
                {
                    Publish(current with { Message = StageMessage(stage) });
                }
            });

            var endpoint = await _supervisor.AcquireAsync(progress, cancellationToken).ConfigureAwait(false);
            var connection = _supervisor.GetConnection();

            var version = await connection.System.GetVersionAsync(cancellationToken).ConfigureAwait(false);
            var drivers = await connection.System.ListDriversAsync(cancellationToken).ConfigureAwait(false);
            var health = await _supervisor.InvokeHealthAsync(cancellationToken).ConfigureAwait(false);

            Publish(new BackendState
            {
                Readiness = BackendReadiness.Ready,
                Message =
                    $"后端 {version.ServiceVersion}（协议 {version.ProtocolVersion}，{version.BuildProfile}）"
                    + $"· 端点 {endpoint.TransportDescription} pid {endpoint.Pid}",
                Version = version,
                Drivers = drivers,
                RegisteredDrivers = health.RegisteredDrivers,
                ActiveSessions = health.ActiveSessions,
            });
        }
        catch (OperationCanceledException)
        {
            Publish(new BackendState
            {
                Readiness = BackendReadiness.Failed,
                Message = "启动本地后端已取消 [cancelled]",
                Code = "cancelled",
            });
        }
        catch (Exception error)
        {
            var failure = SqarvoFailure.FromException(error);
            Publish(new BackendState
            {
                Readiness = BackendReadiness.Failed,
                Message = failure.ToString(),
                Code = failure.Code,
            });
        }
    }

    /// <summary>
    /// 健康探测（界面每 5 秒调一次，间隔取 <see cref="BackendSupervisor.HealthPollInterval"/>）。
    /// </summary>
    /// <param name="cancellationToken">取消这次探测（退出、页面重建）。</param>
    /// <returns>是否仍然就绪。</returns>
    /// <remarks>
    /// 探测失败即视为断连：状态转 <see cref="BackendReadiness.Disconnected"/> 并保留原因。
    /// 不在这里自动重启后端——静默重启会把「为什么断了」藏起来。
    /// </remarks>
    public async Task<bool> RefreshHealthAsync(CancellationToken cancellationToken = default)
    {
        var current = State;
        if (current.Readiness != BackendReadiness.Ready)
        {
            return false;
        }

        try
        {
            var health = await _supervisor.InvokeHealthAsync(cancellationToken).ConfigureAwait(false);
            var message = health.State == HealthState.Degraded && health.Detail.Length > 0
                ? $"后端处于降级状态：{health.Detail}"
                : current.Message;

            Publish(current with
            {
                Message = message,
                RegisteredDrivers = health.RegisteredDrivers,
                ActiveSessions = health.ActiveSessions,
            });
            return true;
        }
        catch (OperationCanceledException)
        {
            return false;
        }
        catch (Exception error)
        {
            var failure = SqarvoFailure.FromException(error);
            Publish(current with
            {
                Readiness = BackendReadiness.Disconnected,
                Message = failure.ToString(),
                Code = failure.Code,
                ActiveSessions = 0,
            });
            ReleaseConnections();
            return false;
        }
    }

    /// <summary>把密码写进系统凭据库（Windows Credential Manager），返回可落盘的引用。</summary>
    /// <param name="credentialRef">已有引用（覆盖同一个密钥）或 <see langword="null"/>（新建）。</param>
    /// <param name="secret">密码明文；本实例不保留它。</param>
    /// <param name="cancellationToken">取消这次调用。</param>
    /// <returns>凭据引用。</returns>
    public async Task<string> StoreCredentialAsync(
        string? credentialRef,
        string secret,
        CancellationToken cancellationToken = default)
    {
        var connection = RequireConnection();
        return await connection.Credentials
            .StoreCredentialAsync(
                string.IsNullOrEmpty(credentialRef) ? null : credentialRef,
                secret,
                cancellationToken)
            .ConfigureAwait(false);
    }

    /// <summary>删除一条凭据（幂等）。用于「写入密码但没能用上」时的尽力清理。</summary>
    /// <param name="credentialRef">凭据引用。</param>
    /// <param name="cancellationToken">取消这次调用。</param>
    /// <returns>完成的任务；失败时抛出 <see cref="SqarvoException"/>。</returns>
    public async Task DeleteCredentialAsync(string credentialRef, CancellationToken cancellationToken = default)
    {
        if (string.IsNullOrEmpty(credentialRef))
        {
            return;
        }

        var connection = RequireConnection();
        await connection.Credentials.DeleteCredentialAsync(credentialRef, cancellationToken).ConfigureAwait(false);
    }

    /// <summary>测试连接（不建立长期会话）。</summary>
    /// <param name="profile">已带凭据引用的条目（密码不在其中）。</param>
    /// <param name="cancellationToken">取消这次调用。</param>
    /// <returns>可达性、延迟与服务端版本。</returns>
    public async Task<ConnectionTestResult> TestConnectionAsync(
        ConnectionProfile profile,
        CancellationToken cancellationToken = default)
    {
        ArgumentNullException.ThrowIfNull(profile);

        var connection = RequireConnection();
        var response = await connection.Connections
            .TestConnectionAsync(ConnectionForm.ToParams(profile), cancellationToken)
            .ConfigureAwait(false);

        return new ConnectionTestResult(
            response.Reachable,
            response.LatencyMillis,
            response.ServerVersion,
            response.Warnings.Select(warning => $"{warning.Code}：{warning.Message}").ToArray());
    }

    /// <summary>建立数据库会话，并记住「这条连接已经连着」。</summary>
    /// <param name="profile">已带凭据引用的条目。</param>
    /// <param name="cancellationToken">取消这次调用。</param>
    /// <returns>会话身份与驱动能力。</returns>
    public async Task<ConnectionOpenResult> ConnectAsync(
        ConnectionProfile profile,
        CancellationToken cancellationToken = default)
    {
        ArgumentNullException.ThrowIfNull(profile);

        var connection = RequireConnection();
        var response = await connection.Connections
            .ConnectAsync(ConnectionForm.ToParams(profile), profile.Name, cancellationToken)
            .ConfigureAwait(false);

        var result = new ConnectionOpenResult(
            response.ConnectionId,
            response.SessionId,
            response.DriverId,
            response.ServerVersion,
            response.Capabilities.ToArray());

        lock (_gate)
        {
            _open[profile.Key] = result.ConnectionId;
            _opened[profile.Key] = result;
        }

        return result;
    }

    /// <summary>关闭某条已保存连接的数据库会话。</summary>
    /// <param name="identity">连接身份键（<see cref="ConnectionProfile.Key"/>）。</param>
    /// <param name="cancellationToken">取消这次调用。</param>
    /// <returns>是否真的关掉了一个会话。</returns>
    public async Task<bool> DisconnectAsync(string identity, CancellationToken cancellationToken = default)
    {
        string? connectionId;
        lock (_gate)
        {
            if (!_open.TryGetValue(identity, out connectionId))
            {
                return false;
            }
        }

        var connection = RequireConnection();
        await connection.Connections.DisconnectAsync(connectionId, cancellationToken).ConfigureAwait(false);

        lock (_gate)
        {
            _open.Remove(identity);
            _opened.Remove(identity);
        }

        return true;
    }

    /// <summary>
    /// 停止后端（仅当本进程启动过它），并释放通道。
    /// </summary>
    /// <param name="cancellationToken">放弃优雅等待；进程仍会被强制终止。</param>
    /// <remarks>重复调用是安全的；异常（后端杀不掉）会转成状态说明，不抛给退出路径。</remarks>
    public async Task ShutdownAsync(CancellationToken cancellationToken = default)
    {
        lock (_gate)
        {
            if (_shutdownRequested)
            {
                return;
            }

            _shutdownRequested = true;
        }

        try
        {
            await _supervisor.ShutdownAsync(cancellationToken).ConfigureAwait(false);
        }
        catch (SqarvoException error)
        {
            var failure = SqarvoFailure.FromException(error);
            Publish(new BackendState
            {
                Readiness = BackendReadiness.Disconnected,
                Message = $"退出时未能停止本地后端：{failure}",
                Code = failure.Code,
            });
            return;
        }
        finally
        {
            ReleaseConnections();
        }

        Publish(new BackendState
        {
            Readiness = BackendReadiness.Disconnected,
            Message = "已退出：本地后端已停止",
            Code = "shutdown",
        });
    }

    private BackendConnection RequireConnection()
    {
        if (!State.IsReady)
        {
            throw new SqarvoProcessException(
                SqarvoErrorKind.BackendNotAcquired,
                "本地后端尚未就绪：请等状态栏显示「已连接」后重试");
        }

        return _supervisor.GetConnection();
    }

    private void ReleaseConnections()
    {
        lock (_gate)
        {
            _open.Clear();
            _opened.Clear();
        }
    }

    private void Publish(BackendState state)
    {
        // 事件在写入之后触发：订阅方读 State 看到的永远不会比事件旧。
        Volatile.Write(ref _state, state);
        StateChanged?.Invoke(this, state);
    }

    private static string StageMessage(BackendStartStage stage) => stage switch
    {
        BackendStartStage.ReadingEndpoint => "正在读取上次运行留下的端点文件…",
        BackendStartStage.CheckingEndpoint => "正在检查已有后端是否还活着…",
        BackendStartStage.StartingBackend => "正在启动后端进程…",
        _ => "正在等待后端发布端点…",
    };
}