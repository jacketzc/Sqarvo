namespace Sqarvo.Core.Errors;

/// <summary>
/// Base class for every failure raised by <c>Sqarvo.Core</c>. Carries a machine-readable
/// <see cref="Kind"/> so callers never need to match on message text.
/// </summary>
public abstract class SqarvoException : Exception
{
    /// <summary>Initialises the exception.</summary>
    /// <param name="kind">Machine-readable classification.</param>
    /// <param name="message">Safe-to-display message; must not contain the session token or credentials.</param>
    /// <param name="innerException">Underlying cause, when there is one.</param>
    protected SqarvoException(SqarvoErrorKind kind, string message, Exception? innerException = null)
        : base(message, innerException)
    {
        Kind = kind;
    }

    /// <summary>Machine-readable classification of this failure.</summary>
    public SqarvoErrorKind Kind { get; }

    /// <summary>
    /// Whether retrying the same operation may succeed without user intervention (for example a
    /// transient transport or availability failure).
    /// </summary>
    public virtual bool IsRetryable => Kind is SqarvoErrorKind.Unavailable
        or SqarvoErrorKind.TransportFailed
        or SqarvoErrorKind.DeadlineExceeded
        or SqarvoErrorKind.ResourceExhausted;
}