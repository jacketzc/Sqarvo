using System.Diagnostics;
using Sqarvo.Core.Errors;
using Sqarvo.Core.Ipc;
using Sqarvo.Core.Process;
using Sqarvo.Core.Tests.Support;

namespace Sqarvo.Core.Tests;

public sealed class BackendSupervisorTests
{
    private static BackendSupervisorOptions Options(
        TestPathLayout layout,
        FakeProcessLauncher launcher,
        FakePidLiveness? liveness = null,
        TimeSpan? endpointTimeout = null,
        TimeSpan? shutdownGrace = null,
        TimeSpan? forceKillTimeout = null,
        IReadOnlyList<string>? additionalArguments = null,
        IReadOnlyDictionary<string, string?>? childEnvironment = null) =>
        new()
        {
            PathLayout = layout,
            ProcessLauncher = launcher,
            PidLiveness = liveness ?? new FakePidLiveness(),
            ConnectionFactory = _ => throw new InvalidOperationException("单元测试不建立通道"),
            EndpointTimeout = endpointTimeout ?? TimeSpan.FromSeconds(2),
            EndpointPollInterval = TimeSpan.FromMilliseconds(20),
            ShutdownGrace = shutdownGrace ?? TimeSpan.FromMilliseconds(200),
            ForceKillTimeout = forceKillTimeout ?? TimeSpan.FromMilliseconds(100),
            AdditionalArguments = additionalArguments ?? Array.Empty<string>(),
            ChildEnvironment = childEnvironment ?? new Dictionary<string, string?>(),
        };

    [Fact]
    public void DefaultTimingsMatchTheAgreedHandshake()
    {
        var options = new BackendSupervisorOptions { PathLayout = new TestPathLayout("/tmp/unused.json") };

        Assert.Equal(TimeSpan.FromSeconds(10), options.EndpointTimeout);
        Assert.Equal(TimeSpan.FromMilliseconds(100), options.EndpointPollInterval);
        Assert.Equal(TimeSpan.FromSeconds(5), options.ShutdownGrace);
        Assert.Equal(TimeSpan.FromSeconds(2), options.ForceKillTimeout);
        Assert.Equal(TimeSpan.FromSeconds(5), BackendSupervisor.HealthPollInterval);
    }

    [Fact]
    public async Task ReusesLiveBackendAndLeavesItAlone()
    {
        using var directory = new TempDirectory();
        var endpointPath = directory.WriteFile("backend-endpoint.json", EndpointJson.Tcp(pid: 4242));
        var layout = new TestPathLayout(endpointPath);
        var launcher = new FakeProcessLauncher(new FakeProcessHandle(1));
        var progress = new RecordingProgress<BackendStartStage>();
        var supervisor = new BackendSupervisor(
            Options(layout, launcher, new FakePidLiveness().Alive(4242)));

        var endpoint = await supervisor.AcquireAsync(progress);

        Assert.Equal(4242, endpoint.Pid);
        Assert.False(supervisor.OwnsBackend);
        Assert.Empty(launcher.Launches);
        Assert.Equal(
            new[] { BackendStartStage.ReadingEndpoint, BackendStartStage.CheckingEndpoint },
            progress.Values);

        var shutdown = await supervisor.ShutdownAsync();

        Assert.False(shutdown.OwnedBackend);
        Assert.False(shutdown.Forced);
        Assert.False(shutdown.EndpointFileRemoved);
        Assert.True(File.Exists(endpointPath), "别人启动的后端端点文件不能被本实例删除");
    }

    [Fact]
    public async Task IncompatibleProtocolOnExistingEndpointIsFatalBeforeSpawning()
    {
        using var directory = new TempDirectory();
        var endpointPath = directory.WriteFile(
            "backend-endpoint.json",
            EndpointJson.Tcp(pid: 4242, protocolVersion: "v2"));
        var launcher = new FakeProcessLauncher(new FakeProcessHandle(1));
        var supervisor = new BackendSupervisor(
            Options(new TestPathLayout(endpointPath), launcher, new FakePidLiveness().Alive(4242)));

        var exception = await Assert.ThrowsAsync<SqarvoProtocolException>(() => supervisor.AcquireAsync());

        Assert.Equal(SqarvoErrorKind.ProtocolIncompatible, exception.Kind);
        Assert.Empty(launcher.Launches);
        Assert.True(File.Exists(endpointPath));
    }

