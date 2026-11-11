using Xunit.Abstractions;

namespace Sqarvo.Core.Tests.Support;

/// <summary>Locates the Rust backend executable built in this worktree.</summary>
internal static class BackendBinary
{
    private const string EnvironmentVariable = "SQARVO_BACKEND_BIN";

    internal static string Locate(ITestOutputHelper output)
    {
        var overridden = Environment.GetEnvironmentVariable(EnvironmentVariable);
        if (!string.IsNullOrWhiteSpace(overridden))
        {
            if (!File.Exists(overridden))
            {
                throw new InvalidOperationException($"{EnvironmentVariable} 指向的文件不存在：{overridden}");
            }

            return Path.GetFullPath(overridden);
        }

        var name = OperatingSystem.IsWindows() ? "sqarvo-backend.exe" : "sqarvo-backend";
        var roots = new[] { AppContext.BaseDirectory, Directory.GetCurrentDirectory() };
        foreach (var root in roots)
        {
            var directory = new DirectoryInfo(root);
            for (var depth = 0; depth < 10 && directory is not null; depth++)
            {
                foreach (var configuration in new[] { "debug", "release" })
                {
                    var candidate = Path.Combine(directory.FullName, "backend", "target", configuration, name);
                    if (File.Exists(candidate))
                    {
                        output.WriteLine($"后端可执行文件：{candidate}");
                        return candidate;
                    }
                }

                directory = directory.Parent;
            }
        }

        throw new InvalidOperationException(
            "找不到后端可执行文件。先运行 `cd backend && cargo build -p sqarvo-backend-service`，"
            + "或用 " + EnvironmentVariable + " 指定绝对路径。");
    }
}