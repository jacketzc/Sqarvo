using Sqarvo.Common.V1;
using Sqarvo.Core.Errors;
using Sqarvo.Core.Model;
using Sqarvo.Services.V1;

namespace Sqarvo.Core.Clients;

/// <summary>
/// An open query cursor. Owns the <c>Close</c> obligation of <c>QueryService</c>.
/// </summary>
/// <remarks>
/// <para>
/// The backend allows at most 8 concurrent query cursors per process
/// (<see cref="QueryClient.MaxConcurrentQueries"/>). A cursor that is never closed keeps its slot until
/// the session ends, so after eight forgotten cursors every further <c>Execute</c> fails with
/// RESOURCE_EXHAUSTED. Making the handle <see cref="IAsyncDisposable"/> is the enforcement mechanism:
/// <c>await using</c> guarantees a <c>Close</c>, whether the caller read the result to the end, stopped
/// early, or threw.
/// </para>
/// <para>
/// <see cref="DisposeAsync"/> never throws (disposal runs on paths that already have an exception in
/// flight). If <c>Close</c> fails, the failure is preserved in <see cref="CloseFailure"/> and the slot
/// may still be held; callers that need to report it should call <see cref="CloseAsync"/> explicitly.
/// </para>
/// <para>
/// The first batch of rows is already returned by <c>Execute</c>; <see cref="InitialRows"/> exposes it
/// so the UI can render immediately while the remaining batches are fetched on demand.
/// </para>
/// </remarks>
public sealed class QueryHandle : IAsyncDisposable
{
    private static readonly TimeSpan DisposeCloseTimeout = TimeSpan.FromSeconds(2);

    private readonly IQueryRpc _rpc;
    private readonly int _statementIndex;
    private string _pageToken;
    private int _closed;

    internal QueryHandle(IQueryRpc rpc, ExecuteQueryResponse first, int statementIndex = 0)
    {
        _rpc = rpc;
        QueryId = first.QueryId;
        Statements = first.Statements.ToArray();
        InitialRows = first.Rows.ToArray();
        _pageToken = first.PageToken;
        HasMore = first.HasMore;
        Warnings = first.Warnings.ToArray();
        _statementIndex = statementIndex;
    }

    /// <summary>Query id assigned by the backend; needed for <c>Cancel</c>/<c>Close</c>.</summary>
    public string QueryId { get; }

    /// <summary>Statement summaries (result set shape, affected row counts) for this execution.</summary>
    public IReadOnlyList<StatementSummary> Statements { get; }

    /// <summary>Rows returned by the first batch.</summary>
    public IReadOnlyList<Row> InitialRows { get; }

    /// <summary>Non-fatal warnings reported by the backend; display them, do not drop them.</summary>
    public IReadOnlyList<Warning> Warnings { get; }

    /// <summary>Whether more rows are available.</summary>
    public bool HasMore { get; private set; }

    /// <summary>Cursor for the next batch; empty when <see cref="HasMore"/> is false.</summary>
    public string PageToken => _pageToken;

    /// <summary>Whether <c>Close</c> has succeeded (or the cursor was already released).</summary>
    public bool IsClosed => Volatile.Read(ref _closed) != 0;

    /// <summary>
    /// Failure observed while closing from <see cref="DisposeAsync"/>, or <see langword="null"/>. Set only
    /// when the implicit close failed; the exception itself is not thrown from disposal.
    /// </summary>
    public SqarvoException? CloseFailure { get; private set; }

    /// <summary>Fetches the next batch of rows.</summary>
    /// <param name="maxRows">Maximum rows for this batch; 0 means the server default.</param>
    /// <param name="cancellationToken">Cancels the call.</param>
    /// <returns>The batch; also updates <see cref="HasMore"/> and <see cref="PageToken"/>.</returns>
    /// <exception cref="SqarvoClientException">The cursor was closed or the backend rejected the fetch.</exception>
    public async Task<QueryBatch> FetchBatchAsync(uint maxRows = 0, CancellationToken cancellationToken = default)
    {
        if (IsClosed)
        {
            throw new SqarvoClientException(
                SqarvoErrorKind.InvalidArgument,
                "QueryService.FetchBatch",
                "查询句柄已关闭，不能再取下一批（先 Execute 得到新的 query_id）",
                Grpc.Core.StatusCode.FailedPrecondition);
        }

        var request = new FetchBatchRequest
        {
            QueryId = QueryId,
            PageToken = _pageToken,
            MaxRows = maxRows,
            StatementIndex = (uint)_statementIndex,
        };

        var response = await _rpc.FetchBatchAsync(request, cancellationToken).ConfigureAwait(false);

        _pageToken = response.PageToken;
        HasMore = response.HasMore;
        if (response.Complete)
        {
            // The backend already released the cursor when it reports completion.
            Interlocked.Exchange(ref _closed, 1);
        }

        return new QueryBatch(response.Rows.ToArray(), response.PageToken, response.HasMore, response.Complete);
    }

    /// <summary>
    /// Cancels the running statement. The cursor stays open: the contract keeps cancellation and cursor
    /// release separate, so the caller still has to <see cref="CloseAsync"/> (or dispose).
    /// </summary>
    /// <param name="cancellationToken">Cancels the RPC itself; use a short timeout - the point of the call
    /// is to stop waiting.</param>
    public Task CancelAsync(CancellationToken cancellationToken = default) =>
        IsClosed ? Task.CompletedTask : _rpc.CancelAsync(QueryId, cancellationToken);

    /// <summary>
    /// Releases the cursor. Idempotent; safe to call after the result set completed on its own.
    /// </summary>
    /// <param name="cancellationToken">Cancels the RPC.</param>
    /// <exception cref="SqarvoException">The backend could not release the cursor.</exception>
    public async Task CloseAsync(CancellationToken cancellationToken = default)
    {
        if (IsClosed)
        {
            return;
        }

        await _rpc.CloseAsync(QueryId, cancellationToken).ConfigureAwait(false);
        Interlocked.Exchange(ref _closed, 1);
    }

    /// <summary>
    /// Closes the cursor if the caller did not. Never throws; see <see cref="CloseFailure"/>.
    /// </summary>
    /// <returns>A completed task once the close attempt has finished.</returns>
    public async ValueTask DisposeAsync()
    {
        if (IsClosed)
        {
            return;
        }

        using var timeout = new CancellationTokenSource(DisposeCloseTimeout);
        try
        {
            await CloseAsync(timeout.Token).ConfigureAwait(false);
        }
        catch (SqarvoException exception)
        {
            CloseFailure = exception;
        }
        catch (OperationCanceledException exception)
        {
            CloseFailure = new SqarvoClientException(
                SqarvoErrorKind.DeadlineExceeded,
                "QueryService.Close",
                $"关闭查询 {QueryId} 超时（{DisposeCloseTimeout.TotalSeconds} 秒）；并发查询名额可能仍被占用",
                Grpc.Core.StatusCode.DeadlineExceeded,
                innerException: exception);
        }
    }
}