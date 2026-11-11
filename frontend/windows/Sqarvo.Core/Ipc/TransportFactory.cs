using Sqarvo.Core.Errors;

namespace Sqarvo.Core.Ipc;

/// <summary>
/// The single place where the in-process transport implementation is selected from an endpoint
/// description. Nothing else in <c>Sqarvo.Core</c> branches on <see cref="TransportKind"/>.
/// </summary>
public static class TransportFactory
{
    /// <summary>
    /// Creates the transport for an endpoint.
    /// </summary>
    /// <param name="endpoint">Validated endpoint description read from disk.</param>
    /// <returns>The transport implementation for the declared kind.</returns>
    /// <exception cref="SqarvoTransportException">
    /// The endpoint declares a transport this client does not implement. A Unix socket cannot be
    /// reached from a Windows client, so the failure is explicit instead of silently falling back to
    /// something else (a named-pipe implementation can be added here later without touching callers).
    /// </exception>
    public static ITransport Create(EndpointInfo endpoint)
    {
        ArgumentNullException.ThrowIfNull(endpoint);

        return endpoint.Transport switch
        {
            TransportKind.Tcp => new TcpTransport(endpoint),
            _ => throw new SqarvoTransportException(
                $"本客户端只实现回环 TCP 传输，端点声明的是 {endpoint.Transport}；"
                + "对应平台的传输实现（如命名管道）尚未提供"),
        };
    }
}