    [Fact]
    public async Task SpawnsBackendAndWaitsForMatchingPid()
    {
        using var directory = new TempDirectory();
        var endpointPath = Path.Combine(directory.Path, "backend-endpoint.json");
        var handle = new FakeProcessHandle(1234);
        var layout = new TestPathLayout(endpointPath, "/fake/sqarvo-backend");
        layout.ChildEnvironment["XDG_RUNTIME_DIR"] = "/tmp/run-root";
        var launcher = new FakeProcessLauncher(_ =>
        {
            File.WriteAllText(endpointPath, EndpointJson.Tcp(pid: 1234, port: 60001));
            return handle;
        });
        var progress = new RecordingProgress<BackendStartStage>();
        var supervisor = new BackendSupervisor(Options(
            layout,
            launcher,
            new FakePidLiveness(),
            additionalArguments: new[] { "--listen", "127.0.0.1:0" },
            childEnvironment: new Dictionary<string, string?> { ["SQARVO_LOG"] = "warn" }));

        var endpoint = await supervisor.AcquireAsync(progress);

        Assert.Equal(1234, endpoint.Pid);
        Assert.True(supervisor.OwnsBackend);
        Assert.Contains(BackendStartStage.StartingBackend, progress.Values);
        Assert.Contains(BackendStartStage.WaitingEndpoint, progress.Values);

        var launch = Assert.Single(launcher.Launches);
        Assert.Equal("/fake/sqarvo-backend", launch.ExecutablePath);
        Assert.Equal("--parent-pid", launch.Arguments[0]);
        Assert.Equal(Environment.ProcessId.ToString(), launch.Arguments[1]);
        Assert.Equal(new[] { "--listen", "127.0.0.1:0" }, launch.Arguments.Skip(2));
        Assert.Equal("/tmp/run-root", launch.Environment["XDG_RUNTIME_DIR"]);
        Assert.Equal("warn", launch.Environment["SQARVO_LOG"]);
    }

    [Fact]
    public async Task IgnoresEndpointFileWrittenByAnotherProcessAndTimesOut()
    {
        using var directory = new TempDirectory();
        var endpointPath = Path.Combine(directory.Path, "backend-endpoint.json");
        var handle = new FakeProcessHandle(1234);
        var launcher = new FakeProcessLauncher(_ =>
        {
            // A leftover file from a previous run: pid must not match, so it is not "ready".
            File.WriteAllText(endpointPath, EndpointJson.Tcp(pid: 999));
            return handle;
        });
        var supervisor = new BackendSupervisor(Options(
            new TestPathLayout(endpointPath),
            launcher,
            endpointTimeout: TimeSpan.FromMilliseconds(150)));

        var exception = await Assert.ThrowsAsync<SqarvoProcessException>(() => supervisor.AcquireAsync());

        Assert.Equal(SqarvoErrorKind.BackendStartTimeout, exception.Kind);
        Assert.Contains("pid=999", exception.Message, StringComparison.Ordinal);
        Assert.Equal(1, handle.KillRequests);
        Assert.True(handle.Disposed);
        Assert.False(supervisor.OwnsBackend);
    }

    [Fact]
    public async Task ReportsEarlyExitWithCapturedDiagnostics()
    {
        using var directory = new TempDirectory();
        var endpointPath = Path.Combine(directory.Path, "backend-endpoint.json");
        var handle = new FakeProcessHandle(4321, diagnostics: "[stderr] boom: 无法绑定端口");
        handle.MarkExited(3);
        var launcher = new FakeProcessLauncher(handle);
        var supervisor = new BackendSupervisor(Options(new TestPathLayout(endpointPath), launcher));

        var exception = await Assert.ThrowsAsync<SqarvoProcessException>(() => supervisor.AcquireAsync());

        Assert.Equal(SqarvoErrorKind.BackendExitedEarly, exception.Kind);
        Assert.Equal(4321, exception.ProcessId);
        Assert.Contains("退出码 3", exception.Message, StringComparison.Ordinal);
        Assert.Contains("boom", exception.CapturedError!, StringComparison.Ordinal);
    }

    [Fact]
    public async Task IncompatibleProtocolFromSpawnedBackendStopsTheProcess()
    {
        using var directory = new TempDirectory();
        var endpointPath = Path.Combine(directory.Path, "backend-endpoint.json");
        var handle = new FakeProcessHandle(1234);
        var launcher = new FakeProcessLauncher(_ =>
        {
            File.WriteAllText(endpointPath, EndpointJson.Tcp(pid: 1234, protocolVersion: "v9"));
            return handle;
        });
        var supervisor = new BackendSupervisor(Options(new TestPathLayout(endpointPath), launcher));

        var exception = await Assert.ThrowsAsync<SqarvoProtocolException>(() => supervisor.AcquireAsync());

        Assert.Equal(SqarvoErrorKind.ProtocolIncompatible, exception.Kind);
        Assert.Equal(1, handle.KillRequests);
        Assert.True(handle.Disposed);
    }

    [Fact]
    public async Task StaleEndpointFromDeadPidStartsANewBackend()
    {
        using var directory = new TempDirectory();
        var endpointPath = directory.WriteFile("backend-endpoint.json", EndpointJson.Tcp(pid: 4242));
        var handle = new FakeProcessHandle(5555);
        var launcher = new FakeProcessLauncher(_ =>
        {
            File.WriteAllText(endpointPath, EndpointJson.Tcp(pid: 5555, port: 60002));
            return handle;
        });
        var supervisor = new BackendSupervisor(
            Options(new TestPathLayout(endpointPath), launcher, new FakePidLiveness().Dead(4242)));

        var endpoint = await supervisor.AcquireAsync();

        Assert.Equal(5555, endpoint.Pid);
        Assert.True(supervisor.OwnsBackend);
        Assert.Single(launcher.Launches);
    }

