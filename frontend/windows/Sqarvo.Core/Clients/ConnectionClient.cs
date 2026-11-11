using Sqarvo.Core.Protocol;
using Sqarvo.Services.V1;

namespace Sqarvo.Core.Clients;

/// <summary>
/// Thin wrapper over <c>ConnectionService</c>: test, connect, disconnect, list sessions.
/// </summary>
/// <remarks>
/// <para>
/// Passwords never travel through this API: <see cref="ConnectionParams"/> only carries a
/// <c>credential_ref</c> pointing at the OS credential store, which the backend resolves. See
/// <see cref="CredentialClient"/> for storing the secret in the first place.
/// </para>
/// <para>
/// The backend validates every parameter again; client-side validation is only for immediate feedback.
/// </para>
/// </remarks>
public sealed class ConnectionClient
{
    private readonly ConnectionService.ConnectionServiceClient _client;
    private readonly SqarvoChannelOptions _options;

    internal ConnectionClient(ConnectionService.ConnectionServiceClient client, SqarvoChannelOptions options)
    {
        _client = client;
        _options = options;
    }

    /// <summary>
    /// Tests whether a connection would be reachable, without creating a session.
    /// </summary>
    /// <param name="parameters">Connection parameters (no password field exists by design).</param>
    /// <param name="cancellationToken">Cancels the call.</param>
    /// <returns>Reachability, measured latency and the server version when it could be read.</returns>
    public async Task<TestConnectionResponse> TestConnectionAsync(
        ConnectionParams parameters,
        CancellationToken cancellationToken = default)
    {
        var call = _client.TestConnectionAsync(
            new TestConnectionRequest { Params = parameters },
            deadline: RpcCall.DeadlineAfter(_options.OperationTimeout),
            cancellationToken: cancellationToken);

        return await RpcCall.AwaitAsync(call, "ConnectionService.TestConnection", cancellationToken)
            .ConfigureAwait(false);
    }

    /// <summary>Opens a database session.</summary>
    /// <param name="parameters">Connection parameters.</param>
    /// <param name="displayName">User-visible session name.</param>
    /// <param name="cancellationToken">Cancels the call.</param>
    /// <returns>Session and connection identifiers plus the negotiated capabilities.</returns>
    public async Task<ConnectResponse> ConnectAsync(
        ConnectionParams parameters,
        string displayName,
        CancellationToken cancellationToken = default)
    {
        var call = _client.ConnectAsync(
            new ConnectRequest { Params = parameters, DisplayName = displayName },
            deadline: RpcCall.DeadlineAfter(_options.OperationTimeout),
            cancellationToken: cancellationToken);

        return await RpcCall.AwaitAsync(call, "ConnectionService.Connect", cancellationToken)
            .ConfigureAwait(false);
    }

    /// <summary>Closes a database session and every resource owned by it.</summary>
    /// <param name="connectionId">Identifier returned by <see cref="ConnectAsync"/>.</param>
    /// <param name="cancellationToken">Cancels the call.</param>
    public async Task DisconnectAsync(string connectionId, CancellationToken cancellationToken = default)
    {
        var call = _client.DisconnectAsync(
            new DisconnectRequest { ConnectionId = connectionId },
            deadline: RpcCall.DeadlineAfter(_options.OperationTimeout),
            cancellationToken: cancellationToken);

        await RpcCall.AwaitAsync(call, "ConnectionService.Disconnect", cancellationToken).ConfigureAwait(false);
    }

    /// <summary>Lists the open sessions of this backend.</summary>
    /// <param name="cancellationToken">Cancels the call.</param>
    /// <returns>Session summaries; empty when nothing is connected.</returns>
    public async Task<IReadOnlyList<SessionSummary>> ListSessionsAsync(CancellationToken cancellationToken = default)
    {
        var call = _client.ListSessionsAsync(
            new Sqarvo.Common.V1.Empty(),
            deadline: RpcCall.DeadlineAfter(_options.DefaultTimeout),
            cancellationToken: cancellationToken);

        var response = await RpcCall.AwaitAsync(call, "ConnectionService.ListSessions", cancellationToken)
            .ConfigureAwait(false);

        return response.Sessions.ToArray();
    }
}