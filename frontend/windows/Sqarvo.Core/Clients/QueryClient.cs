using Sqarvo.Common.V1;
using Sqarvo.Core.Model;
using Sqarvo.Core.Protocol;
using Sqarvo.Services.V1;

namespace Sqarvo.Core.Clients;

/// <summary>
/// Thin wrapper over <c>QueryService</c>: execute SQL, fetch batches, cancel, close.
/// </summary>
/// <remarks>
/// <para>
/// Result sets are never returned whole. <see cref="ExecuteQueryAsync(ExecuteQueryRequest, CancellationToken)"/>
/// yields the first batch plus a cursor; further batches come from <see cref="QueryHandle.FetchBatchAsync"/>.
/// </para>
/// <para>
/// Every execution occupies one of <see cref="MaxConcurrentQueries"/> slots until it is closed. Always
/// use the returned handle with <c>await using</c> so the slot is released even on the error path.
/// </para>
/// <para>
/// Cancellation semantics follow the contract: <c>QueryService.Cancel</c> releases a cursor, it cannot
/// interrupt an <c>Execute</c> that is still in flight (a unary call has no cancellation channel back
/// to the server). Long statements must therefore be bounded with <c>timeout_millis</c> on the request,
/// and the client applies its own deadline on top.
/// </para>
/// </remarks>
public sealed class QueryClient : IQueryRpc
{
    /// <summary>
    /// Maximum number of concurrent query cursors the backend accepts per process. Exceeding it fails
    /// with RESOURCE_EXHAUSTED; forgetting to close a cursor keeps its slot occupied.
    /// </summary>
    public const int MaxConcurrentQueries = 8;

    private readonly QueryService.QueryServiceClient _client;
    private readonly SqarvoChannelOptions _options;

    internal QueryClient(QueryService.QueryServiceClient client, SqarvoChannelOptions options)
    {
        _client = client;
        _options = options;
    }

    /// <summary>Executes SQL and returns a handle owning the result cursor.</summary>
    /// <param name="request">Full request message (session, SQL, batch size, auto-commit, server timeout).</param>
    /// <param name="cancellationToken">Cancels the RPC.</param>
    /// <returns>A handle that must be disposed; disposing closes the cursor.</returns>
    public async Task<QueryHandle> ExecuteQueryAsync(
        ExecuteQueryRequest request,
        CancellationToken cancellationToken = default)
    {
        ArgumentNullException.ThrowIfNull(request);

        var call = _client.ExecuteAsync(
            request,
            deadline: RpcCall.DeadlineAfter(_options.OperationTimeout),
            cancellationToken: cancellationToken);

        var response = await RpcCall.AwaitAsync(call, "QueryService.Execute", cancellationToken)
            .ConfigureAwait(false);

        return new QueryHandle(this, response);
    }

    /// <summary>Convenience overload for a single statement.</summary>
    /// <param name="sessionId">Session to execute in.</param>
    /// <param name="sql">SQL text.</param>
    /// <param name="maxRowsPerBatch">Rows per batch; 0 means the server default, the server caps the value.</param>
    /// <param name="autoCommit">Whether the statement commits automatically.</param>
    /// <param name="executionTimeout">
    /// Server-side execution timeout. This is the only way to bound a statement that is already running;
    /// <see langword="null"/> leaves the policy to the backend.
    /// </param>
    /// <param name="cancellationToken">Cancels the RPC.</param>
    /// <returns>A handle that must be disposed; disposing closes the cursor.</returns>
    public Task<QueryHandle> ExecuteQueryAsync(
        string sessionId,
        string sql,
        int maxRowsPerBatch = 0,
        bool autoCommit = true,
        TimeSpan? executionTimeout = null,
        CancellationToken cancellationToken = default)
    {
        ArgumentException.ThrowIfNullOrEmpty(sessionId);
        ArgumentNullException.ThrowIfNull(sql);

        var request = new ExecuteQueryRequest
        {
            SessionId = sessionId,
            Sql = sql,
            MaxRowsPerBatch = maxRowsPerBatch <= 0 ? 0u : (uint)maxRowsPerBatch,
            AutoCommit = autoCommit,
            TimeoutMillis = executionTimeout is null
                ? 0u
                : (uint)Math.Clamp(executionTimeout.Value.TotalMilliseconds, 0, uint.MaxValue),
        };

        return ExecuteQueryAsync(request, cancellationToken);
    }

