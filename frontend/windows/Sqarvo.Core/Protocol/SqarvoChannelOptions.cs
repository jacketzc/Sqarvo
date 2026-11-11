namespace Sqarvo.Core.Protocol;

/// <summary>
/// Timeouts and limits applied by the local gRPC channel.
/// </summary>
/// <remarks>
/// All timeouts are deliberately short: the peer is a local companion process, so a request that
/// takes longer indicates a hung backend rather than a slow network. Values can be overridden in
/// tests; production code uses <see cref="Default"/>.
/// </remarks>
public sealed class SqarvoChannelOptions
{
    /// <summary>Shared default instance: 5 s per call, 10 s for bulk operations, 16 MiB receive limit.</summary>
    public static SqarvoChannelOptions Default { get; } = new();

    /// <summary>Deadline applied to ordinary calls (health, version, drivers, transactions).</summary>
    public TimeSpan DefaultTimeout { get; init; } = TimeSpan.FromSeconds(5);

    /// <summary>
    /// Deadline applied to calls that move data or touch a database: execute/fetch/close/cancel,
    /// metadata listing, DDL, table browsing and row changes. The backend applies its own server-side
    /// timeouts on top.
    /// </summary>
    public TimeSpan OperationTimeout { get; init; } = TimeSpan.FromSeconds(10);

    /// <summary>
    /// Maximum size of a single received message. A table page is capped at 500 rows
    /// (see <c>TableDataService</c> in the contract); 16 MiB keeps wide text/JSON rows inside one
    /// batch without falling back to the 4 MiB gRPC default and failing with RESOURCE_EXHAUSTED.
    /// </summary>
    public int MaxReceiveMessageSizeBytes { get; init; } = 16 * 1024 * 1024;
}