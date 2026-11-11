using Sqarvo.Common.V1;
using Sqarvo.Core.Protocol;
using Sqarvo.Services.V1;

namespace Sqarvo.Core.Clients;

/// <summary>
/// Thin wrapper over <c>TableDataService</c>: browse table data, apply row changes, insert rows,
/// cancel a running change batch.
/// </summary>
/// <remarks>
/// <para>
/// Paging has two mutually exclusive modes (ADR 0022): <c>offset</c> for page jumps and the opaque
/// <c>next_page_token</c> for stable sequential paging (primary-key ascending). Do not send both. The
/// Windows data grid uses offset paging with a page size of <see cref="MaxPageSize"/>, which is the same
/// choice the Linux client makes.
/// </para>
/// <para>
/// Read-only browsing is explicit state: honour <c>read_only</c> / <c>read_only_reason</c> instead of
/// enabling the editor and letting the write fail. Objects without a primary key are never editable.
/// </para>
/// <para>
/// Row changes carry typed primary-key values and column changes only; the backend binds them as
/// parameters. Keep <c>query_id</c> on <c>ApplyRowChanges</c> so a long batch can be cancelled with
/// <see cref="CancelRowChangesAsync"/> (cancellation applies at row boundaries).
/// </para>
/// <para>
/// <c>total_rows</c> is three-state: read <c>total_kind</c> (<c>Unknown</c> / <c>Estimate</c> /
/// <c>Exact</c>) and <c>total_skipped</c> before formatting a count, never infer it.
/// </para>
/// </remarks>
public sealed class TableDataClient
{
    /// <summary>Page size the backend uses when the request asks for 0.</summary>
    public const int DefaultPageSize = 100;

    /// <summary>Largest page the backend accepts; larger requests are clamped, not rejected.</summary>
    public const int MaxPageSize = 500;

    private readonly TableDataService.TableDataServiceClient _client;
    private readonly SqarvoChannelOptions _options;

    internal TableDataClient(TableDataService.TableDataServiceClient client, SqarvoChannelOptions options)
    {
        _client = client;
        _options = options;
    }

    /// <summary>Browses one page of table data.</summary>
    /// <param name="request">Full request message (paging mode, sort, filter, total).</param>
    /// <param name="cancellationToken">Cancels the call.</param>
    /// <returns>Columns, rows and paging metadata.</returns>
    public async Task<BrowseTableResponse> BrowseTableAsync(
        BrowseTableRequest request,
        CancellationToken cancellationToken = default)
    {
        ArgumentNullException.ThrowIfNull(request);

        var call = _client.BrowseTableAsync(
            request,
            deadline: RpcCall.DeadlineAfter(_options.OperationTimeout),
            cancellationToken: cancellationToken);

        return await RpcCall.AwaitAsync(call, "TableDataService.BrowseTable", cancellationToken)
            .ConfigureAwait(false);
    }

    /// <summary>Convenience overload for offset paging.</summary>
    /// <param name="table">Table or view reference.</param>
    /// <param name="pageSize">Rows per page; clamped to <see cref="MaxPageSize"/> by the backend.</param>
    /// <param name="offset">First row offset. Offset paging is not stable across concurrent writes.</param>
    /// <param name="pageToken">Cursor for sequential paging; must be empty when <paramref name="offset"/> &gt; 0.</param>
    /// <param name="includeTotal">Whether to request a row count; read <c>total_kind</c> in the response.</param>
    /// <param name="sort">Sort keys, or <see langword="null"/> for primary-key order.</param>
    /// <param name="filter">Structured filter, or <see langword="null"/> for none.</param>
    /// <param name="totalTimeout">Timeout for the exact count; <see langword="null"/> uses the server default.</param>
    /// <param name="cancellationToken">Cancels the call.</param>
    /// <returns>One page of rows.</returns>
    public Task<BrowseTableResponse> BrowseTableAsync(
        ObjectRef table,
        int pageSize = DefaultPageSize,
        ulong offset = 0,
        string? pageToken = null,
        bool includeTotal = false,
        IReadOnlyList<SortKey>? sort = null,
        Filter? filter = null,
        TimeSpan? totalTimeout = null,
        CancellationToken cancellationToken = default)
    {
        var request = new BrowseTableRequest
        {
            Object = table,
            PageSize = pageSize <= 0 ? 0u : (uint)Math.Min(pageSize, MaxPageSize),
            Offset = offset,
            PageToken = pageToken ?? string.Empty,
            IncludeTotal = includeTotal,
            TimeoutMillis = totalTimeout is null
                ? 0u
                : (uint)Math.Clamp(totalTimeout.Value.TotalMilliseconds, 0, uint.MaxValue),
        };

        if (sort is not null)
        {
            request.Sort.AddRange(sort);
        }

        if (filter is not null)
        {
            request.Filter = filter;
        }

        return BrowseTableAsync(request, cancellationToken);
    }

    /// <summary>Applies row updates and deletes in one batch.</summary>
    /// <param name="request">Batch description; set <c>query_id</c> to make it cancellable.</param>
    /// <param name="cancellationToken">Cancels the RPC.</param>
    /// <returns>Per-row outcomes plus the net affected rows (0 when the batch was cancelled).</returns>
    public async Task<ApplyRowChangesResponse> ApplyRowChangesAsync(
        ApplyRowChangesRequest request,
        CancellationToken cancellationToken = default)
    {
        ArgumentNullException.ThrowIfNull(request);

        var call = _client.ApplyRowChangesAsync(
            request,
            deadline: RpcCall.DeadlineAfter(_options.OperationTimeout),
            cancellationToken: cancellationToken);

        return await RpcCall.AwaitAsync(call, "TableDataService.ApplyRowChanges", cancellationToken)
            .ConfigureAwait(false);
    }

    /// <summary>Inserts rows in one batch.</summary>
    /// <param name="request">Rows to insert plus transaction and risk acknowledgement.</param>
    /// <param name="cancellationToken">Cancels the RPC.</param>
    /// <returns>
    /// Per-row results. Judge success by <c>rolled_back</c> and <c>total_rows_affected</c>, not by the
    /// per-row outcome (a batch rolls back as a whole when any row fails).
    /// </returns>
    public async Task<InsertRowsResponse> InsertRowsAsync(
        InsertRowsRequest request,
        CancellationToken cancellationToken = default)
    {
        ArgumentNullException.ThrowIfNull(request);

        var call = _client.InsertRowsAsync(
            request,
            deadline: RpcCall.DeadlineAfter(_options.OperationTimeout),
            cancellationToken: cancellationToken);

        return await RpcCall.AwaitAsync(call, "TableDataService.InsertRows", cancellationToken)
            .ConfigureAwait(false);
    }

    /// <summary>Cancels a running row-change batch at the next row boundary.</summary>
    /// <param name="queryId">Client-generated <c>query_id</c> of the batch.</param>
    /// <param name="cancellationToken">Cancels the RPC.</param>
    /// <remarks>Cancelling an unknown or finished batch is success (idempotent).</remarks>
    public async Task CancelRowChangesAsync(string queryId, CancellationToken cancellationToken = default)
    {
        var call = _client.CancelRowChangesAsync(
            new CancelRowChangesRequest { QueryId = queryId },
            deadline: RpcCall.DeadlineAfter(_options.OperationTimeout),
            cancellationToken: cancellationToken);

        await RpcCall.AwaitAsync(call, "TableDataService.CancelRowChanges", cancellationToken)
            .ConfigureAwait(false);
    }
}