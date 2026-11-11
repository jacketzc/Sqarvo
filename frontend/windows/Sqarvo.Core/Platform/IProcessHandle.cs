namespace Sqarvo.Core.Platform;

/// <summary>
/// Handle to a backend process started by <see cref="IProcessLauncher"/>.
/// </summary>
/// <remarks>
/// The handle owns the process: disposing it releases platform resources (on Windows the job object
/// handle, whose closure terminates the process if it is somehow still running) and never leaves a
/// reaped-but-unwaited child behind.
/// </remarks>
public interface IProcessHandle : IDisposable
{
    /// <summary>Operating system process id.</summary>
    int Id { get; }

    /// <summary>Whether the process has exited.</summary>
    bool HasExited { get; }

    /// <summary>Exit code when the process has exited; otherwise <see langword="null"/>.</summary>
    int? ExitCode { get; }

    /// <summary>
    /// Tail of the process's standard error and standard output, used to explain a failed start.
    /// Bounded in size; never contains the session token (the backend does not log it).
    /// </summary>
    string CapturedDiagnostics { get; }

    /// <summary>
    /// Asks the process to exit gracefully.
    /// </summary>
    /// <returns>
    /// <see langword="true"/> when a graceful signal was delivered (Unix: <c>SIGTERM</c>, which the
    /// backend handles by cleaning up its socket and endpoint file). <see langword="false"/> on
    /// platforms without such a signal (Windows: no <c>SIGTERM</c> and the contract has no shutdown
    /// RPC), where the caller must terminate instead of waiting for a graceful exit that cannot happen.
    /// </returns>
    bool RequestGracefulExit();

    /// <summary>Terminates the process unconditionally (Unix <c>SIGKILL</c>, Windows <c>TerminateProcess</c>).</summary>
    void Kill();

    /// <summary>
    /// Waits for the process to exit.
    /// </summary>
    /// <param name="timeout">Upper bound to wait.</param>
    /// <param name="cancellationToken">Abandons the wait (the process is not killed).</param>
    /// <returns><see langword="true"/> when the process has exited within the timeout.</returns>
    Task<bool> WaitForExitAsync(TimeSpan timeout, CancellationToken cancellationToken = default);
}