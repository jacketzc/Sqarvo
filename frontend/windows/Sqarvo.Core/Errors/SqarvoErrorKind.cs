namespace Sqarvo.Core.Errors;

/// <summary>
/// Classification of a client-side failure. Callers branch on this instead of parsing messages,
/// so wording can change without breaking behaviour.
/// </summary>
/// <remarks>
/// Values below <see cref="TransportFailed"/> describe local lifecycle failures (endpoint file,
/// backend process); the rest mirror the gRPC status codes the backend returns plus
/// <see cref="TransportFailed"/>, which is raised before any RPC reaches the server.
/// </remarks>
public enum SqarvoErrorKind
{
    /// <summary>The endpoint file exists but its contents violate the contract.</summary>
    EndpointInvalid,

    /// <summary>The endpoint file exists but could not be read (permissions, I/O error).</summary>
    EndpointUnreadable,

    /// <summary>The backend declares a protocol major version this client does not support.</summary>
    ProtocolIncompatible,

    /// <summary>The backend protocol version string is not of the form <c>v&lt;major&gt;</c>.</summary>
    ProtocolUnparsable,

    /// <summary>No backend executable could be located.</summary>
    BackendBinaryNotFound,

    /// <summary>Starting the backend process failed.</summary>
    BackendSpawnFailed,

    /// <summary>The backend process exited before it published an endpoint.</summary>
    BackendExitedEarly,

    /// <summary>The backend did not publish a matching endpoint file within the timeout.</summary>
    BackendStartTimeout,

    /// <summary>An operation needs an acquired backend, but none was acquired yet.</summary>
    BackendNotAcquired,

    /// <summary>The backend process could not be stopped.</summary>
    ShutdownFailed,

    /// <summary>The local IPC connection failed (refused, reset, loopback policy, unsupported transport).</summary>
    TransportFailed,

    /// <summary>The backend rejected the request because the session token is missing or wrong.</summary>
    Unauthenticated,

    /// <summary>The call was cancelled.</summary>
    Cancelled,

    /// <summary>The call or the server-side deadline expired.</summary>
    DeadlineExceeded,

    /// <summary>The request arguments are invalid (server-side validation included).</summary>
    InvalidArgument,

    /// <summary>The referenced object does not exist.</summary>
    NotFound,

    /// <summary>The object already exists.</summary>
    AlreadyExists,

    /// <summary>The operation is not permitted.</summary>
    PermissionDenied,

    /// <summary>The current state does not allow the operation.</summary>
    FailedPrecondition,

    /// <summary>A resource limit (for example the concurrent query limit) was hit.</summary>
    ResourceExhausted,

    /// <summary>The backend or driver does not implement the capability.</summary>
    Unimplemented,

    /// <summary>The operation was aborted (concurrency conflict).</summary>
    Aborted,

    /// <summary>A value was out of the accepted range.</summary>
    OutOfRange,

    /// <summary>An internal backend error.</summary>
    Internal,

    /// <summary>The backend is temporarily unavailable.</summary>
    Unavailable,

    /// <summary>Data loss or corruption was reported.</summary>
    DataLoss,

    /// <summary>Any other failure.</summary>
    Unknown,
}