using Sqarvo.Core.Errors;
using Sqarvo.Core.Ipc;

namespace Sqarvo.Core.Platform;

/// <summary>
/// Default <see cref="IPathLayout"/>: derives the endpoint file path from the same conventions as the
/// backend and finds the backend executable next to the client, in the repository build output, or
/// through an environment variable.
/// </summary>
/// <remarks>
/// <para>
/// Path derivation (must match <c>backend/crates/local-store/src/paths.rs</c>):
/// </para>
/// <list type="bullet">
/// <item><description>
/// Linux: <c>$XDG_RUNTIME_DIR/sqarvo/backend-endpoint.json</c>, falling back to
/// <c>$XDG_DATA_HOME/sqarvo/run/</c> (or <c>~/.local/share/sqarvo/run/</c>) when no runtime directory
/// exists.
/// </description></item>
/// <item><description>
/// Windows: <c>%APPDATA%\sqarvo\Sqarvo\data\run\backend-endpoint.json</c> - the <c>data_dir</c> of
/// <c>directories::ProjectDirs</c>, whose <c>runtime_dir</c> is <c>None</c> on Windows. This value has
/// not been measured on a real Windows host yet (plan §7) and is therefore overridable, which is what
/// the packaging step is expected to do once measured.
/// </description></item>
/// <item><description>Other platforms: <c>~/Library/Application Support/io.sqarvo.Sqarvo/run/</c>.</description></item>
/// </list>
/// <para>
/// <c>SQARVO_ENDPOINT_FILE</c> overrides the path, and <c>SQARVO_BACKEND_BIN</c> pins the executable.
/// When the endpoint path has the standard <c>&lt;root&gt;/sqarvo/backend-endpoint.json</c> shape, the
/// child is given <c>XDG_RUNTIME_DIR=&lt;root&gt;</c> so it writes where this client reads. The
/// backend has no <c>--endpoint-file</c> flag yet, hence the environment variable (plan WP1).
/// </para>
/// </remarks>
public sealed class DefaultPathLayout : IPathLayout
{
    /// <summary>Environment variable pinning the backend executable.</summary>
    public const string BackendBinaryEnvironmentVariable = "SQARVO_BACKEND_BIN";

    /// <summary>Backend executable base name (the Rust package is <c>sqarvo-backend-service</c>).</summary>
    public const string BackendBinaryName = "sqarvo-backend";

    /// <summary>Runtime sub-directory used by <c>directories::ProjectDirs</c> on Linux.</summary>
    public const string RuntimeProjectDirectory = "sqarvo";

    private const int MaxParentSearchDepth = 8;

    private readonly string? _backendBinaryOverride;

    /// <summary>Creates a layout from the platform defaults, honouring the environment overrides.</summary>
    public DefaultPathLayout()
        : this(ResolveEndpointFilePath(), backendBinaryOverride: null)
    {
    }

    private DefaultPathLayout(string endpointFilePath, string? backendBinaryOverride)
    {
        EndpointFilePath = endpointFilePath;
        _backendBinaryOverride = backendBinaryOverride;
    }

