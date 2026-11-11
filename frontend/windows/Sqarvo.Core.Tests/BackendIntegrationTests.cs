using System.Diagnostics;
using Sqarvo.Core.Clients;
using Sqarvo.Core.Errors;
using Sqarvo.Core.Ipc;
using Sqarvo.Core.Model;
using Sqarvo.Core.Platform;
using Sqarvo.Core.Process;
using Sqarvo.Core.Tests.Support;
using Xunit.Abstractions;

namespace Sqarvo.Core.Tests;

/// <summary>
/// Integration tests against the real Rust backend process, on this machine.
/// </summary>
/// <remarks>
/// <para>
/// The backend is started with <c>--listen 127.0.0.1:0</c>, i.e. the real Windows transport (loopback TCP
/// plus the per-process token), not the Unix socket used on Linux by default. The endpoint file location
/// is pinned through <see cref="DefaultPathLayout.ForEndpointFile"/> and handed to the child as
/// <c>XDG_RUNTIME_DIR</c>, which is exactly how the Windows build will locate it after the
/// <c>--endpoint-file</c> flag lands (plan WP1).
/// </para>
/// <para>
/// These tests require a Linux/macOS host with the backend built
/// (<c>cd backend &amp;&amp; cargo build -p sqarvo-backend-service</c>); they are skipped on Windows because
/// the local binary is a Unix executable.
/// </para>
/// </remarks>
public sealed class BackendIntegrationTests
{
    private readonly ITestOutputHelper _output;

    public BackendIntegrationTests(ITestOutputHelper output) => _output = output;

    [Fact]
    public async Task SupervisorStartsLoopbackTcpBackendServesRpcsAndStopsIt()
    {
        if (OperatingSystem.IsWindows())
        {
            return;
        }

        using var directory = new TempDirectory("sqarvo-core-it-");
        var binary = BackendBinary.Locate(_output);
        var supervisor = CreateSupervisor(directory, binary);
        var progress = new RecordingProgress<BackendStartStage>();

        try
        {
            var endpoint = await supervisor.AcquireAsync(progress);

            foreach (var stage in progress.Values)
            {
                _output.WriteLine($"stage: {stage}");
            }

            _output.WriteLine(
                $"endpoint: {endpoint.TransportDescription} pid={endpoint.Pid} "
                + $"protocol={endpoint.ProtocolVersion} service={endpoint.ServiceVersion} "
                + $"tokenLength={endpoint.TokenLength}");

            Assert.Equal(TransportKind.Tcp, endpoint.Transport);
            Assert.Equal("127.0.0.1", endpoint.Host);
            Assert.InRange(endpoint.Port!.Value, 1, 65535);
            Assert.Equal("v1", endpoint.ProtocolVersion);
            Assert.Equal(64, endpoint.TokenLength);
            Assert.Equal(endpoint.Pid, supervisor.Endpoint!.Pid);
            Assert.True(supervisor.OwnsBackend, "本测试启动的后端应归本 supervisor 所有");
            Assert.True(File.Exists(supervisor.EndpointFilePath), "后端应写出端点文件");
            Assert.Contains(BackendStartStage.WaitingEndpoint, progress.Values);

            using var connection = supervisor.GetConnection();

            var version = await connection.System.GetVersionAsync();
            _output.WriteLine(
                $"SystemService.GetVersion -> service_version={version.ServiceVersion} "
                + $"protocol_version={version.ProtocolVersion} build_profile={version.BuildProfile}");
            Assert.False(string.IsNullOrWhiteSpace(version.ServiceVersion));
            Assert.Equal(endpoint.ProtocolVersion, version.ProtocolVersion);

            var drivers = await connection.System.ListDriversAsync();
            _output.WriteLine($"SystemService.ListDrivers -> {drivers.Count} 个驱动");
            foreach (var driver in drivers)
            {
                _output.WriteLine($"  {driver.Label} {driver.Version} [{string.Join(", ", driver.Capabilities)}]");
            }

            var health = await supervisor.InvokeHealthAsync();
            _output.WriteLine(
                $"SystemService.GetHealth -> state={health.State} uptime={health.Uptime} "
                + $"sessions={health.ActiveSessions} drivers={health.RegisteredDrivers}");
            Assert.Equal(HealthState.Serving, health.State);
            Assert.Equal((uint)drivers.Count, health.RegisteredDrivers);

            var backendPid = endpoint.Pid;
            var shutdown = await supervisor.ShutdownAsync();
            _output.WriteLine(
                $"ShutdownAsync -> graceful={shutdown.Graceful} forced={shutdown.Forced} "
                + $"endpointFileRemoved={shutdown.EndpointFileRemoved} "
                + $"elapsed={shutdown.Elapsed.TotalMilliseconds:0} ms");

            Assert.True(shutdown.OwnedBackend);
            Assert.True(shutdown.Graceful, "Linux 上后端应响应 SIGTERM 并自行清理");
            Assert.False(shutdown.Forced);
            Assert.True(shutdown.EndpointFileRemoved);
            Assert.False(File.Exists(supervisor.EndpointFilePath));

            await WaitUntilAsync(
                () => !new DefaultPidLiveness().IsAlive(backendPid),
                TimeSpan.FromSeconds(10));
            _output.WriteLine($"后端进程 {backendPid} 已确认退出");
        }
        finally
        {
            if (supervisor.OwnsBackend)
            {
                await supervisor.ShutdownAsync();
            }
        }
    }

