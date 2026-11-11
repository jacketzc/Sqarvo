namespace Sqarvo.Core.Model;

/// <summary>Backend health state as reported by <c>SystemService.GetHealth</c>.</summary>
public enum HealthState
{
    /// <summary>Not reported, or a value this client does not know (newer backend).</summary>
    Unspecified,

    /// <summary>Serving normally.</summary>
    Serving,

    /// <summary>Serving with degraded capabilities; <see cref="HealthSnapshot.Detail"/> explains how.</summary>
    Degraded,
}

/// <summary>
/// Snapshot of the backend's health. Used for the periodic 5 s poll and for diagnostics; it is not part
/// of the startup handshake (the endpoint file plus a successful RPC already prove the backend is up).
/// </summary>
/// <param name="State">Reported state.</param>
/// <param name="Detail">Backend-provided explanation; contains no credentials or private paths.</param>
/// <param name="Uptime">How long the backend process has been running.</param>
/// <param name="ActiveSessions">Number of open database sessions.</param>
/// <param name="RegisteredDrivers">Number of registered drivers.</param>
public sealed record HealthSnapshot(
    HealthState State,
    string Detail,
    TimeSpan Uptime,
    uint ActiveSessions,
    uint RegisteredDrivers);