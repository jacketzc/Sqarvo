using Sqarvo.Common.V1;
using Sqarvo.Core.Protocol;
using Sqarvo.Services.V1;

namespace Sqarvo.Core.Clients;

/// <summary>
/// Thin wrapper over <c>MetadataService</c>: lazily browse the object tree one level at a time, plus
/// columns, DDL and primary keys.
/// </summary>
/// <remarks>
/// <para>
/// The contract forbids loading a whole schema at once: <see cref="ListNodesAsync"/> returns only the
/// direct children of <c>parent</c> and paginates with an opaque token. A driver that has
/// not implemented a level fails with <c>capability.unsupported</c> instead of returning an empty list,
/// so an empty result genuinely means "this level has no objects".
/// </para>
/// <para>
/// Object kinds differ per database (catalog / schema / table...). Do not collapse them: pass the
/// <c>ObjectRef</c> back exactly as returned by the previous level, including empty
/// <c>catalog</c>/<c>schema</c> fields, and let the backend decide.
/// </para>
/// </remarks>
public sealed class MetadataClient
{
    private readonly MetadataService.MetadataServiceClient _client;
    private readonly SqarvoChannelOptions _options;

    internal MetadataClient(MetadataService.MetadataServiceClient client, SqarvoChannelOptions options)
    {
        _client = client;
        _options = options;
    }

    /// <summary>Lists the direct children of one object node.</summary>
    /// <param name="parent">
    /// Parent reference. To list the connection's current database objects, pass a reference that only
    /// has <c>session_id</c> set; to enumerate catalogs, use <c>kind = OBJECT_KIND_CATALOG</c> with all
    /// name fields empty.
    /// </param>
    /// <param name="pageSize">Requested page size; 0 means the server default.</param>
    /// <param name="pageToken">Opaque token from the previous page, or <see langword="null"/> for the first page.</param>
    /// <param name="filter">Optional name/kind filter, pushed down to the database; <see langword="null"/> means no filter.</param>
    /// <param name="cancellationToken">Cancels the call.</param>
    /// <returns>Nodes plus paging information.</returns>
    public async Task<ListNodesResponse> ListNodesAsync(
        ObjectRef parent,
        int pageSize = 0,
        string? pageToken = null,
        NodeFilter? filter = null,
        CancellationToken cancellationToken = default)
    {
        var request = new ListNodesRequest
        {
            Parent = parent,
            Page = new PageRequest
            {
                PageSize = pageSize <= 0 ? 0u : (uint)pageSize,
                PageToken = pageToken ?? string.Empty,
            },
        };

        if (filter is not null)
        {
            request.Filter = filter;
        }

        var call = _client.ListNodesAsync(
            request,
            deadline: RpcCall.DeadlineAfter(_options.OperationTimeout),
            cancellationToken: cancellationToken);

        return await RpcCall.AwaitAsync(call, "MetadataService.ListNodes", cancellationToken).ConfigureAwait(false);
    }

    /// <summary>Reads the column metadata of a table or view.</summary>
    /// <param name="target">Object reference, exactly as returned by <see cref="ListNodesAsync"/>.</param>
    /// <param name="cancellationToken">Cancels the call.</param>
    /// <returns>Columns in ordinal order.</returns>
    public async Task<IReadOnlyList<ColumnMeta>> GetColumnsAsync(
        ObjectRef target,
        CancellationToken cancellationToken = default)
    {
        var call = _client.GetColumnsAsync(
            new GetColumnsRequest { Object = target },
            deadline: RpcCall.DeadlineAfter(_options.OperationTimeout),
            cancellationToken: cancellationToken);

        var response = await RpcCall.AwaitAsync(call, "MetadataService.GetColumns", cancellationToken)
            .ConfigureAwait(false);

        return response.Columns.ToArray();
    }

    /// <summary>Reads the DDL of an object.</summary>
    /// <param name="target">Object reference.</param>
    /// <param name="cancellationToken">Cancels the call.</param>
    /// <returns>
    /// The DDL text plus whether it is the database's original text (<c>Exact</c>) and where it came
    /// from. When <c>Exact</c> is false the caller must show <c>SourceDetail</c>: the user has to know
    /// the statement was reconstructed.
    /// </returns>
    public async Task<GetDdlResponse> GetDdlAsync(ObjectRef target, CancellationToken cancellationToken = default)
    {
        var call = _client.GetDdlAsync(
            new GetDdlRequest { Object = target },
            deadline: RpcCall.DeadlineAfter(_options.OperationTimeout),
            cancellationToken: cancellationToken);

        return await RpcCall.AwaitAsync(call, "MetadataService.GetDdl", cancellationToken).ConfigureAwait(false);
    }

    /// <summary>Reads the primary key definition of a table.</summary>
    /// <param name="target">Object reference.</param>
    /// <param name="cancellationToken">Cancels the call.</param>
    /// <returns>
    /// Columns in primary-key order, empty for objects without a primary key (such objects are
    /// browsable read-only and cannot be edited).
    /// </returns>
    public async Task<IReadOnlyList<PrimaryKeyColumn>> GetPrimaryKeyAsync(
        ObjectRef target,
        CancellationToken cancellationToken = default)
    {
        var call = _client.GetPrimaryKeyAsync(
            new GetPrimaryKeyRequest { Object = target },
            deadline: RpcCall.DeadlineAfter(_options.OperationTimeout),
            cancellationToken: cancellationToken);

        var response = await RpcCall.AwaitAsync(call, "MetadataService.GetPrimaryKey", cancellationToken)
            .ConfigureAwait(false);

        return response.Columns.ToArray();
    }
}