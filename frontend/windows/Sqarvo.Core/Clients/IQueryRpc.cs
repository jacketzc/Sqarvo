using Sqarvo.Services.V1;

namespace Sqarvo.Core.Clients;

/// <summary>
/// The slice of <c>QueryService</c> that a <see cref="QueryHandle"/> needs. Exists so the handle's
/// lifecycle (fetch / cancel / close-on-dispose) can be tested without a network or a backend.
/// </summary>
internal interface IQueryRpc
{
    /// <summary>Fetches one batch of rows for a query.</summary>
    Task<FetchBatchResponse> FetchBatchAsync(FetchBatchRequest request, CancellationToken cancellationToken);

    /// <summary>Cancels a running query without releasing its cursor.</summary>
    Task CancelAsync(string queryId, CancellationToken cancellationToken);

    /// <summary>Releases the query's server-side cursor and its concurrency slot.</summary>
    Task CloseAsync(string queryId, CancellationToken cancellationToken);
}