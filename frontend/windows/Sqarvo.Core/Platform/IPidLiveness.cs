namespace Sqarvo.Core.Platform;

/// <summary>
/// Answers "is this pid still running?" for the backend endpoint file, so a stale endpoint left by a
/// crashed backend is not mistaken for a live one.
/// </summary>
/// <remarks>
/// Injectable because the check is platform specific and because tests must be able to script it
/// without real processes.
/// </remarks>
public interface IPidLiveness
{
    /// <summary>
    /// Returns whether the process is alive. A pid that does not exist, or that only exists as an
    /// unreaped zombie, counts as dead.
    /// </summary>
    /// <param name="pid">Process id from the endpoint file.</param>
    /// <returns><see langword="true"/> when the process still runs.</returns>
    bool IsAlive(int pid);
}