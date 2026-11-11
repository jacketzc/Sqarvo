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