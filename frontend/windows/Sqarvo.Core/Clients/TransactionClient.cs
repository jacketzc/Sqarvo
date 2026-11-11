using Sqarvo.Core.Protocol;
using Sqarvo.Services.V1;

namespace Sqarvo.Core.Clients;

/// <summary>
/// Thin wrapper over <c>TransactionService</c>: begin, commit, rollback.
/// </summary>
/// <remarks>
/// <para>
/// <c>Commit</c> and <c>Rollback</c> are idempotent for ended or unknown transactions (ADR 0017 §3);
/// "the transaction must exist" is enforced on the write paths instead. Drivers without transaction
/// support fail with <c>capability.unsupported</c> rather than silently auto-committing, so this
/// client never pretends a transaction exists.
/// </para>
/// <para>
/// A failed transaction reports <c>TransactionState.Failed</c> and can only be rolled back; do not send
/// further statements through it.
/// </para>
/// </remarks>
public sealed class TransactionClient
{
    private readonly TransactionService.TransactionServiceClient _client;
    private readonly SqarvoChannelOptions _options;

    internal TransactionClient(TransactionService.TransactionServiceClient client, SqarvoChannelOptions options)
    {
        _client = client;
        _options = options;
    }

    /// <summary>Begins an explicit transaction on a session.</summary>
    /// <param name="sessionId">Session that will own the transaction.</param>
    /// <param name="isolation">Requested isolation level; <c>Unspecified</c> leaves it to the database.</param>
    /// <param name="readOnly">Whether the transaction must be read-only.</param>
    /// <param name="cancellationToken">Cancels the call.</param>
    /// <returns>The new transaction descriptor (always <c>Active</c>).</returns>
    public async Task<TransactionInfo> BeginTransactionAsync(
        string sessionId,
        IsolationLevel isolation = IsolationLevel.Unspecified,
        bool readOnly = false,
        CancellationToken cancellationToken = default)
    {
        var call = _client.BeginAsync(
            new BeginTransactionRequest
            {
                SessionId = sessionId,
                Isolation = isolation,
                ReadOnly = readOnly,
            },
            deadline: RpcCall.DeadlineAfter(_options.DefaultTimeout),
            cancellationToken: cancellationToken);

        return await RpcCall.AwaitAsync(call, "TransactionService.Begin", cancellationToken).ConfigureAwait(false);
    }

    /// <summary>Commits a transaction. Idempotent for already ended transactions.</summary>
    /// <param name="transactionId">Transaction to commit.</param>
    /// <param name="cancellationToken">Cancels the call.</param>
    public async Task CommitAsync(string transactionId, CancellationToken cancellationToken = default)
    {
        var call = _client.CommitAsync(
            new TransactionRef { TransactionId = transactionId },
            deadline: RpcCall.DeadlineAfter(_options.DefaultTimeout),
            cancellationToken: cancellationToken);

        await RpcCall.AwaitAsync(call, "TransactionService.Commit", cancellationToken).ConfigureAwait(false);
    }

    /// <summary>Rolls a transaction back. Idempotent for already ended transactions.</summary>
    /// <param name="transactionId">Transaction to roll back.</param>
    /// <param name="cancellationToken">Cancels the call.</param>
    public async Task RollbackAsync(string transactionId, CancellationToken cancellationToken = default)
    {
        var call = _client.RollbackAsync(
            new TransactionRef { TransactionId = transactionId },
            deadline: RpcCall.DeadlineAfter(_options.DefaultTimeout),
            cancellationToken: cancellationToken);

        await RpcCall.AwaitAsync(call, "TransactionService.Rollback", cancellationToken).ConfigureAwait(false);
    }
}