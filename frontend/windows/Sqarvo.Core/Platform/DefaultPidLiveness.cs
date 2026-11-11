using System.Diagnostics;

using DiagnosticsProcess = System.Diagnostics.Process;

namespace Sqarvo.Core.Platform;

/// <summary>
/// Default <see cref="IPidLiveness"/>: <c>/proc</c> on Linux, <c>OpenProcess</c> on Windows, and
/// <see cref="System.Diagnostics.Process.GetProcessById(int)"/> elsewhere.
/// </summary>
/// <remarks>
/// The Linux branch additionally treats a zombie as dead: the pid directory still exists after a
/// child exits until it is reaped, and reporting "alive" there would make the supervisor reuse a dead
/// backend's endpoint file.
/// </remarks>
public sealed class DefaultPidLiveness : IPidLiveness
{
    /// <inheritdoc />
    public bool IsAlive(int pid)
    {
        if (pid <= 0)
        {
            return false;
        }

#if WINDOWS
        return WindowsProcessInfo.IsAlive(pid);
#else
        if (OperatingSystem.IsLinux())
        {
            return IsAliveOnLinux(pid);
        }

        try
        {
            using var process = DiagnosticsProcess.GetProcessById(pid);
            return !process.HasExited;
        }
        catch (ArgumentException)
        {
            return false;
        }
        catch (InvalidOperationException)
        {
            return false;
        }
#endif
    }

#if !WINDOWS
    private static bool IsAliveOnLinux(int pid)
    {
        var directory = "/proc/" + pid.ToString(System.Globalization.CultureInfo.InvariantCulture);
        if (!Directory.Exists("/proc"))
        {
            // Not a procfs system despite being reported as Linux (unusual, e.g. weird containers).
            try
            {
                using var process = DiagnosticsProcess.GetProcessById(pid);
                return !process.HasExited;
            }
            catch (ArgumentException)
            {
                return false;
            }
        }

        if (!Directory.Exists(directory))
        {
            return false;
        }

        try
        {
            var stat = File.ReadAllText(Path.Combine(directory, "stat"));
            // Format: "<pid> (<comm>) <state> ...". comm may contain spaces and parentheses, so the
            // state character is the first non-space character after the last ')'.
            var close = stat.LastIndexOf(')');
            if (close >= 0 && close + 2 < stat.Length)
            {
                var state = stat[close + 2];
                return state is not ('Z' or 'X' or 'x');
            }
        }
        catch (IOException)
        {
            // Racing with process exit: the directory check already succeeded, so assume alive and
            // let the connection attempt decide.
        }
        catch (UnauthorizedAccessException)
        {
        }

        return true;
    }
#endif
}