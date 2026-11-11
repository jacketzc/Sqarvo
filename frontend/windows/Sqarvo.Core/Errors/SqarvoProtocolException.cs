namespace Sqarvo.Core.Errors;

/// <summary>
/// The backend speaks an incompatible (or unreadable) protocol version.
/// </summary>
/// <remarks>
/// A major-version mismatch is fatal by design: no amount of retrying or starting another backend
/// can fix it, and starting a second backend would compete with the running one for the endpoint
/// file. The user has to upgrade one side.
/// </remarks>
public sealed class SqarvoProtocolException : SqarvoException
{
    /// <summary>Initialises the exception.</summary>
    /// <param name="kind">Either <see cref="SqarvoErrorKind.ProtocolIncompatible"/> or <see cref="SqarvoErrorKind.ProtocolUnparsable"/>.</param>
    /// <param name="clientVersion">Protocol version this client implements.</param>
    /// <param name="backendVersion">Protocol version reported by the backend, verbatim.</param>
    public SqarvoProtocolException(SqarvoErrorKind kind, string clientVersion, string? backendVersion)
        : base(kind, BuildMessage(kind, clientVersion, backendVersion))
    {
        ClientVersion = clientVersion;
        BackendVersion = backendVersion;
    }

    /// <summary>Protocol version this client implements, for example <c>v1</c>.</summary>
    public string ClientVersion { get; }

    /// <summary>Protocol version reported by the backend, verbatim.</summary>
    public string? BackendVersion { get; }

    private static string BuildMessage(SqarvoErrorKind kind, string clientVersion, string? backendVersion) =>
        kind == SqarvoErrorKind.ProtocolIncompatible
            ? $"协议版本不兼容：客户端 {clientVersion}，后端 {backendVersion}；请升级客户端或后端后再连接"
            : $"后端声明的协议版本无法解析：\"{backendVersion}\"（客户端为 {clientVersion}）";
}