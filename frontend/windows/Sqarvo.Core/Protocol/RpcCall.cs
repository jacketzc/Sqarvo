using Grpc.Core;
using Sqarvo.Core.Errors;

namespace Sqarvo.Core.Protocol;

/// <summary>
/// Single place that turns gRPC failures into <see cref="SqarvoException"/> instances, so every
/// client wrapper classifies errors identically.
/// </summary>
internal static class RpcCall
{
    /// <summary>gRPC metadata key carrying the local session token (must match the backend interceptor).</summary>
    internal const string TokenMetadataKey = "x-sqarvo-token";

    /// <summary>Computes an absolute deadline from a relative timeout.</summary>
    internal static DateTime DeadlineAfter(TimeSpan timeout) => DateTime.UtcNow + timeout;

    /// <summary>
    /// Awaits a unary call and maps every failure mode to a classified exception.
    /// </summary>
    /// <typeparam name="TResponse">Response message type.</typeparam>
    /// <param name="call">Call returned by a generated client.</param>
    /// <param name="operation">RPC name used in messages.</param>
    /// <param name="cancellationToken">
    /// Token the caller passed. When it is signalled the original
    /// <see cref="OperationCanceledException"/> is rethrown (caller-initiated cancellation), otherwise
    /// an expired deadline is reported as <see cref="SqarvoErrorKind.DeadlineExceeded"/>.
    /// </param>
    /// <returns>The response message.</returns>
    internal static async Task<TResponse> AwaitAsync<TResponse>(
        AsyncUnaryCall<TResponse> call,
        string operation,
        CancellationToken cancellationToken)
    {
        try
        {
            return await call.ResponseAsync.ConfigureAwait(false);
        }
        catch (RpcException exception)
        {
            throw RpcErrorMapper.Map(exception, operation);
        }
        catch (OperationCanceledException) when (cancellationToken.IsCancellationRequested)
        {
            throw;
        }
        catch (OperationCanceledException exception)
        {
            throw new SqarvoClientException(
                SqarvoErrorKind.DeadlineExceeded,
                operation,
                $"{operation} 超时：本地后端未在截止时间内响应",
                StatusCode.DeadlineExceeded,
                innerException: exception);
        }
        catch (HttpRequestException exception)
        {
            throw new SqarvoTransportException(
                $"{operation} 失败：本地 IPC 连接不可用（{exception.Message}）",
                exception);
        }
        catch (IOException exception)
        {
            throw new SqarvoTransportException(
                $"{operation} 失败：本地 IPC 连接中断（{exception.Message}）",
                exception);
        }
        catch (ObjectDisposedException exception)
        {
            throw new SqarvoTransportException($"{operation} 失败：连接已关闭", exception);
        }
    }
}