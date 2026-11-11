using System.ComponentModel;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
using Sqarvo.Core.Errors;

using DiagnosticsProcess = System.Diagnostics.Process;

namespace Sqarvo.Core.Platform;

/// <summary>
/// Default <see cref="IProcessLauncher"/>, backed by <see cref="System.Diagnostics.Process"/>.
/// </summary>
/// <remarks>
/// <para>
/// The child gets a closed stdin (the backend must not read the console), no window, and redirected
/// stdout/stderr so a failed start can be explained. Output is captured into a bounded buffer; the
/// backend never prints the session token, and nothing here adds it.
/// </para>
/// <para>
/// Windows additionally puts the child into a job object with <c>KILL_ON_JOB_CLOSE</c>, so the backend
/// dies with the client even when the client is force-killed and never runs its shutdown path
/// (the Windows replacement for <c>PR_SET_PDEATHSIG</c>).
/// </para>
/// </remarks>
public sealed class DefaultProcessLauncher : IProcessLauncher
{
    /// <inheritdoc />
    public IProcessHandle Launch(BackendLaunchOptions options)
    {
        ArgumentNullException.ThrowIfNull(options);

        var startInfo = new ProcessStartInfo(options.ExecutablePath)
        {
            UseShellExecute = false,
            CreateNoWindow = true,
            RedirectStandardInput = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
        };

        if (!string.IsNullOrEmpty(options.WorkingDirectory))
        {
            startInfo.WorkingDirectory = options.WorkingDirectory;
        }

        foreach (var argument in options.Arguments)
        {
            startInfo.ArgumentList.Add(argument);
        }

        foreach (var pair in options.Environment)
        {
            if (pair.Value is null)
            {
                startInfo.Environment.Remove(pair.Key);
            }
            else
            {
                startInfo.Environment[pair.Key] = pair.Value;
            }
        }

        DiagnosticsProcess process;
        try
        {
            process = DiagnosticsProcess.Start(startInfo)
                ?? throw new SqarvoProcessException(
                    SqarvoErrorKind.BackendSpawnFailed,
                    $"无法启动后端进程：{options.ExecutablePath}");
        }
        catch (SqarvoException)
        {
            throw;
        }
        catch (Exception exception) when (exception is Win32Exception or IOException or InvalidOperationException or PlatformNotSupportedException)
        {
            throw new SqarvoProcessException(
                SqarvoErrorKind.BackendSpawnFailed,
                $"无法启动后端进程 {options.ExecutablePath}：{exception.Message}",
                innerException: exception);
        }

        try
        {
            // EOF immediately: the backend does not read stdin, and an inherited console handle would
            // let it consume keystrokes meant for the UI.
            process.StandardInput.Close();
        }
        catch (IOException)
        {
        }

        var handle = new DefaultProcessHandle(process);
        handle.StartDiagnosticCapture();
        return handle;
    }
}

/// <summary>
/// <see cref="IProcessHandle"/> over <see cref="System.Diagnostics.Process"/>, with a Windows job
/// object attached when available.
/// </summary>
internal sealed class DefaultProcessHandle : IProcessHandle
{
    private const int MaxCapturedCharacters = 8 * 1024;

    private readonly DiagnosticsProcess _process;
    private readonly object _captureGate = new();
    private readonly StringBuilder _captured = new();
    private bool _disposed;

#if WINDOWS
    private WindowsJobObject? _job;
    private bool _jobAssigned;
#endif

    internal DefaultProcessHandle(DiagnosticsProcess process)
    {
        _process = process;

#if WINDOWS
        _job = WindowsJobObject.TryCreate();
        _jobAssigned = _job is not null && _job.TryAssign(process.Handle);
#endif
    }

    /// <inheritdoc />
    public int Id => _process.Id;

    /// <inheritdoc />
    public bool HasExited
    {
        get
        {
            try
            {
                return _process.HasExited;
            }
            catch (InvalidOperationException)
            {
                return true;
            }
        }
    }

    /// <inheritdoc />
    public int? ExitCode
    {
        get
        {
            try
            {
                return _process.HasExited ? _process.ExitCode : null;
            }
            catch (InvalidOperationException)
            {
                return null;
            }
        }
    }

