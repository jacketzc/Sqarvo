using System.Diagnostics;

using DiagnosticsProcess = System.Diagnostics.Process;
using Sqarvo.Core.Platform;
using Sqarvo.Core.Tests.Support;

namespace Sqarvo.Core.Tests;

public sealed class PidLivenessTests
{
    private readonly DefaultPidLiveness _liveness = new();

    [Fact]
    public void CurrentProcessIsAlive()
    {
        Assert.True(_liveness.IsAlive(Environment.ProcessId));
    }

    [Theory]
    [InlineData(0)]
    [InlineData(-1)]
    [InlineData(int.MaxValue)]
    public void NonsensePidsAreDead(int pid)
    {
        Assert.False(_liveness.IsAlive(pid));
    }

    [Fact]
    public void ExitedProcessIsReportedAsDead()
    {
        if (OperatingSystem.IsWindows())
        {
            // /bin/true does not exist there; the Windows branch is covered by the compile probe.
            return;
        }

        var process = DiagnosticsProcess.Start(new ProcessStartInfo("/bin/true")
        {
            UseShellExecute = false,
        });

        Assert.NotNull(process);
        var pid = process.Id;
        process.WaitForExit(5000);
        process.Dispose();

        Assert.False(_liveness.IsAlive(pid));
    }

    [Fact]
    public void DefaultPathLayoutDerivesChildRuntimeRootFromStandardLayout()
    {
        // 期望值用 Path.Combine 拼：Windows 的分隔符是反斜杠，硬编码 "/tmp/..." 只在 Linux 成立
        // （CI 第一次跑 windows-latest 就红在这个断言上）。
        var root = Path.Combine(Path.GetTempPath(), "run-root");
        var projectDirectory = Path.Combine(root, "sqarvo");
        var endpoint = Path.Combine(projectDirectory, Sqarvo.Core.Ipc.EndpointFile.FileName);

        Assert.Equal(root, DefaultPathLayout.ChildRuntimeRoot(endpoint));
        Assert.Null(DefaultPathLayout.ChildRuntimeRoot(
            Path.Combine(Path.GetTempPath(), "other", Sqarvo.Core.Ipc.EndpointFile.FileName)));
        Assert.Null(DefaultPathLayout.ChildRuntimeRoot(Path.Combine(projectDirectory, "not-the-name.json")));
    }

    [Fact]
    public void DefaultPathLayoutGivesTheChildTheMatchingRuntimeDirectory()
    {
        if (OperatingSystem.IsWindows())
        {
            // On Windows the backend derives %APPDATA% itself; no child override is needed.
            return;
        }

        using var directory = new TempDirectory();
        var endpointPath = Path.Combine(directory.Path, "sqarvo", "backend-endpoint.json");
        var binaryPath = directory.WriteFile("bin/sqarvo-backend", string.Empty);
        var layout = DefaultPathLayout.ForEndpointFile(endpointPath, binaryPath);

        var environment = layout.CreateChildEnvironment();

        Assert.Equal(directory.Path, environment["XDG_RUNTIME_DIR"]);
        Assert.Equal(Path.GetFullPath(binaryPath), layout.ResolveBackendBinary());
    }

    [Fact]
    public void DefaultPathLayoutReportsEverySearchedBinaryPath()
    {
        using var directory = new TempDirectory();
        var layout = DefaultPathLayout.ForEndpointFile(
            Path.Combine(directory.Path, "sqarvo", "backend-endpoint.json"),
            Path.Combine(directory.Path, "missing-binary"));

        var exception = Assert.Throws<Errors.SqarvoProcessException>(() => layout.ResolveBackendBinary());

        Assert.Equal(Errors.SqarvoErrorKind.BackendBinaryNotFound, exception.Kind);
        Assert.Contains("missing-binary", exception.Message, StringComparison.Ordinal);
    }
}