    [Fact]
    public async Task CancellationAbandonsTheHandshakeWithoutSpawning()
    {
        using var directory = new TempDirectory();
        var launcher = new FakeProcessLauncher(new FakeProcessHandle(1));
        var supervisor = new BackendSupervisor(
            Options(new TestPathLayout(Path.Combine(directory.Path, "absent.json")), launcher));
        using var cancellation = new CancellationTokenSource();
        cancellation.Cancel();

        await Assert.ThrowsAnyAsync<OperationCanceledException>(
            () => supervisor.AcquireAsync(cancellation.Token));

        Assert.Empty(launcher.Launches);
        Assert.False(supervisor.OwnsBackend);
    }

    [Fact]
    public async Task ShutdownForcesTerminationWhenNoGracefulSignalIsAvailable()
    {
        using var directory = new TempDirectory();
        var endpointPath = Path.Combine(directory.Path, "backend-endpoint.json");
        var handle = new FakeProcessHandle(777) { AcceptsGracefulSignal = false, ExitsOnKill = true };
        var launcher = new FakeProcessLauncher(_ =>
        {
            File.WriteAllText(endpointPath, EndpointJson.Tcp(pid: 777, port: 60003));
            return handle;
        });
        var supervisor = new BackendSupervisor(Options(new TestPathLayout(endpointPath), launcher));
        await supervisor.AcquireAsync();

        var shutdown = await supervisor.ShutdownAsync();

        Assert.False(shutdown.Graceful);
        Assert.True(shutdown.Forced);
        Assert.True(shutdown.OwnedBackend);
        Assert.True(shutdown.EndpointFileRemoved);
        Assert.Equal(1, handle.KillRequests);
        Assert.True(handle.Disposed);
        Assert.False(File.Exists(endpointPath));
    }

    [Fact]
    public async Task ShutdownReportsFailureWhenTheProcessSurvives()
    {
        using var directory = new TempDirectory();
        var endpointPath = Path.Combine(directory.Path, "backend-endpoint.json");
        var handle = new FakeProcessHandle(888) { ExitsOnGraceful = false, ExitsOnKill = false };
        var launcher = new FakeProcessLauncher(_ =>
        {
            File.WriteAllText(endpointPath, EndpointJson.Tcp(pid: 888, port: 60004));
            return handle;
        });
        var supervisor = new BackendSupervisor(Options(
            new TestPathLayout(endpointPath),
            launcher,
            shutdownGrace: TimeSpan.FromMilliseconds(50),
            forceKillTimeout: TimeSpan.FromMilliseconds(50)));
        await supervisor.AcquireAsync();

        var exception = await Assert.ThrowsAsync<SqarvoProcessException>(() => supervisor.ShutdownAsync());

        Assert.Equal(SqarvoErrorKind.ShutdownFailed, exception.Kind);
        Assert.Equal(888, exception.ProcessId);
        Assert.Contains("仍未退出", exception.Message, StringComparison.Ordinal);
        Assert.Equal(1, handle.KillRequests);
        Assert.True(handle.Disposed);
    }

    [Fact]
    public async Task GracefulShutdownSucceedsOnFirstSignal()
    {
        using var directory = new TempDirectory();
        var endpointPath = Path.Combine(directory.Path, "backend-endpoint.json");
        var handle = new FakeProcessHandle(999) { ExitsOnGraceful = true };
        var launcher = new FakeProcessLauncher(_ =>
        {
            File.WriteAllText(endpointPath, EndpointJson.Tcp(pid: 999, port: 60005));
            return handle;
        });
        var supervisor = new BackendSupervisor(Options(new TestPathLayout(endpointPath), launcher));
        await supervisor.AcquireAsync();

        var shutdown = await supervisor.ShutdownAsync();

        Assert.True(shutdown.Graceful);
        Assert.False(shutdown.Forced);
        Assert.True(shutdown.EndpointFileRemoved);
        Assert.Equal(0, handle.KillRequests);
        Assert.Equal(1, handle.GracefulRequests);
    }

    [Fact]
    public async Task ConnectionRequiresAnAcquiredBackend()
    {
        using var directory = new TempDirectory();
        var supervisor = new BackendSupervisor(Options(
            new TestPathLayout(Path.Combine(directory.Path, "absent.json")),
            new FakeProcessLauncher(new FakeProcessHandle(1))));

        var exception = Assert.Throws<SqarvoProcessException>(() => supervisor.GetConnection());

        Assert.Equal(SqarvoErrorKind.BackendNotAcquired, exception.Kind);
        await supervisor.ShutdownAsync();
    }

    [Fact]
    public async Task DisposeWithoutAcquireIsHarmless()
    {
        using var directory = new TempDirectory();
        var supervisor = new BackendSupervisor(Options(
            new TestPathLayout(Path.Combine(directory.Path, "absent.json")),
            new FakeProcessLauncher(new FakeProcessHandle(1))));

        await supervisor.DisposeAsync();

        Assert.Null(supervisor.Endpoint);
        Assert.False(supervisor.OwnsBackend);
    }
}