    /// <summary>Fetches the next batch of an existing cursor.</summary>
    /// <param name="queryId">Query id returned by <c>Execute</c>.</param>
    /// <param name="pageToken">Cursor from the previous batch; empty for the first fetch.</param>
    /// <param name="maxRows">Rows to fetch; 0 means the server default.</param>
    /// <param name="statementIndex">Statement index inside a multi-statement execution (v0.1: always 0).</param>
    /// <param name="cancellationToken">Cancels the RPC.</param>
    /// <returns>The batch.</returns>
    public async Task<QueryBatch> FetchBatchAsync(
        string queryId,
        string? pageToken,
        int maxRows = 0,
        int statementIndex = 0,
        CancellationToken cancellationToken = default)
    {
        var request = new FetchBatchRequest
        {
            QueryId = queryId,
            PageToken = pageToken ?? string.Empty,
            MaxRows = maxRows <= 0 ? 0u : (uint)maxRows,
            StatementIndex = (uint)statementIndex,
        };

        var call = _client.FetchBatchAsync(
            request,
            deadline: RpcCall.DeadlineAfter(_options.OperationTimeout),
            cancellationToken: cancellationToken);

        var response = await RpcCall.AwaitAsync(call, "QueryService.FetchBatch", cancellationToken)
            .ConfigureAwait(false);

        return new QueryBatch(response.Rows.ToArray(), response.PageToken, response.HasMore, response.Complete);
    }

    /// <summary>Cancels a running statement. The cursor stays open until it is closed.</summary>
    /// <param name="queryId">Query id returned by <c>Execute</c>.</param>
    /// <param name="cancellationToken">Cancels the RPC.</param>
    public async Task CancelQueryAsync(string queryId, CancellationToken cancellationToken = default)
    {
        var call = _client.CancelAsync(
            new CancelQueryRequest { QueryId = queryId },
            deadline: RpcCall.DeadlineAfter(_options.OperationTimeout),
            cancellationToken: cancellationToken);

        await RpcCall.AwaitAsync(call, "QueryService.Cancel", cancellationToken).ConfigureAwait(false);
    }

    /// <summary>Releases the cursor and its concurrency slot.</summary>
    /// <param name="queryId">Query id returned by <c>Execute</c>.</param>
    /// <param name="cancellationToken">Cancels the RPC.</param>
    public async Task CloseQueryAsync(string queryId, CancellationToken cancellationToken = default)
    {
        var call = _client.CloseAsync(
            new CloseQueryRequest { QueryId = queryId },
            deadline: RpcCall.DeadlineAfter(_options.OperationTimeout),
            cancellationToken: cancellationToken);

        await RpcCall.AwaitAsync(call, "QueryService.Close", cancellationToken).ConfigureAwait(false);
    }

    async Task<FetchBatchResponse> IQueryRpc.FetchBatchAsync(
        FetchBatchRequest request,
        CancellationToken cancellationToken)
    {
        var call = _client.FetchBatchAsync(
            request,
            deadline: RpcCall.DeadlineAfter(_options.OperationTimeout),
            cancellationToken: cancellationToken);

        return await RpcCall.AwaitAsync(call, "QueryService.FetchBatch", cancellationToken).ConfigureAwait(false);
    }

    Task IQueryRpc.CancelAsync(string queryId, CancellationToken cancellationToken) =>
        CancelQueryAsync(queryId, cancellationToken);

    Task IQueryRpc.CloseAsync(string queryId, CancellationToken cancellationToken) =>
        CloseQueryAsync(queryId, cancellationToken);
}