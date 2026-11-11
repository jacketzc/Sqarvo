namespace Sqarvo.Core.Model;

/// <summary>A database version range declared by a driver.</summary>
/// <param name="Name">Database name, for example <c>MariaDB</c>.</param>
/// <param name="MinVersion">Minimum supported version, empty when unbounded.</param>
/// <param name="MaxVersion">Maximum supported version, empty when unbounded.</param>
public sealed record DriverDatabaseFlavor(string Name, string MinVersion, string MaxVersion);

/// <summary>
/// One driver registered in the backend, as returned by <c>SystemService.ListDrivers</c>.
/// </summary>
/// <remarks>
/// The backend is the only source of this list: the UI must not keep its own copy, otherwise a backend
/// that gains a driver keeps showing a stale list. Capabilities are stable identifiers and the rule is
/// "absent means unsupported" - never try an operation and infer support from the failure.
/// </remarks>
/// <param name="DriverId">Stable driver identifier, for example <c>mariadb</c>.</param>
/// <param name="DisplayName">Display name declared by the driver.</param>
/// <param name="Version">Driver version.</param>
/// <param name="SupportedDatabases">Declared database version ranges.</param>
/// <param name="Capabilities">Capability identifiers such as <c>transactions</c>.</param>
public sealed record DriverSummary(
    string DriverId,
    string DisplayName,
    string Version,
    IReadOnlyList<DriverDatabaseFlavor> SupportedDatabases,
    IReadOnlyList<string> Capabilities)
{
    /// <summary>Label for a driver picker: display name plus stable id (used when matching logs).</summary>
    public string Label => $"{DisplayName}（{DriverId}）";

    /// <summary>Whether the driver declared a capability.</summary>
    /// <param name="capability">Stable capability identifier.</param>
    /// <returns><see langword="true"/> when the identifier is present.</returns>
    public bool Supports(string capability) =>
        Capabilities.Contains(capability, StringComparer.Ordinal);
}