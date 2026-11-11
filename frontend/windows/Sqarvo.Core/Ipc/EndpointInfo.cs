using Sqarvo.Core.Errors;

namespace Sqarvo.Core.Ipc;

/// <summary>
/// Validated contents of the backend's endpoint description file
/// (<c>backend-endpoint.json</c>), i.e. how to reach the backend and with which session token.
/// </summary>
/// <remarks>
/// <para>
/// The token is the only access control on the local IPC channel. It is therefore treated as a
/// secret: <see cref="ToString"/> and <see cref="RedactedSummary"/> never contain it, and no
/// exception message is allowed to echo it. Instances are immutable.
/// </para>
/// <para>
/// The constructor validates the invariants, so any instance that exists is well formed. Transport
/// specific fields are only required for the matching transport kind.
/// </para>
/// </remarks>
public sealed class EndpointInfo
{
    /// <summary>Expected token length: 32 random bytes rendered as hexadecimal.</summary>
    public const int TokenHexLength = 64;

    /// <summary>Initialises and validates an endpoint description.</summary>
    /// <param name="transport">Transport kind declared by the backend.</param>
    /// <param name="token">Session token (64 hexadecimal characters).</param>
    /// <param name="pid">Backend process id that wrote the file.</param>
    /// <param name="protocolVersion">Protocol version declared by the backend, for example <c>v1</c>.</param>
    /// <param name="serviceVersion">Backend service version, for example <c>0.1.0</c>.</param>
    /// <param name="socketPath">Unix socket path; required for <see cref="TransportKind.Unix"/>.</param>
    /// <param name="host">TCP host; required for <see cref="TransportKind.Tcp"/>.</param>
    /// <param name="port">TCP port; required for <see cref="TransportKind.Tcp"/>.</param>
    /// <param name="createdAt">Write timestamp, diagnostic only.</param>
    /// <exception cref="SqarvoEndpointException">The combination of fields is invalid.</exception>
    public EndpointInfo(
        TransportKind transport,
        string token,
        int pid,
        string protocolVersion,
        string serviceVersion,
        string? socketPath = null,
        string? host = null,
        int? port = null,
        DateTimeOffset? createdAt = null)
    {
        if (token is null
            || token.Length != TokenHexLength
            || !token.All(static character => character is (>= '0' and <= '9') or (>= 'a' and <= 'f') or (>= 'A' and <= 'F')))
        {
            throw new SqarvoEndpointException(
                SqarvoErrorKind.EndpointInvalid,
                null,
                $"token 格式非法：期望 {TokenHexLength} 位十六进制，实际 {(token is null ? 0 : token.Length)} 个字符");
        }

        if (pid <= 0)
        {
            throw new SqarvoEndpointException(SqarvoErrorKind.EndpointInvalid, null, "pid 必须为正整数");
        }

        if (string.IsNullOrWhiteSpace(protocolVersion))
        {
            throw new SqarvoEndpointException(SqarvoErrorKind.EndpointInvalid, null, "缺少 protocol_version");
        }

        if (string.IsNullOrWhiteSpace(serviceVersion))
        {
            throw new SqarvoEndpointException(SqarvoErrorKind.EndpointInvalid, null, "缺少 service_version");
        }

        switch (transport)
        {
            case TransportKind.Unix:
                if (string.IsNullOrWhiteSpace(socketPath))
                {
                    throw new SqarvoEndpointException(
                        SqarvoErrorKind.EndpointInvalid,
                        null,
                        "transport=unix 但缺少 socket_path");
                }

                break;

            case TransportKind.Tcp:
                if (string.IsNullOrWhiteSpace(host))
                {
                    throw new SqarvoEndpointException(SqarvoErrorKind.EndpointInvalid, null, "transport=tcp 但缺少 host");
                }

                if (port is null or <= 0 or > 65535)
                {
                    throw new SqarvoEndpointException(
                        SqarvoErrorKind.EndpointInvalid,
                        null,
                        "transport=tcp 但缺少 port 或端口越界（1-65535）");
                }

                break;

            default:
                throw new SqarvoEndpointException(
                    SqarvoErrorKind.EndpointInvalid,
                    null,
                    $"未知的 transport 取值：{transport}");
        }

        Transport = transport;
        Token = token;
        Pid = pid;
        ProtocolVersion = protocolVersion;
        ServiceVersion = serviceVersion;
        SocketPath = socketPath;
        Host = host;
        Port = port;
        CreatedAt = createdAt;
    }

    /// <summary>Transport kind declared by the backend.</summary>
    public TransportKind Transport { get; }

    /// <summary>
    /// Session token sent as the <c>x-sqarvo-token</c> metadata on every request. Sensitive: never
    /// write it to logs, UI, crash reports or configuration files.
    /// </summary>
    public string Token { get; }

    /// <summary>Length of <see cref="Token"/>, safe to log for diagnostics.</summary>
    public int TokenLength => Token.Length;

    /// <summary>Backend process id that wrote the endpoint file.</summary>
    public int Pid { get; }

    /// <summary>Protocol version declared by the backend, for example <c>v1</c>.</summary>
    public string ProtocolVersion { get; }

    /// <summary>Backend service version, for example <c>0.1.0</c>.</summary>
    public string ServiceVersion { get; }

    /// <summary>Unix socket path; <see langword="null"/> for TCP endpoints.</summary>
    public string? SocketPath { get; }

    /// <summary>TCP host; <see langword="null"/> for Unix endpoints.</summary>
    public string? Host { get; }

    /// <summary>TCP port; <see langword="null"/> for Unix endpoints.</summary>
    public int? Port { get; }

    /// <summary>Write timestamp reported by the backend, diagnostic only.</summary>
    public DateTimeOffset? CreatedAt { get; }

    /// <summary>
    /// Returns a copy carrying a different token. Used when re-reading the endpoint after a backend
    /// restart, and by tests that need to prove the token is actually enforced.
    /// </summary>
    /// <param name="token">New session token (64 hexadecimal characters).</param>
    /// <returns>A new, validated instance.</returns>
    public EndpointInfo WithToken(string token) =>
        new(Transport, token, Pid, ProtocolVersion, ServiceVersion, SocketPath, Host, Port, CreatedAt);

    /// <summary>
    /// Human readable transport description that is safe to display (contains no token), for
    /// example <c>回环 TCP（127.0.0.1:51234）</c>.
    /// </summary>
    public string TransportDescription => Transport switch
    {
        TransportKind.Tcp => $"回环 TCP（{Host}:{Port}）",
        TransportKind.Unix => $"Unix 域套接字（{SocketPath}）",
        _ => Transport.ToString(),
    };

    /// <summary>Diagnostic summary with the token replaced by its length.</summary>
    public string RedactedSummary =>
        $"transport={Transport} pid={Pid} protocol={ProtocolVersion} service={ServiceVersion} token=<redacted>({TokenLength} 字符)";

    /// <summary>Returns <see cref="RedactedSummary"/>; never includes the token.</summary>
    public override string ToString() => RedactedSummary;
}