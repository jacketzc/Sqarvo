namespace Sqarvo.Core.Errors;

/// <summary>
/// A failure around the backend process lifecycle: locating, starting, waiting for, or stopping it.
/// </summary>
public sealed class SqarvoProcessException : SqarvoException
{
    /// <summary>Initialises the exception.</summary>
    /// <param name="kind">Lifecycle classification (spawn, early exit, timeout, shutdown, ...).</param>
    /// <param name="message">Safe-to-display message.</param>
    /// <param name="processId">Backend process id, when one was started.</param>
    /// <param name="capturedError">Tail of the backend's standard error output, when captured.</param>
    /// <param name="innerException">Underlying cause, when there is one.</param>
    public SqarvoProcessException(
        SqarvoErrorKind kind,
        string message,
        int? processId = null,
        string? capturedError = null,
        Exception? innerException = null)
        : base(kind, message, innerException)
    {
        ProcessId = processId;
        CapturedError = capturedError;
    }

    /// <summary>Backend process id, when one was started.</summary>
    public int? ProcessId { get; }

    /// <summary>
    /// Tail of the backend's standard error output captured at failure time. Diagnostics only; the
    /// backend never logs the token, and this client never adds it.
    /// </summary>
    public string? CapturedError { get; }
}