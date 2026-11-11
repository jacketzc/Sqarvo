namespace Sqarvo.Core.Platform;

/// <summary>
/// Description of the backend process to start. Kept transport- and platform-neutral so tests can
/// assert on it without spawning anything.
/// </summary>
public sealed class BackendLaunchOptions
{
    /// <summary>Absolute path to the backend executable.</summary>
    public required string ExecutablePath { get; init; }

    /// <summary>Arguments in order, excluding the executable name.</summary>
    public IReadOnlyList<string> Arguments { get; init; } = Array.Empty<string>();

    /// <summary>
    /// Environment variables to set on the child. Values override the inherited environment; a
    /// <see langword="null"/> value removes the variable.
    /// </summary>
    public IReadOnlyDictionary<string, string?> Environment { get; init; } =
        new Dictionary<string, string?>(StringComparer.Ordinal);

    /// <summary>Working directory for the child; inherited when <see langword="null"/>.</summary>
    public string? WorkingDirectory { get; init; }
}

/// <summary>
/// Starts backend processes. Injectable so that supervisor logic (endpoint polling, pid matching,
/// shutdown escalation) can be tested without spawning real processes.
/// </summary>
public interface IProcessLauncher
{
    /// <summary>Starts a process described by <paramref name="options"/>.</summary>
    /// <param name="options">Executable, arguments and environment.</param>
    /// <returns>A handle owning the new process.</returns>
    /// <exception cref="Errors.SqarvoProcessException">
    /// The process could not be started (<see cref="Errors.SqarvoErrorKind.BackendSpawnFailed"/>).
    /// </exception>
    IProcessHandle Launch(BackendLaunchOptions options);
}