using System.Globalization;
using Sqarvo.Core.Errors;

namespace Sqarvo.Core.Protocol;

/// <summary>
/// Protocol version handshake rules shared by the endpoint reader, the supervisor and the channel.
/// </summary>
/// <remarks>
/// Only the <b>major</b> version is compared: within one major version the contract only ever gains
/// fields, which proto3 clients tolerate. A different major version means message layouts may have
/// been reused, so the connection is refused and no second backend is started (two processes cannot
/// fix a version mismatch and would compete for the endpoint file).
/// </remarks>
public static class ProtocolVersion
{
    /// <summary>The gRPC contract major version this client implements, for example <c>v1</c>.</summary>
    public const string Client = "v1";

    /// <summary>
    /// Extracts the major version from a backend-declared version string.
    /// </summary>
    /// <param name="version">Version string such as <c>v1</c> or <c>v2.7</c>.</param>
    /// <param name="major">Parsed major version when the call succeeds.</param>
    /// <returns><see langword="true"/> when the string has the form <c>v&lt;digits&gt;[.&lt;rest&gt;]</c>.</returns>
    public static bool TryGetMajor(string? version, out int major)
    {
        major = 0;
        if (string.IsNullOrWhiteSpace(version))
        {
            return false;
        }

        var trimmed = version.Trim();
        if (trimmed.Length < 2 || trimmed[0] is not ('v' or 'V'))
        {
            return false;
        }

        var rest = trimmed.AsSpan(1);
        var dot = rest.IndexOf('.');
        var digits = dot < 0 ? rest : rest[..dot];
        if (digits.IsEmpty)
        {
            return false;
        }

        foreach (var character in digits)
        {
            if (!char.IsAsciiDigit(character))
            {
                return false;
            }
        }

        return int.TryParse(digits, NumberStyles.None, CultureInfo.InvariantCulture, out major);
    }

    /// <summary>
    /// Returns whether a backend-declared version is compatible with <see cref="Client"/>.
    /// </summary>
    /// <param name="backendVersion">Version string reported by the backend.</param>
    /// <returns><see langword="true"/> when both strings parse and the major versions match.</returns>
    public static bool IsCompatible(string? backendVersion) =>
        TryGetMajor(backendVersion, out var backendMajor)
        && TryGetMajor(Client, out var clientMajor)
        && backendMajor == clientMajor;

    /// <summary>
    /// Throws unless the backend-declared version is compatible with <see cref="Client"/>.
    /// </summary>
    /// <param name="backendVersion">Version string reported by the backend.</param>
    /// <exception cref="SqarvoProtocolException">
    /// <see cref="SqarvoErrorKind.ProtocolUnparsable"/> when the string is malformed,
    /// <see cref="SqarvoErrorKind.ProtocolIncompatible"/> when the major version differs.
    /// </exception>
    public static void EnsureCompatible(string? backendVersion)
    {
        if (!TryGetMajor(backendVersion, out var backendMajor))
        {
            throw new SqarvoProtocolException(SqarvoErrorKind.ProtocolUnparsable, Client, backendVersion);
        }

        if (!TryGetMajor(Client, out var clientMajor))
        {
            throw new SqarvoProtocolException(SqarvoErrorKind.ProtocolUnparsable, Client, Client);
        }

        if (backendMajor != clientMajor)
        {
            throw new SqarvoProtocolException(SqarvoErrorKind.ProtocolIncompatible, Client, backendVersion);
        }
    }
}