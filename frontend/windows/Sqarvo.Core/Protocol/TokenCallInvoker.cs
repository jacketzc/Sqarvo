using Grpc.Core;

namespace Sqarvo.Core.Protocol;

/// <summary>
/// <see cref="CallInvoker"/> that stamps the session token on every outgoing call and applies a
/// default deadline when the caller did not set one.
/// </summary>
/// <remarks>
/// <para>
/// Injecting at the invoker level (rather than adding metadata at each call site) is deliberate: the
/// token is the only access control on the local channel, so it must be impossible to forget. A
/// caller cannot override it either - a supplied <c>x-sqarvo-token</c> header is dropped and replaced.
/// </para>
/// <para>
/// The token is never rendered: <see cref="ToString"/> only reports its length.
/// </para>
/// </remarks>
internal sealed class TokenCallInvoker : CallInvoker
{
    private readonly CallInvoker _inner;
    private readonly Metadata _sessionHeaders;
    private readonly TimeSpan _defaultTimeout;

    /// <summary>Wraps an inner invoker.</summary>
    /// <param name="inner">Invoker created from the gRPC channel.</param>
    /// <param name="token">Validated session token (hexadecimal, so it is a legal ASCII metadata value).</param>
    /// <param name="defaultTimeout">Deadline applied when the caller passes none.</param>
    internal TokenCallInvoker(CallInvoker inner, string token, TimeSpan defaultTimeout)
    {
        _inner = inner;
        _sessionHeaders = new Metadata { { RpcCall.TokenMetadataKey, token } };
        _defaultTimeout = defaultTimeout;
    }

    /// <inheritdoc />
    public override TResponse BlockingUnaryCall<TRequest, TResponse>(
        Method<TRequest, TResponse> method,
        string? host,
        CallOptions options,
        TRequest request) =>
        _inner.BlockingUnaryCall(method, host, Prepare(options), request);

    /// <inheritdoc />
    public override AsyncUnaryCall<TResponse> AsyncUnaryCall<TRequest, TResponse>(
        Method<TRequest, TResponse> method,
        string? host,
        CallOptions options,
        TRequest request) =>
        _inner.AsyncUnaryCall(method, host, Prepare(options), request);

    /// <inheritdoc />
    public override AsyncServerStreamingCall<TResponse> AsyncServerStreamingCall<TRequest, TResponse>(
        Method<TRequest, TResponse> method,
        string? host,
        CallOptions options,
        TRequest request) =>
        _inner.AsyncServerStreamingCall(method, host, Prepare(options), request);

    /// <inheritdoc />
    public override AsyncClientStreamingCall<TRequest, TResponse> AsyncClientStreamingCall<TRequest, TResponse>(
        Method<TRequest, TResponse> method,
        string? host,
        CallOptions options) =>
        _inner.AsyncClientStreamingCall<TRequest, TResponse>(method, host, Prepare(options));

    /// <inheritdoc />
    public override AsyncDuplexStreamingCall<TRequest, TResponse> AsyncDuplexStreamingCall<TRequest, TResponse>(
        Method<TRequest, TResponse> method,
        string? host,
        CallOptions options) =>
        _inner.AsyncDuplexStreamingCall<TRequest, TResponse>(method, host, Prepare(options));

    private CallOptions Prepare(CallOptions options)
    {
        var headers = MergeHeaders(options.Headers);
        var deadline = options.Deadline ?? DateTime.UtcNow + _defaultTimeout;

        return new CallOptions(
            headers,
            deadline,
            options.CancellationToken,
            options.WriteOptions,
            options.PropagationToken,
            options.Credentials);
    }

    private Metadata MergeHeaders(Metadata? callerHeaders)
    {
        var merged = new Metadata();
        foreach (var entry in _sessionHeaders)
        {
            merged.Add(entry);
        }

        if (callerHeaders is null)
        {
            return merged;
        }

        foreach (var entry in callerHeaders)
        {
            // Never let a caller replace the session token.
            if (!string.Equals(entry.Key, RpcCall.TokenMetadataKey, StringComparison.OrdinalIgnoreCase))
            {
                merged.Add(entry);
            }
        }

        return merged;
    }

    /// <summary>Diagnostic representation that never contains the token.</summary>
    public override string ToString() => $"TokenCallInvoker(defaultTimeout={_defaultTimeout}, token=<redacted>)";
}