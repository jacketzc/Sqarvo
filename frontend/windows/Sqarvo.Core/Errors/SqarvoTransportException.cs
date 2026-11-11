namespace Sqarvo.Core.Errors;

/// <summary>
/// The local IPC transport failed before an RPC could complete: unsupported transport kind,
/// connection refused, or a peer that is not the loopback backend.
/// </summary>
public sealed class SqarvoTransportException : SqarvoException
{
    /// <summary>Initialises the exception.</summary>
    /// <param name="message">Safe-to-display message (never contains the token).</param>
    /// <param name="innerException">Underlying cause, when there is one.</param>
    public SqarvoTransportException(string message, Exception? innerException = null)
        : base(SqarvoErrorKind.TransportFailed, message, innerException)
    {
    }
}