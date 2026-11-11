using Grpc.Core;

namespace Sqarvo.Core.Errors;

/// <summary>
/// The backend rejected a request because the local session token was missing or did not match.
/// </summary>
/// <remarks>
/// This is the dedicated type for <see cref="StatusCode.Unauthenticated"/>. It exists because the
/// token is the only access control on the local IPC channel: a stale token (for example after the
/// backend restarted) must be distinguishable from a genuine RPC failure so the caller can
/// re-read the endpoint file and reconnect instead of showing an error dialog.
/// </remarks>
public sealed class SqarvoAuthenticationException : SqarvoClientException
{
    /// <summary>Initialises the exception.</summary>
    /// <param name="operation">Name of the RPC that failed.</param>
    /// <param name="message">Safe-to-display message; never contains the token.</param>
    /// <param name="errorCode">Stable backend error code, if the server sent one.</param>
    /// <param name="innerException">Underlying cause, when there is one.</param>
    public SqarvoAuthenticationException(
        string operation,
        string message,
        string? errorCode = null,
        Exception? innerException = null)
        : base(SqarvoErrorKind.Unauthenticated, operation, message, StatusCode.Unauthenticated, errorCode, innerException)
    {
    }
}