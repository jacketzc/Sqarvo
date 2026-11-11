namespace Sqarvo.Core.Platform;

/// <summary>
/// Supplies the two locations the supervisor needs: where the backend writes its endpoint file, and
/// which executable to start.
/// </summary>
/// <remarks>
/// Injectable so packaging, tests and custom installations can redirect both without touching process
/// wide state. The default implementation mirrors the backend's own path derivation
/// (<c>directories::ProjectDirs</c> with qualifier <c>io</c>, organization <c>sqarvo</c>,
/// application <c>Sqarvo</c>).
/// </remarks>
public interface IPathLayout
{
    /// <summary>Absolute path of the endpoint description file.</summary>
    string EndpointFilePath { get; }

    /// <summary>
    /// Locates the backend executable.
    /// </summary>
    /// <returns>Absolute path of an existing executable.</returns>
    /// <exception cref="Errors.SqarvoProcessException">
    /// No candidate exists (<see cref="Errors.SqarvoErrorKind.BackendBinaryNotFound"/>); the message
    /// lists every path that was tried.
    /// </exception>
    string ResolveBackendBinary();

    /// <summary>
    /// Environment variables the child needs so it writes the endpoint file where this client reads
    /// it. A <see langword="null"/> value removes the variable from the child environment.
    /// </summary>
    /// <returns>Variables to apply on top of the inherited environment.</returns>
    IReadOnlyDictionary<string, string?> CreateChildEnvironment();
}