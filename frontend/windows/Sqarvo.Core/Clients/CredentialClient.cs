using Sqarvo.Core.Protocol;
using Sqarvo.Services.V1;

namespace Sqarvo.Core.Clients;

/// <summary>
/// Thin wrapper over <c>CredentialService</c>: the only contract path that carries a database password.
/// </summary>
/// <remarks>
/// <para>
/// The secret exists only as a method argument and inside the single request message. It is never
/// stored in a field, never written to configuration, never logged and never part of an exception
/// message produced here. The backend writes it to the OS credential store (Windows Credential
/// Manager on this platform) and returns a reference that is safe to persist.
/// </para>
/// <para>
/// If the credential store is unavailable the backend fails with <c>UNAVAILABLE</c> and
/// <c>credential.unavailable</c>; it never falls back to plaintext storage, and neither does this
/// client.
/// </para>
/// </remarks>
public sealed class CredentialClient
{
    private readonly CredentialService.CredentialServiceClient _client;
    private readonly SqarvoChannelOptions _options;

    internal CredentialClient(CredentialService.CredentialServiceClient client, SqarvoChannelOptions options)
    {
        _client = client;
        _options = options;
    }

    /// <summary>Stores (or overwrites) a password in the OS credential store.</summary>
    /// <param name="credentialRef">
    /// Existing reference to overwrite, or <see langword="null"/>/empty to let the backend mint a new
    /// one. References look like <c>sqarvo:conn/&lt;uuid-v4&gt;</c>.
    /// </param>
    /// <param name="secret">Password to store. Not retained by this instance.</param>
    /// <param name="cancellationToken">Cancels the call.</param>
    /// <returns>The reference to persist in the connection configuration.</returns>
    public async Task<string> StoreCredentialAsync(
        string? credentialRef,
        string secret,
        CancellationToken cancellationToken = default)
    {
        var call = _client.StoreCredentialAsync(
            new StoreCredentialRequest { CredentialRef = credentialRef ?? string.Empty, Secret = secret },
            deadline: RpcCall.DeadlineAfter(_options.DefaultTimeout),
            cancellationToken: cancellationToken);

        var response = await RpcCall.AwaitAsync(call, "CredentialService.StoreCredential", cancellationToken)
            .ConfigureAwait(false);

        return response.CredentialRef;
    }

    /// <summary>Deletes a stored credential. Deleting a non-existent reference succeeds (idempotent).</summary>
    /// <param name="credentialRef">Reference to delete.</param>
    /// <param name="cancellationToken">Cancels the call.</param>
    public async Task DeleteCredentialAsync(string credentialRef, CancellationToken cancellationToken = default)
    {
        var call = _client.DeleteCredentialAsync(
            new DeleteCredentialRequest { CredentialRef = credentialRef },
            deadline: RpcCall.DeadlineAfter(_options.DefaultTimeout),
            cancellationToken: cancellationToken);

        await RpcCall.AwaitAsync(call, "CredentialService.DeleteCredential", cancellationToken).ConfigureAwait(false);
    }
}