    /// <summary>
    /// Creates a layout with an explicit endpoint file path (tests, packaging) and optionally an
    /// explicit backend executable.
    /// </summary>
    /// <param name="endpointFilePath">Absolute path the backend should write its endpoint file to.</param>
    /// <param name="backendBinaryOverride">Backend executable to use; discovered when omitted.</param>
    /// <returns>A layout pinned to those paths.</returns>
    public static DefaultPathLayout ForEndpointFile(string endpointFilePath, string? backendBinaryOverride = null)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(endpointFilePath);
        return new DefaultPathLayout(Path.GetFullPath(endpointFilePath), backendBinaryOverride);
    }

    /// <inheritdoc />
    public string EndpointFilePath { get; }

    /// <inheritdoc />
    public string ResolveBackendBinary()
    {
        var binaryName = OperatingSystem.IsWindows() ? BackendBinaryName + ".exe" : BackendBinaryName;

        if (!string.IsNullOrWhiteSpace(_backendBinaryOverride))
        {
            return RequireExisting(_backendBinaryOverride, only: _backendBinaryOverride);
        }

        var fromEnvironment = Environment.GetEnvironmentVariable(BackendBinaryEnvironmentVariable);
        if (!string.IsNullOrWhiteSpace(fromEnvironment))
        {
            // An explicit override that does not exist is an error, not a reason to guess another binary.
            return RequireExisting(fromEnvironment, only: fromEnvironment);
        }

        var searched = new List<string>();
        foreach (var candidate in CandidateBinaryPaths(binaryName))
        {
            if (searched.Contains(candidate, StringComparer.Ordinal))
            {
                continue;
            }

            searched.Add(candidate);
            if (File.Exists(candidate))
            {
                return candidate;
            }
        }

        throw new SqarvoProcessException(
            SqarvoErrorKind.BackendBinaryNotFound,
            $"找不到后端可执行文件 {binaryName}；已尝试：{string.Join("；", searched)}。"
            + $"可用环境变量 {BackendBinaryEnvironmentVariable} 指定绝对路径。");
    }

    /// <inheritdoc />
    public IReadOnlyDictionary<string, string?> CreateChildEnvironment()
    {
        var environment = new Dictionary<string, string?>(StringComparer.Ordinal);

        // Windows: the backend derives %APPDATA%\sqarvo\Sqarvo\data\run by itself, no override needed.
        // On Unix the runtime directory comes from XDG_RUNTIME_DIR; point it at the directory that
        // contains the standard "sqarvo" project directory so the child writes the file this client
        // reads.
        if (!OperatingSystem.IsWindows())
        {
            var runtimeRoot = ChildRuntimeRoot(EndpointFilePath);
            if (runtimeRoot is not null)
            {
                environment["XDG_RUNTIME_DIR"] = runtimeRoot;
            }
        }

        return environment;
    }

    /// <summary>
    /// Returns <c>&lt;root&gt;</c> when <paramref name="endpointFilePath"/> has the standard
    /// <c>&lt;root&gt;/sqarvo/backend-endpoint.json</c> shape, otherwise <see langword="null"/>.
    /// </summary>
    /// <param name="endpointFilePath">Endpoint file path.</param>
    /// <returns>The runtime root the child must use, or <see langword="null"/>.</returns>
    public static string? ChildRuntimeRoot(string endpointFilePath)
    {
        var projectDirectory = Path.GetDirectoryName(endpointFilePath);
        if (projectDirectory is null
            || !string.Equals(Path.GetFileName(projectDirectory), RuntimeProjectDirectory, StringComparison.Ordinal)
            || !string.Equals(Path.GetFileName(endpointFilePath), EndpointFile.FileName, StringComparison.Ordinal))
        {
            return null;
        }

        var root = Path.GetDirectoryName(projectDirectory);
        return string.IsNullOrEmpty(root) ? null : root;
    }

    private static IEnumerable<string> CandidateBinaryPaths(string binaryName)
    {
        var baseDirectory = AppContext.BaseDirectory;
        yield return Path.Combine(baseDirectory, binaryName);

        var roots = new List<string> { baseDirectory };
        var currentDirectory = Directory.GetCurrentDirectory();
        if (!string.IsNullOrEmpty(currentDirectory) && !string.Equals(currentDirectory, baseDirectory, StringComparison.Ordinal))
        {
            roots.Add(currentDirectory);
        }

        foreach (var root in roots)
        {
            var parent = root;
            for (var depth = 1; depth <= MaxParentSearchDepth; depth++)
            {
                parent = Path.GetDirectoryName(parent) ?? parent;
                foreach (var configuration in new[] { "debug", "release" })
                {
                    yield return Path.GetFullPath(
                        Path.Combine(parent, "backend", "target", configuration, binaryName));
                }
            }
        }
    }

    private static string RequireExisting(string path, string only)
    {
        var full = Path.GetFullPath(path);
        if (File.Exists(full))
        {
            return full;
        }

        throw new SqarvoProcessException(
            SqarvoErrorKind.BackendBinaryNotFound,
            $"后端可执行文件不存在：{only}（解析为 {full}）。"
            + $"请修正 {BackendBinaryEnvironmentVariable} 或先构建 backend。");
    }

    private static string ResolveEndpointFilePath()
    {
        var overridden = Environment.GetEnvironmentVariable(EndpointFile.PathEnvironmentVariable);
        if (!string.IsNullOrWhiteSpace(overridden))
        {
            return Path.GetFullPath(overridden);
        }

        return DefaultEndpointFilePath();
    }

    private static string DefaultEndpointFilePath()
    {
#if WINDOWS
        var appData = Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData);
        return Path.Combine(appData, "sqarvo", "Sqarvo", "data", "run", EndpointFile.FileName);
#else
        if (OperatingSystem.IsLinux())
        {
            var runtimeDirectory = Environment.GetEnvironmentVariable("XDG_RUNTIME_DIR");
            if (!string.IsNullOrWhiteSpace(runtimeDirectory))
            {
                return Path.Combine(runtimeDirectory, RuntimeProjectDirectory, EndpointFile.FileName);
            }

            var dataHome = Environment.GetEnvironmentVariable("XDG_DATA_HOME");
            var dataRoot = string.IsNullOrWhiteSpace(dataHome)
                ? Path.Combine(HomeDirectory(), ".local", "share")
                : dataHome;

            return Path.Combine(dataRoot, RuntimeProjectDirectory, "run", EndpointFile.FileName);
        }

        // macOS: directories::ProjectDirs data_dir, runtime_dir == None.
        return Path.Combine(
            HomeDirectory(),
            "Library",
            "Application Support",
            "io.sqarvo.Sqarvo",
            "run",
            EndpointFile.FileName);
#endif
    }

#if !WINDOWS
    private static string HomeDirectory() =>
        Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
#endif
}