namespace Sqarvo.Core.Process;

/// <summary>
/// Progress stages of <see cref="BackendSupervisor.AcquireAsync(System.Threading.CancellationToken)"/>, reported so the UI can show what the
/// startup is doing (the handshake can take up to the endpoint timeout).
/// </summary>
public enum BackendStartStage
{
    /// <summary>Reading the endpoint file left by a previous backend run.</summary>
    ReadingEndpoint,

    /// <summary>Validating the protocol major version and checking whether the recorded process is alive.</summary>
    CheckingEndpoint,

    /// <summary>Locating and starting the backend executable.</summary>
    StartingBackend,

    /// <summary>Waiting for the backend to publish an endpoint file whose pid matches the child process.</summary>
    WaitingEndpoint,
}