using Grpc.Core;
using Sqarvo.Core.Errors;

namespace Sqarvo.Core.Protocol;

/// <summary>
/// Translates backend failures into classified client exceptions.
/// </summary>
/// <remarks>
/// The backend puts a stable error code in the <c>sqarvo-error-code</c> trailing metadata
/// (see <c>backend/crates/rpc-server/src/error.rs</c>) and a human readable message in the status
/// detail. Both are preserved: <see cref="SqarvoClientException.ErrorCode"/> for branching,
/// <see cref="Exception.Message"/> for display.
/// </remarks>
public static class RpcErrorMapper
{
    /// <summary>gRPC metadata key carrying the stable backend error code.</summary>
    public const string ErrorCodeMetadataKey = "sqarvo-error-code";

    /// <summary>
    /// Maps an <see cref="RpcException"/> to a classified <see cref="SqarvoException"/>.
    /// </summary>
    /// <param name="exception">Exception thrown by a generated client call.</param>
    /// <param name="operation">RPC name used in the message, for example <c>SystemService.GetVersion</c>.</param>
    /// <returns>
    /// <see cref="SqarvoAuthenticationException"/> for <see cref="StatusCode.Unauthenticated"/>,
    /// otherwise a <see cref="SqarvoClientException"/> carrying the mapped kind.
    /// </returns>
    public static SqarvoException Map(RpcException exception, string operation)
    {
        ArgumentNullException.ThrowIfNull(exception);

        var status = exception.Status;
        var errorCode = ReadErrorCode(exception);
        var detail = string.IsNullOrWhiteSpace(status.Detail) ? status.StatusCode.ToString() : status.Detail;
        var message = $"{operation} 失败：{detail}"
            + (errorCode is null ? string.Empty : $"（错误码 {errorCode}）");

        if (status.StatusCode == StatusCode.Unauthenticated)
        {
            return new SqarvoAuthenticationException(operation, message, errorCode, exception);
        }

        return new SqarvoClientException(
            MapStatusCode(status.StatusCode),
            operation,
            message,
            status.StatusCode,
            errorCode,
            exception);
    }

    /// <summary>
    /// Maps a gRPC status code to an error kind. Unknown codes fall back to
    /// <see cref="SqarvoErrorKind.Unknown"/> instead of throwing, so a newer server cannot crash an
    /// older client.
    /// </summary>
    /// <param name="statusCode">gRPC status code reported by the backend.</param>
    /// <returns>The matching classification.</returns>
    public static SqarvoErrorKind MapStatusCode(StatusCode statusCode) => statusCode switch
    {
        StatusCode.OK => SqarvoErrorKind.Unknown,
        StatusCode.Cancelled => SqarvoErrorKind.Cancelled,
        StatusCode.Unknown => SqarvoErrorKind.Unknown,
        StatusCode.InvalidArgument => SqarvoErrorKind.InvalidArgument,
        StatusCode.DeadlineExceeded => SqarvoErrorKind.DeadlineExceeded,
        StatusCode.NotFound => SqarvoErrorKind.NotFound,
        StatusCode.AlreadyExists => SqarvoErrorKind.AlreadyExists,
        StatusCode.PermissionDenied => SqarvoErrorKind.PermissionDenied,
        StatusCode.ResourceExhausted => SqarvoErrorKind.ResourceExhausted,
        StatusCode.FailedPrecondition => SqarvoErrorKind.FailedPrecondition,
        StatusCode.Aborted => SqarvoErrorKind.Aborted,
        StatusCode.OutOfRange => SqarvoErrorKind.OutOfRange,
        StatusCode.Unimplemented => SqarvoErrorKind.Unimplemented,
        StatusCode.Internal => SqarvoErrorKind.Internal,
        StatusCode.Unavailable => SqarvoErrorKind.Unavailable,
        StatusCode.DataLoss => SqarvoErrorKind.DataLoss,
        StatusCode.Unauthenticated => SqarvoErrorKind.Unauthenticated,
        _ => SqarvoErrorKind.Unknown,
    };

    private static string? ReadErrorCode(RpcException exception)
    {
        // tonic sends errors as a trailers-only response, so grpc-dotnet surfaces the error code in
        // RpcException.Trailers.
        var entry = exception.Trailers.Get(ErrorCodeMetadataKey);
        return entry is null || entry.IsBinary || string.IsNullOrWhiteSpace(entry.Value) ? null : entry.Value;
    }
}