    /// <inheritdoc />
    public string CapturedDiagnostics
    {
        get
        {
            lock (_captureGate)
            {
                return _captured.ToString();
            }
        }
    }

    internal void StartDiagnosticCapture()
    {
        _ = Task.Run(CaptureAsync);
    }

    /// <inheritdoc />
    public bool RequestGracefulExit()
    {
#if WINDOWS
        // Windows has no SIGTERM and the contract has no shutdown RPC: there is no graceful channel to
        // the backend, so report "not delivered" and let the caller terminate after the grace period
        // (the job object is the hard guarantee that nothing is left behind).
        return false;
#else
        if (HasExited)
        {
            return false;
        }

        // SIGTERM lets the backend run its cleanup path (delete socket and endpoint file).
        return UnixSignals.SendTerminate(_process.Id);
#endif
    }

    /// <inheritdoc />
    public void Kill()
    {
        try
        {
            if (!HasExited)
            {
                _process.Kill(entireProcessTree: false);
            }
        }
        catch (InvalidOperationException)
        {
            // Already gone.
        }
        catch (System.ComponentModel.Win32Exception)
        {
            // Access denied on a process that is already exiting: the subsequent wait decides.
        }
    }

    /// <inheritdoc />
    public async Task<bool> WaitForExitAsync(TimeSpan timeout, CancellationToken cancellationToken = default)
    {
        if (HasExited)
        {
            return true;
        }

        using var timeoutSource = new CancellationTokenSource(timeout);
        using var linked = CancellationTokenSource.CreateLinkedTokenSource(timeoutSource.Token, cancellationToken);
        try
        {
            await _process.WaitForExitAsync(linked.Token).ConfigureAwait(false);
            return true;
        }
        catch (OperationCanceledException)
        {
            if (cancellationToken.IsCancellationRequested)
            {
                throw;
            }

            return HasExited;
        }
    }

    private async Task CaptureAsync()
    {
        var stdout = DrainAsync(_process.StandardOutput, "stdout");
        var stderr = DrainAsync(_process.StandardError, "stderr");
        await Task.WhenAll(stdout, stderr).ConfigureAwait(false);
    }

    private async Task DrainAsync(StreamReader reader, string source)
    {
        var buffer = new char[1024];
        try
        {
            while (true)
            {
                var read = await reader.ReadAsync(buffer, 0, buffer.Length).ConfigureAwait(false);
                if (read <= 0)
                {
                    return;
                }

                Append(source, new string(buffer, 0, read));
            }
        }
        catch (Exception exception) when (exception is IOException or ObjectDisposedException or InvalidOperationException)
        {
            // Process exited and the pipe was torn down; whatever was captured stays captured.
        }
    }

    private void Append(string source, string text)
    {
        lock (_captureGate)
        {
            _captured.Append('[').Append(source).Append("] ").Append(text);
            if (_captured.Length > MaxCapturedCharacters)
            {
                _captured.Remove(0, _captured.Length - MaxCapturedCharacters);
            }
        }
    }

    /// <inheritdoc />
    public void Dispose()
    {
        if (_disposed)
        {
            return;
        }

        _disposed = true;

#if WINDOWS
        if (_job is not null)
        {
            // Closing the job handle terminates any process still in the job: the last-resort
            // guarantee that a force-killed client leaves no orphan backend.
            _job.Dispose();
            _job = null;
        }
#endif
        _process.Dispose();
    }
}

#if !WINDOWS
/// <summary>Unix signal helpers. Only <c>SIGTERM</c> is used; termination uses
/// <see cref="System.Diagnostics.Process.Kill(bool)"/>, which maps to <c>SIGKILL</c>.</summary>
internal static class UnixSignals
{
    private const int SigTerm = 15;

    [DllImport("libc", SetLastError = true, EntryPoint = "kill")]
    private static extern int Kill(int pid, int signal);

    /// <summary>Sends <c>SIGTERM</c>; returns whether the signal was accepted.</summary>
    internal static bool SendTerminate(int pid)
    {
        // ESRCH (3) means the process is already gone: treat it as "no graceful channel needed".
        return Kill(pid, SigTerm) == 0;
    }
}
#endif