using Grpc.Net.Client;
using Sqarvo.Core.Ipc;
using Sqarvo.Core.Protocol;
using Sqarvo.Services.V1;

namespace Sqarvo.Core.Clients;

/// <summary>
/// A connected backend: one gRPC channel plus the seven service wrappers.
/// </summary>
/// <remarks>
/// <para>
/// This is the <b>only</b> place in the repository that constructs generated gRPC stubs. Everything a UI
/// needs is reachable through the typed clients below, so the WinUI project never references
/// <c>Sqarvo.Services.V1.*ServiceClient</c>, never touches a channel, and cannot forget the token or the
/// deadline.
/// </para>
/// <para>
/// Construction performs no I/O: <see cref="GrpcChannel"/> connects lazily on the first call. A wrong
/// token therefore surfaces as <see cref="Errors.SqarvoAuthenticationException"/> on the first RPC, not
/// here.
/// </para>
/// <para>
/// Instances are cheap to share (the channel multiplexes streams) and must be disposed when the endpoint
/// is abandoned; disposing it does not stop the backend process - that is
/// <c>BackendSupervisor.ShutdownAsync</c>'s job.
/// </para>
/// </remarks>
public sealed class BackendConnection : IDisposable
{
    private readonly GrpcChannel _channel;
    private bool _disposed;

    private BackendConnection(
        EndpointInfo endpoint,
        GrpcChannel channel,
        SqarvoChannelOptions options)
    {
        Endpoint = endpoint;
        _channel = channel;

        var invoker = new TokenCallInvoker(channel.CreateCallInvoker(), endpoint.Token, options.DefaultTimeout);

        System = new SystemClient(new SystemService.SystemServiceClient(invoker), options);
        Connections = new ConnectionClient(new ConnectionService.ConnectionServiceClient(invoker), options);
        Credentials = new CredentialClient(new CredentialService.CredentialServiceClient(invoker), options);
        Metadata = new MetadataClient(new MetadataService.MetadataServiceClient(invoker), options);
        Queries = new QueryClient(new QueryService.QueryServiceClient(invoker), options);
        Transactions = new TransactionClient(new TransactionService.TransactionServiceClient(invoker), options);
        TableData = new TableDataClient(new TableDataService.TableDataServiceClient(invoker), options);
    }

    /// <summary>Endpoint (and therefore token) this connection uses.</summary>
    public EndpointInfo Endpoint { get; }

    /// <summary>System service: version, health, driver list.</summary>
    public SystemClient System { get; }

    /// <summary>Connection service: test, connect, disconnect, list sessions.</summary>
    public ConnectionClient Connections { get; }

    /// <summary>Credential service: store and delete database passwords in the OS credential store.</summary>
    public CredentialClient Credentials { get; }

    /// <summary>Metadata service: object tree, columns, DDL, primary keys.</summary>
    public MetadataClient Metadata { get; }

    /// <summary>Query service: execute, fetch batches, cancel, close.</summary>
    public QueryClient Queries { get; }

    /// <summary>Transaction service: begin, commit, rollback.</summary>
    public TransactionClient Transactions { get; }

    /// <summary>Table data service: browse, apply row changes, insert rows, cancel batches.</summary>
    public TableDataClient TableData { get; }

    /// <summary>Creates a connection for an endpoint. No I/O happens until the first RPC.</summary>
    /// <param name="endpoint">Validated endpoint description.</param>
    /// <param name="options">Timeouts and limits; <see cref="SqarvoChannelOptions.Default"/> when omitted.</param>
    /// <returns>A connection whose clients are ready to use.</returns>
    /// <exception cref="Errors.SqarvoException">
    /// The transport cannot be created for this endpoint (unsupported kind, non-loopback host, ...).
    /// </exception>
    public static BackendConnection Create(EndpointInfo endpoint, SqarvoChannelOptions? options = null)
    {
        ArgumentNullException.ThrowIfNull(endpoint);
        var effectiveOptions = options ?? SqarvoChannelOptions.Default;

        // Fail fast for callers that bypass the supervisor: an incompatible backend cannot be talked to.
        ProtocolVersion.EnsureCompatible(endpoint.ProtocolVersion);

        // Single transport selection point; the handler it returns enforces loopback-only, no proxy.
        var transport = TransportFactory.Create(endpoint);

        var channel = GrpcChannel.ForAddress(
            transport.ChannelAddress,
            new GrpcChannelOptions
            {
                HttpHandler = transport.CreateHandler(),
                DisposeHttpClient = true,
                MaxReceiveMessageSize = effectiveOptions.MaxReceiveMessageSizeBytes,
                MaxSendMessageSize = effectiveOptions.MaxReceiveMessageSizeBytes,
                // Caller cancellation should look like cancellation to C# callers; the wrapper still maps
                // an expired deadline to DeadlineExceeded.
                ThrowOperationCanceledOnCancellation = true,
            });

        return new BackendConnection(endpoint, channel, effectiveOptions);
    }

    /// <summary>Disposes the channel and its HTTP handler. Does not touch the backend process.</summary>
    public void Dispose()
    {
        if (_disposed)
        {
            return;
        }

        _disposed = true;
        _channel.Dispose();
    }
}