using System.Reflection;
using System.Xml.Linq;
using Sqarvo.Core.Clients;

namespace Sqarvo.Core.Tests;

/// <summary>
/// Guards the layering rules that make this project worth having: no UI dependency inside Core, and no
/// duplicated protobuf generation in the WinUI project.
/// </summary>
public sealed class ArchitectureTests
{
    private static readonly string[] ForbiddenUiAssemblies =
    {
        "Microsoft.UI",
        "Microsoft.WindowsAppSDK",
        "WinUI",
        "Windows.UI",
    };

    /// <summary>
    /// Type/namespace references only. Plain words like "WinUI" may legitimately appear in comments
    /// (for example explaining that this assembly must not be used from the window layer).
    /// </summary>
    private static readonly string[] ForbiddenUiTypes =
    {
        "Microsoft.UI",
        "Windows.UI",
        "Microsoft.WindowsAppSDK",
    };

    [Fact]
    public void CoreAssemblyDoesNotReferenceUiFrameworks()
    {
        var referenced = typeof(BackendConnection).Assembly
            .GetReferencedAssemblies()
            .Select(name => name.Name ?? string.Empty)
            .ToArray();

        foreach (var forbidden in ForbiddenUiAssemblies)
        {
            Assert.DoesNotContain(
                referenced,
                name => name.StartsWith(forbidden, StringComparison.OrdinalIgnoreCase));
        }
    }

    /// <summary>
    /// Core 的目标框架是 <c>net8.0</c>，SDK 只在 <c>net8.0-windows</c> 上定义 <c>WINDOWS</c> 符号，
    /// 所以任何 <c>#if WINDOWS</c> 都会**永远**走 <c>#else</c>：Windows 上编译出来的仍然是 Unix 分支，
    /// 运行到 <c>[DllImport("libc")]</c> 就是 <c>DllNotFoundException</c>（已在 Windows 实机复现过）。
    /// 平台分支必须用 <see cref="OperatingSystem.IsWindows"/> 这类运行时判断。
    /// </summary>
    [Fact]
    public void CoreSourcesDoNotUseTheWindowsPreprocessorSymbol()
    {
        var coreDirectory = Path.Combine(RepositoryRoot(), "frontend", "windows", "Sqarvo.Core");
        var offenders = Directory
            .EnumerateFiles(coreDirectory, "*.cs", SearchOption.AllDirectories)
            .Where(path => !path.Contains($"{Path.DirectorySeparatorChar}obj{Path.DirectorySeparatorChar}", StringComparison.Ordinal)
                && !path.Contains($"{Path.DirectorySeparatorChar}bin{Path.DirectorySeparatorChar}", StringComparison.Ordinal))
            .Where(path => File.ReadLines(path).Any(line =>
                line.TrimStart().StartsWith("#if WINDOWS", StringComparison.Ordinal)
                || line.TrimStart().StartsWith("#if !WINDOWS", StringComparison.Ordinal)
                || line.TrimStart().StartsWith("#elif WINDOWS", StringComparison.Ordinal)))
            .ToArray();

        Assert.Empty(offenders);
    }

    [Fact]
    public void CoreSourcesDoNotReferenceUiFrameworkTypes()
    {
        var coreDirectory = Path.Combine(RepositoryRoot(), "frontend", "windows", "Sqarvo.Core");
        var sources = Directory
            .EnumerateFiles(coreDirectory, "*.cs", SearchOption.AllDirectories)
            .Where(path => !path.Contains($"{Path.DirectorySeparatorChar}obj{Path.DirectorySeparatorChar}", StringComparison.Ordinal)
                && !path.Contains($"{Path.DirectorySeparatorChar}bin{Path.DirectorySeparatorChar}", StringComparison.Ordinal))
            .ToArray();

        Assert.NotEmpty(sources);

        foreach (var source in sources)
        {
            var text = File.ReadAllText(source);
            foreach (var forbidden in ForbiddenUiTypes)
            {
                Assert.False(
                    text.Contains(forbidden, StringComparison.Ordinal),
                    $"{Path.GetFileName(source)} 引用了 UI 框架类型 {forbidden}");
            }
        }
    }

    [Fact]
    public void WinUiProjectReferencesCoreAndNoLongerGeneratesItsOwnProtoStubs()
    {
        var projectPath = Path.Combine(
            RepositoryRoot(),
            "frontend",
            "windows",
            "Sqarvo.Windows",
            "Sqarvo.Windows.csproj");

        var project = XDocument.Load(projectPath);

        Assert.DoesNotContain(project.Descendants(), element => element.Name.LocalName == "Protobuf");

        var references = project
            .Descendants()
            .Where(element => element.Name.LocalName == "ProjectReference")
            .Select(element => (string?)element.Attribute("Include"))
            .Where(value => value is not null)
            .ToArray();

        Assert.Contains(references, value => value!.EndsWith("Sqarvo.Core.csproj", StringComparison.OrdinalIgnoreCase));

        var targetFramework = project
            .Descendants()
            .First(element => element.Name.LocalName == "TargetFramework")
            .Value;
        Assert.Equal("net8.0-windows10.0.19041.0", targetFramework);
    }

    [Fact]
    public void WinUiSourcesDoNotConstructChannelsOrGeneratedClients()
    {
        var projectDirectory = Path.Combine(
            RepositoryRoot(),
            "frontend",
            "windows",
            "Sqarvo.Windows");
        var sources = Directory
            .EnumerateFiles(projectDirectory, "*.cs", SearchOption.AllDirectories)
            .Where(path => !path.Contains($"{Path.DirectorySeparatorChar}obj{Path.DirectorySeparatorChar}", StringComparison.Ordinal)
                && !path.Contains($"{Path.DirectorySeparatorChar}bin{Path.DirectorySeparatorChar}", StringComparison.Ordinal))
            .ToArray();

        Assert.NotEmpty(sources);

        foreach (var source in sources)
        {
            var text = File.ReadAllText(source);
            Assert.False(
                text.Contains("GrpcChannel", StringComparison.Ordinal),
                $"{Path.GetFileName(source)} 直接建立了 gRPC 通道；通道只能由 Sqarvo.Core 装配");
            Assert.False(
                text.Contains("ServiceClient", StringComparison.Ordinal),
                $"{Path.GetFileName(source)} 直接使用了生成的 gRPC 客户端；应改用 Sqarvo.Core 的 Clients 封装");
        }
    }

    private static string RepositoryRoot()
    {
        var directory = new DirectoryInfo(AppContext.BaseDirectory);
        while (directory is not null)
        {
            if (File.Exists(Path.Combine(directory.FullName, "AGENTS.md"))
                && Directory.Exists(Path.Combine(directory.FullName, "proto")))
            {
                return directory.FullName;
            }

            directory = directory.Parent;
        }

        throw new InvalidOperationException($"无法从 {AppContext.BaseDirectory} 定位仓库根目录");
    }
}