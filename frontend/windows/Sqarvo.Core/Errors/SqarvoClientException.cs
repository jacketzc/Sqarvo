using Grpc.Core;

namespace Sqarvo.Core.Errors;

/// <summary>
/// A failure reported by the backend over gRPC, already mapped from <see cref="RpcException"/>.
/// </summary>
public class SqarvoClientException : SqarvoException
{
    /// <summary>Initialises the exception.</summary>
    /// <param name="kind">Classification derived from the gRPC status code.</param>
    /// <param name="operation">Name of the RPC that failed, for example <c>SystemService.GetVersion</c>.</param>
    /// <param name="message">Safe-to-display message.</param>
    /// <param name="grpcStatus">Raw gRPC status code returned by the backend.</param>
    /// <param name="errorCode">Stable backend error code from the <c>sqarvo-error-code</c> trailer, if present.</param>
    /// <param name="innerException">Underlying cause, when there is one.</param>
    public SqarvoClientException(
        SqarvoErrorKind kind,
        string operation,
        string message,
        StatusCode grpcStatus,
        string? errorCode = null,
        Exception? innerException = null)
        : base(kind, message, innerException)
    {
        Operation = operation;
        GrpcStatus = grpcStatus;
        ErrorCode = errorCode;
    }

    /// <summary>Name of the RPC that failed, for example <c>SystemService.GetVersion</c>.</summary>
    public string Operation { get; }

    /// <summary>Raw gRPC status code returned by the backend.</summary>
    public StatusCode GrpcStatus { get; }

    /// <summary>
    /// Stable backend error code taken from the <c>sqarvo-error-code</c> trailer (for example
    /// <c>metadata.object_not_found</c>), or <see langword="null"/> when the server did not send one.
    /// </summary>
    public string? ErrorCode { get; }
}