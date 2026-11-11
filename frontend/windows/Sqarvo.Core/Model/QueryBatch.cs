using Sqarvo.Common.V1;

namespace Sqarvo.Core.Model;

/// <summary>
/// One page of a result set returned by <c>QueryService.FetchBatch</c> (or by the first batch of
/// <c>Execute</c>).
/// </summary>
/// <param name="Rows">Rows in this batch; never the whole result set.</param>
/// <param name="PageToken">Opaque cursor for the next batch; empty when there is none.</param>
/// <param name="HasMore">Whether more rows exist.</param>
/// <param name="Complete">Whether the result set is exhausted and its server-side resources released.</param>
public sealed record QueryBatch(
    IReadOnlyList<Row> Rows,
    string PageToken,
    bool HasMore,
    bool Complete);