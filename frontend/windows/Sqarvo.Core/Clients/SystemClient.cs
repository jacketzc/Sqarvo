using Sqarvo.Common.V1;
using Sqarvo.Core.Model;
using Sqarvo.Core.Protocol;
using Sqarvo.Services.V1;

namespace Sqarvo.Core.Clients;

/// <summary>
/// Thin wrapper over <c>SystemService</c>: version, health and the registered driver list.
/// </summary>
/// <remarks>
/// Obtained from <see cref="BackendConnection"/>; the generated stub, the channel, the deadline and the
/// token metadata stay inside this assembly so callers cannot accidentally bypass them.
/// </remarks>
public sealed class SystemClient
{
    private readonly SystemService.SystemServiceClient _client;
    private readonly SqarvoChannelOptions _options;

    internal SystemClient(SystemService.SystemServiceClient client, SqarvoChannelOptions options)
    {
        _client = client;
        _options = options;
    }

    /// <summary>Reads the backend version and protocol version.</summary>
    /// <param name="cancellationToken">Cancels the call.</param>
    /// <returns>The backend's declared versions.</returns>
    public async Task<VersionInfo> GetVersionAsync(CancellationToken cancellationToken = default)
    {
        var call = _client.GetVersionAsync(
            new Empty(),
            deadline: RpcCall.DeadlineAfter(_options.DefaultTimeout),
            cancellationToken: cancellationToken);

        var response = await RpcCall.AwaitAsync(call, "SystemService.GetVersion", cancellationToken)
            .ConfigureAwait(false);

        return new VersionInfo(response.ServiceVersion, response.ProtocolVersion, response.BuildProfile);
    }

    /// <summary>Reads the backend health snapshot.</summary>
    /// <param name="cancellationToken">Cancels the call.</param>
    /// <returns>The current health snapshot.</returns>
    public async Task<HealthSnapshot> GetHealthAsync(CancellationToken cancellationToken = default)
    {
        var call = _client.GetHealthAsync(
            new Empty(),
            deadline: RpcCall.DeadlineAfter(_options.DefaultTimeout),
            cancellationToken: cancellationToken);

        var response = await RpcCall.AwaitAsync(call, "SystemService.GetHealth", cancellationToken)
            .ConfigureAwait(false);

        var state = response.State switch
        {
            HealthResponse.Types.State.Serving => HealthState.Serving,
            HealthResponse.Types.State.Degraded => HealthState.Degraded,
            _ => HealthState.Unspecified,
        };

        return new HealthSnapshot(
            state,
            response.Detail,
            TimeSpan.FromSeconds(response.UptimeSeconds),
            response.ActiveSessions,
            response.RegisteredDrivers);
    }

    /// <summary>Lists the drivers registered in this backend build.</summary>
    /// <param name="cancellationToken">Cancels the call.</param>
    /// <returns>
    /// One entry per driver. An empty list means the build contains no drivers yet; it does not mean
    /// "unknown" (the backend never fakes driver availability).
    /// </returns>
    public async Task<IReadOnlyList<DriverSummary>> ListDriversAsync(CancellationToken cancellationToken = default)
    {
        var call = _client.ListDriversAsync(
            new Empty(),
            deadline: RpcCall.DeadlineAfter(_options.DefaultTimeout),
            cancellationToken: cancellationToken);

        var response = await RpcCall.AwaitAsync(call, "SystemService.ListDrivers", cancellationToken)
            .ConfigureAwait(false);

        return response.Drivers
            .Select(static driver => new DriverSummary(
                driver.DriverId,
                driver.DisplayName,
                driver.Version,
                driver.SupportedDatabases
                    .Select(static flavor => new DriverDatabaseFlavor(flavor.Name, flavor.MinVersion, flavor.MaxVersion))
                    .ToArray(),
                driver.Capabilities.ToArray()))
            .ToArray();
    }
}