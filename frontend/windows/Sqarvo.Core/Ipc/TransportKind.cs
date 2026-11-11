namespace Sqarvo.Core.Ipc;

/// <summary>
/// Transport used between the desktop client and the backend service, as declared by the
/// backend's endpoint file.
/// </summary>
public enum TransportKind
{
    /// <summary>Unix domain socket (Linux and macOS backends).</summary>
    Unix,

    /// <summary>Loopback TCP with a per-process session token (the Windows transport).</summary>
    Tcp,
}