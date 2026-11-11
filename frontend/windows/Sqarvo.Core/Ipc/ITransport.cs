namespace Sqarvo.Core.Ipc;

/// <summary>
/// Transport abstraction for the local backend connection.
/// </summary>
/// <remarks>
/// Business code never depends on a concrete socket, pipe or port. A platform or packaging change
/// (for example moving from loopback TCP to named pipes on Windows) adds one implementation here and
/// one branch in <see cref="TransportFactory"/>; nothing else changes.
/// </remarks>
public interface ITransport
{
    /// <summary>Transport kind this instance implements.</summary>
    TransportKind Kind { get; }

    /// <summary>Safe-to-display transport description, for diagnostics and error messages.</summary>
    string Description { get; }

    /// <summary>
    /// Base address the gRPC channel dials. For stream-like transports (named pipes) this is a
    /// placeholder; the actual connection is made by <see cref="CreateHandler"/>.
    /// </summary>
    Uri ChannelAddress { get; }

    /// <summary>
    /// Creates a fresh HTTP/2 message handler that connects exclusively over this transport. Each
    /// gRPC channel owns one handler and disposes it with the channel.
    /// </summary>
    /// <returns>A handler ready to be passed to <c>GrpcChannelOptions.HttpHandler</c>.</returns>
    HttpMessageHandler CreateHandler();
}