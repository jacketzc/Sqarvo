using Grpc.Core;
using Sqarvo.Core.Errors;
using Sqarvo.Core.Protocol;

namespace Sqarvo.Core.Tests;

public sealed class ErrorMappingTests
{
    [Fact]
    public void UnauthenticatedBecomesDedicatedAuthenticationError()
    {
        var trailers = new Metadata { { "sqarvo-error-code", "auth.invalid_token" } };
        var rpc = new RpcException(new Status(StatusCode.Unauthenticated, "本地会话 token 无效"), trailers);

        var mapped = RpcErrorMapper.Map(rpc, "SystemService.GetVersion");

        var exception = Assert.IsType<SqarvoAuthenticationException>(mapped);
        Assert.Equal(SqarvoErrorKind.Unauthenticated, exception.Kind);
        Assert.Equal(StatusCode.Unauthenticated, exception.GrpcStatus);
        Assert.Equal("auth.invalid_token", exception.ErrorCode);
        Assert.Equal("SystemService.GetVersion", exception.Operation);
        Assert.Contains("本地会话 token 无效", exception.Message, StringComparison.Ordinal);
        Assert.Contains("auth.invalid_token", exception.Message, StringComparison.Ordinal);
        Assert.False(exception.IsRetryable);
    }

    [Theory]
    [InlineData(StatusCode.NotFound, "metadata.object_not_found", SqarvoErrorKind.NotFound)]
    [InlineData(StatusCode.InvalidArgument, "config.invalid", SqarvoErrorKind.InvalidArgument)]
    [InlineData(StatusCode.FailedPrecondition, "table_data.no_primary_key", SqarvoErrorKind.FailedPrecondition)]
    [InlineData(StatusCode.PermissionDenied, "metadata.permission_denied", SqarvoErrorKind.PermissionDenied)]
    [InlineData(StatusCode.Unimplemented, "capability.unsupported", SqarvoErrorKind.Unimplemented)]
    [InlineData(StatusCode.ResourceExhausted, "query.limit_reached", SqarvoErrorKind.ResourceExhausted)]
    [InlineData(StatusCode.DeadlineExceeded, "backend.timeout", SqarvoErrorKind.DeadlineExceeded)]
    [InlineData(StatusCode.Unavailable, "transport.failed", SqarvoErrorKind.Unavailable)]
    [InlineData(StatusCode.Aborted, "query.id_in_use", SqarvoErrorKind.Aborted)]
    [InlineData(StatusCode.Internal, "backend.internal", SqarvoErrorKind.Internal)]
    public void MapsStatusCodesToKinds(StatusCode statusCode, string errorCode, SqarvoErrorKind expected)
    {
        var trailers = new Metadata { { "sqarvo-error-code", errorCode } };
        var rpc = new RpcException(new Status(statusCode, "服务端文案"), trailers);

        var exception = RpcErrorMapper.Map(rpc, "QueryService.Execute");

        Assert.Equal(typeof(SqarvoClientException), exception.GetType());
        Assert.Equal(expected, exception.Kind);
        Assert.Equal(errorCode, ((SqarvoClientException)exception).ErrorCode);
        Assert.Equal(statusCode, ((SqarvoClientException)exception).GrpcStatus);
    }

    [Fact]
    public void RetryabilityFollowsTheFailureClass()
    {
        Assert.True(RpcErrorMapper.Map(new RpcException(new Status(StatusCode.Unavailable, "down")), "op").IsRetryable);
        Assert.False(RpcErrorMapper.Map(new RpcException(new Status(StatusCode.InvalidArgument, "bad")), "op").IsRetryable);
        Assert.False(RpcErrorMapper.Map(new RpcException(new Status(StatusCode.Unimplemented, "nope")), "op").IsRetryable);
    }

    [Fact]
    public void MissingErrorCodeMetadataIsTolerated()
    {
        var mapped = RpcErrorMapper.Map(new RpcException(new Status(StatusCode.NotFound, "gone")), "op");

        var exception = Assert.IsType<SqarvoClientException>(mapped);
        Assert.Null(exception.ErrorCode);
        Assert.Contains("gone", exception.Message, StringComparison.Ordinal);
    }

    [Fact]
    public void BinaryTrailerIsIgnoredInsteadOfCrashing()
    {
        var trailers = new Metadata { { "sqarvo-error-code-bin", new byte[] { 1, 2, 3 } } };
        var mapped = RpcErrorMapper.Map(new RpcException(new Status(StatusCode.Internal, "boom"), trailers), "op");

        Assert.Null(Assert.IsType<SqarvoClientException>(mapped).ErrorCode);
    }
}