    [Fact]
    public async Task SecondSupervisorReusesTheRunningBackendAndDoesNotKillIt()
    {
        if (OperatingSystem.IsWindows())
        {
            return;
        }

        using var directory = new TempDirectory("sqarvo-core-reuse-");
        var binary = BackendBinary.Locate(_output);
        var first = CreateSupervisor(directory, binary);
        var second = CreateSupervisor(directory, binary);

        try
        {
            var endpoint = await first.AcquireAsync();
            _output.WriteLine($"第一个 supervisor 启动后端 pid={endpoint.Pid}");

            var reused = await second.AcquireAsync();
            _output.WriteLine($"第二个 supervisor 读到端点 pid={reused.Pid}，ownsBackend={second.OwnsBackend}");

            Assert.Equal(endpoint.Pid, reused.Pid);
            Assert.False(second.OwnsBackend, "第二个 supervisor 没有启动后端，就不该拥有它");

            var secondShutdown = await second.ShutdownAsync();
            Assert.False(secondShutdown.OwnedBackend);
            Assert.False(secondShutdown.Forced);
            Assert.True(File.Exists(first.EndpointFilePath), "复用方退出时不能删除别人的端点文件");
            Assert.True(new DefaultPidLiveness().IsAlive(endpoint.Pid), "复用方退出时不能杀掉别人的后端");

            var shutdown = await first.ShutdownAsync();
            _output.WriteLine($"拥有者关闭后端：graceful={shutdown.Graceful} forced={shutdown.Forced}");
            Assert.True(shutdown.Graceful);
            await WaitUntilAsync(() => !new DefaultPidLiveness().IsAlive(endpoint.Pid), TimeSpan.FromSeconds(10));
        }
        finally
        {
            if (first.OwnsBackend)
            {
                await first.ShutdownAsync();
            }

            if (second.OwnsBackend)
            {
                await second.ShutdownAsync();
            }
        }
    }

    [Fact]
    public async Task WrongTokenIsReportedAsAuthenticationError()
    {
        if (OperatingSystem.IsWindows())
        {
            return;
        }

        using var directory = new TempDirectory("sqarvo-core-auth-");
        var binary = BackendBinary.Locate(_output);
        var supervisor = CreateSupervisor(directory, binary);

        try
        {
            var endpoint = await supervisor.AcquireAsync();
            var wrongToken = new string('0', EndpointInfo.TokenHexLength);
            using var connection = BackendConnection.Create(endpoint.WithToken(wrongToken));

            var exception = await Assert.ThrowsAsync<SqarvoAuthenticationException>(
                () => connection.System.GetVersionAsync());

            _output.WriteLine($"错误 token -> {exception.GetType().Name}: {exception.Message}");
            Assert.Equal(SqarvoErrorKind.Unauthenticated, exception.Kind);
            Assert.Equal(Grpc.Core.StatusCode.Unauthenticated, exception.GrpcStatus);
            Assert.DoesNotContain(endpoint.Token, exception.Message, StringComparison.Ordinal);
            Assert.DoesNotContain(wrongToken, exception.Message, StringComparison.Ordinal);

            // The correctly-tokened channel still works, so the failure really was the token.
            var version = await supervisor.GetConnection().System.GetVersionAsync();
            Assert.False(string.IsNullOrWhiteSpace(version.ServiceVersion));

            await supervisor.ShutdownAsync();
        }
        finally
        {
            if (supervisor.OwnsBackend)
            {
                await supervisor.ShutdownAsync();
            }
        }
    }

    private BackendSupervisor CreateSupervisor(TempDirectory directory, string binary)
    {
        var endpointPath = Path.Combine(directory.Path, "sqarvo", "backend-endpoint.json");
        var layout = DefaultPathLayout.ForEndpointFile(endpointPath, binary);

        return new BackendSupervisor(new BackendSupervisorOptions
        {
            PathLayout = layout,
            AdditionalArguments = new[] { "--listen", "127.0.0.1:0" },
            ChildEnvironment = new Dictionary<string, string?>(StringComparer.Ordinal)
            {
                // Keep config/data inside the test directory instead of the developer's home.
                ["XDG_CONFIG_HOME"] = Path.Combine(directory.Path, "config"),
                ["XDG_DATA_HOME"] = Path.Combine(directory.Path, "data"),
                ["SQARVO_LOG"] = "warn",
            },
            EndpointTimeout = TimeSpan.FromSeconds(20),
        });
    }

    private static async Task WaitUntilAsync(Func<bool> condition, TimeSpan timeout)
    {
        var stopwatch = Stopwatch.StartNew();
        while (stopwatch.Elapsed < timeout)
        {
            if (condition())
            {
                return;
            }

            await Task.Delay(50);
        }

        Assert.Fail($"等待条件成立超时（{timeout.TotalSeconds:0.#} 秒）");
    }
}