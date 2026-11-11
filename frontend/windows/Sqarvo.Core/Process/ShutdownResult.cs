namespace Sqarvo.Core.Process;

/// <summary>Outcome of <see cref="BackendSupervisor.ShutdownAsync"/>.</summary>
/// <param name="Graceful">
/// Whether the backend accepted the graceful signal and exited within the grace period. Always
/// <see langword="false"/> on Windows, which has neither <c>SIGTERM</c> nor a shutdown RPC.
/// </param>
/// <param name="Forced">Whether the process had to be terminated instead of exiting on its own.</param>
/// <param name="EndpointFileRemoved">
/// Whether no endpoint file belonging to this backend remains. <see langword="false"/> means a stale file
/// may still be on disk; a later start treats it as stale because its pid is dead, so this is a
/// cleanliness issue, not a functional one.
/// </param>
/// <param name="OwnedBackend">
/// Whether this supervisor had started the backend. <see langword="false"/> means the endpoint belonged
/// to another process and nothing was killed or deleted.
/// </param>
/// <param name="Elapsed">How long the shutdown took.</param>
public sealed record ShutdownResult(
    bool Graceful,
    bool Forced,
    bool EndpointFileRemoved,
    bool OwnedBackend,
    TimeSpan Elapsed);