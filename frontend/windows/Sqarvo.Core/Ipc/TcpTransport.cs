using System.Net;
using System.Net.Sockets;
using Sqarvo.Core.Errors;

namespace Sqarvo.Core.Ipc;

/// <summary>
/// Loopback TCP transport: the transport the Windows backend uses (see AGENTS.md §2.3, the fallback
/// when named pipes are not available).
/// </summary>
/// <remarks>
/// <para>
/// Two security properties are enforced here rather than trusted from the endpoint file:
/// </para>
/// <list type="bullet">
/// <item><description>
/// The host must be a <b>loopback IP literal</b> (<c>127.0.0.1</c> or <c>::1</c>). Host names are
/// rejected instead of resolved, so a poisoned resolver or a stray <c>hosts</c> entry cannot redirect
/// the channel off the machine. The address is used directly by the connect callback; DNS is never
/// consulted.
/// </description></item>
/// <item><description>
/// Proxy detection is disabled (<c>UseProxy = false</c>) and redirects are refused, so an
/// environment-configured HTTP proxy cannot intercept the channel.
/// </description></item>
/// </list>
/// <para>
/// <c>SocketsHttpHandler</c> is used directly rather than wrapping it in <c>HttpClientHandler</c>:
/// only the former exposes <c>ConnectCallback</c>, which is what makes the "connect to this exact
/// loopback address, never anything else" guarantee enforceable. <c>HttpClientHandler</c> would also
/// silently enable system proxy and redirect behaviour.
/// </para>
/// </remarks>
public sealed class TcpTransport : ITransport
{
    private const int ConnectTimeoutSeconds = 5;

    private readonly IPAddress _address;
    private readonly int _port;

    /// <summary>Creates a loopback TCP transport for a validated endpoint.</summary>
    /// <param name="endpoint">Endpoint whose transport is <see cref="TransportKind.Tcp"/>.</param>
    /// <exception cref="ArgumentException">The endpoint uses a different transport kind.</exception>
    /// <exception cref="SqarvoEndpointException">The host is not a loopback IP literal, or the port is missing.</exception>
    public TcpTransport(EndpointInfo endpoint)
    {
        ArgumentNullException.ThrowIfNull(endpoint);

        if (endpoint.Transport != TransportKind.Tcp)
        {
            throw new ArgumentException(
                $"TcpTransport 只接受 transport=tcp 的端点（实际为 {endpoint.Transport}）",
                nameof(endpoint));
        }

        if (!IPAddress.TryParse(endpoint.Host, out var address))
        {
            throw new SqarvoEndpointException(
                SqarvoErrorKind.EndpointInvalid,
                null,
                $"transport=tcp 的 host 必须是 IP 字面量（不做 DNS 解析），实际为 \"{endpoint.Host}\"");
        }

        if (address.AddressFamily is not (AddressFamily.InterNetwork or AddressFamily.InterNetworkV6))
        {
            throw new SqarvoEndpointException(
                SqarvoErrorKind.EndpointInvalid,
                null,
                $"不支持的地址族：{address.AddressFamily}");
        }

        if (!IPAddress.IsLoopback(address))
        {
            throw new SqarvoTransportException($"只允许连接回环地址，端点声明的是 {address}");
        }

        if (endpoint.Port is null or <= 0 or > 65535)
        {
            throw new SqarvoEndpointException(
                SqarvoErrorKind.EndpointInvalid,
                null,
                "transport=tcp 但缺少合法端口");
        }

        _address = address;
        _port = endpoint.Port.Value;
    }

    /// <inheritdoc />
    public TransportKind Kind => TransportKind.Tcp;

    /// <inheritdoc />
    public string Description => $"回环 TCP（{_address}:{_port}）";

    /// <inheritdoc />
    public Uri ChannelAddress => new UriBuilder("http", _address.ToString(), _port).Uri;

    /// <inheritdoc />
    public HttpMessageHandler CreateHandler() => new SocketsHttpHandler
    {
        UseProxy = false,
        AllowAutoRedirect = false,
        AutomaticDecompression = DecompressionMethods.None,
        ConnectTimeout = TimeSpan.FromSeconds(ConnectTimeoutSeconds),
        EnableMultipleHttp2Connections = false,
        // The backend endpoint is decided by the endpoint file; a pooled connection never becomes a
        // different endpoint, so keep connections alive for the channel's lifetime.
        PooledConnectionIdleTimeout = Timeout.InfiniteTimeSpan,
        ConnectCallback = ConnectLoopbackAsync,
    };

    /// <summary>
    /// Connects to the loopback endpoint this transport was created for, ignoring the host in the
    /// channel address (there is exactly one address it is allowed to reach).
    /// </summary>
    private async ValueTask<Stream> ConnectLoopbackAsync(
        SocketsHttpConnectionContext context,
        CancellationToken cancellationToken)
    {
        var socket = new Socket(_address.AddressFamily, SocketType.Stream, ProtocolType.Tcp)
        {
            NoDelay = true,
        };

        try
        {
            await socket.ConnectAsync(new IPEndPoint(_address, _port), cancellationToken).ConfigureAwait(false);
            return new NetworkStream(socket, ownsSocket: true);
        }
        catch (SocketException exception)
        {
            socket.Dispose();
            throw new SqarvoTransportException(
                $"无法连接后端 {Description}：{exception.SocketErrorCode}",
                exception);
        }
        catch
        {
            socket.Dispose();
            throw;
        }
    }
}