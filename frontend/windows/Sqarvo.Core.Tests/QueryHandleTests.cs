using Sqarvo.Core.Clients;
using Sqarvo.Core.Errors;
using Sqarvo.Services.V1;

namespace Sqarvo.Core.Tests;

public sealed class QueryHandleTests
{
    private static readonly ExecuteQueryResponse FirstBatch = new()
    {
        QueryId = "query_0001",
        PageToken = "page-2",
        HasMore = true,
    };

    [Fact]
    public async Task DisposeClosesTheCursorExactlyOnce()
    {
        var rpc = new FakeQueryRpc();
        var handle = new QueryHandle(rpc, FirstBatch);

        await handle.DisposeAsync();
        await handle.DisposeAsync();

        Assert.True(handle.IsClosed);
        Assert.Equal(1, rpc.CloseCalls);
        Assert.Equal("query_0001", rpc.LastClosedQueryId);
        Assert.Null(handle.CloseFailure);
    }

    [Fact]
    public async Task FetchAdvanceTheCursorAndCompleteReleasesTheCursor()
    {
        var rpc = new FakeQueryRpc();
        var handle = new QueryHandle(rpc, FirstBatch);

        rpc.NextResponse = new FetchBatchResponse
        {
            PageToken = string.Empty,
            HasMore = false,
            Complete = true,
        };

        var batch = await handle.FetchBatchAsync(maxRows: 100);

        Assert.False(batch.HasMore);
        Assert.True(batch.Complete);
        Assert.Equal("page-2", rpc.LastFetchRequest!.PageToken);
        Assert.Equal(100u, rpc.LastFetchRequest.MaxRows);
        Assert.True(handle.IsClosed);

        // The backend already released the cursor when it reports "complete".
        await handle.DisposeAsync();
        Assert.Equal(0, rpc.CloseCalls);
    }

    [Fact]
    public async Task FetchAfterDisposeIsRejectedWithoutTouchingTheBackend()
    {
        var rpc = new FakeQueryRpc();
        var handle = new QueryHandle(rpc, FirstBatch);
        await handle.DisposeAsync();

        var exception = await Assert.ThrowsAsync<SqarvoClientException>(() => handle.FetchBatchAsync());

        Assert.Equal(SqarvoErrorKind.InvalidArgument, exception.Kind);
        Assert.Equal(0, rpc.FetchCalls);
    }

    [Fact]
    public async Task CancelKeepsTheCursorOpenAndDisposeStillCloses()
    {
        var rpc = new FakeQueryRpc();
        var handle = new QueryHandle(rpc, FirstBatch);

        await handle.CancelAsync();
        await handle.DisposeAsync();

        Assert.Equal(1, rpc.CancelCalls);
        Assert.Equal(1, rpc.CloseCalls);
    }

    [Fact]
    public async Task ExplicitCloseSurfacesFailureAndDisposeRecordsIt()
    {
        var rpc = new FakeQueryRpc
        {
            CloseFailure = new SqarvoClientException(
                SqarvoErrorKind.Unavailable,
                "QueryService.Close",
                "后端不可用",
                Grpc.Core.StatusCode.Unavailable),
        };

        var handle = new QueryHandle(rpc, FirstBatch);

        var exception = await Assert.ThrowsAsync<SqarvoClientException>(() => handle.CloseAsync());
        Assert.Equal(SqarvoErrorKind.Unavailable, exception.Kind);
        Assert.False(handle.IsClosed);

        // Disposal must not throw, but must not pretend the close succeeded either.
        await handle.DisposeAsync();
        Assert.False(handle.IsClosed);
        Assert.NotNull(handle.CloseFailure);
        Assert.Equal(SqarvoErrorKind.Unavailable, handle.CloseFailure!.Kind);
        Assert.Equal(2, rpc.CloseCalls);
    }

    private sealed class FakeQueryRpc : IQueryRpc
    {
        internal int FetchCalls { get; private set; }

        internal int CancelCalls { get; private set; }

        internal int CloseCalls { get; private set; }

        internal string? LastClosedQueryId { get; private set; }

        internal FetchBatchRequest? LastFetchRequest { get; private set; }

        internal FetchBatchResponse NextResponse { get; set; } = new();

        internal SqarvoException? CloseFailure { get; set; }

        public Task<FetchBatchResponse> FetchBatchAsync(
            FetchBatchRequest request,
            CancellationToken cancellationToken)
        {
            FetchCalls++;
            LastFetchRequest = request;
            return Task.FromResult(NextResponse);
        }

        public Task CancelAsync(string queryId, CancellationToken cancellationToken)
        {
            CancelCalls++;
            return Task.CompletedTask;
        }

        public Task CloseAsync(string queryId, CancellationToken cancellationToken)
        {
            CloseCalls++;
            LastClosedQueryId = queryId;
            return CloseFailure is null ? Task.CompletedTask : Task.FromException(CloseFailure);
        }
    }
}