using System.Runtime.InteropServices;

namespace Sqarvo.Core.Platform;

/// <summary>
/// Win32 helpers used by the Windows implementations of <see cref="IPidLiveness"/> and the default
/// process launcher. Declarations are compiled on every target (a <c>DllImport</c> only fails when
/// called); callers must guard with <see cref="System.OperatingSystem.IsWindows"/>.
/// </summary>
internal static class WindowsNative
{
    internal const int ProcessQueryLimitedInformation = 0x1000;
    internal const int Synchronize = 0x00100000;
    internal const uint StillActive = 259;
    internal const int ErrorAccessDenied = 5;

    internal const uint JobObjectLimitKillOnJobClose = 0x2000;
    internal const int JobObjectExtendedLimitInformationClass = 9;

    [DllImport("kernel32.dll", SetLastError = true)]
    internal static extern IntPtr OpenProcess(int desiredAccess, [MarshalAs(UnmanagedType.Bool)] bool inheritHandle, int processId);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    internal static extern bool GetExitCodeProcess(IntPtr process, out uint exitCode);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    internal static extern bool CloseHandle(IntPtr handle);

    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    internal static extern IntPtr CreateJobObjectW(IntPtr jobAttributes, string? name);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    internal static extern bool SetInformationJobObject(
        IntPtr job,
        int infoClass,
        ref JobObjectExtendedLimitInformation info,
        uint infoLength);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    internal static extern bool AssignProcessToJobObject(IntPtr job, IntPtr process);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    internal static extern bool TerminateProcess(IntPtr process, uint exitCode);

    [StructLayout(LayoutKind.Sequential)]
    internal struct JobObjectBasicLimitInformation
    {
        internal long PerProcessUserTimeLimit;
        internal long PerJobUserTimeLimit;
        internal uint LimitFlags;
        internal UIntPtr MinimumWorkingSetSize;
        internal UIntPtr MaximumWorkingSetSize;
        internal uint ActiveProcessLimit;
        internal UIntPtr Affinity;
        internal uint PriorityClass;
        internal uint SchedulingClass;
    }

    [StructLayout(LayoutKind.Sequential)]
    internal struct IoCounters
    {
        internal ulong ReadOperationCount;
        internal ulong WriteOperationCount;
        internal ulong OtherOperationCount;
        internal ulong ReadTransferCount;
        internal ulong WriteTransferCount;
        internal ulong OtherTransferCount;
    }

    [StructLayout(LayoutKind.Sequential)]
    internal struct JobObjectExtendedLimitInformation
    {
        internal JobObjectBasicLimitInformation BasicLimitInformation;
        internal IoCounters IoInfo;
        internal UIntPtr ProcessMemoryLimit;
        internal UIntPtr JobMemoryLimit;
        internal UIntPtr PeakProcessMemoryUsed;
        internal UIntPtr PeakJobMemoryUsed;
    }
}

/// <summary>Liveness check backed by <c>OpenProcess</c> + <c>GetExitCodeProcess</c>.</summary>
internal static class WindowsProcessInfo
{
    /// <summary>
    /// Returns whether the process is alive. A pid reused by another user's process cannot be
    /// inspected (<c>ERROR_ACCESS_DENIED</c>); it is reported as alive so the caller connects and lets
    /// the handshake decide, instead of silently starting a second backend.
    /// </summary>
    internal static bool IsAlive(int pid)
    {
        var handle = WindowsNative.OpenProcess(
            WindowsNative.ProcessQueryLimitedInformation | WindowsNative.Synchronize,
            false,
            pid);

        if (handle == IntPtr.Zero)
        {
            return Marshal.GetLastWin32Error() == WindowsNative.ErrorAccessDenied;
        }

        try
        {
            return WindowsNative.GetExitCodeProcess(handle, out var exitCode)
                && exitCode == WindowsNative.StillActive;
        }
        finally
        {
            WindowsNative.CloseHandle(handle);
        }
    }
}

/// <summary>
/// Job object with <c>KILL_ON_JOB_CLOSE</c>, used as the hard guarantee that a backend started by this
/// client dies with it even if the client is killed and never runs its shutdown path.
/// </summary>
internal sealed class WindowsJobObject : IDisposable
{
    private IntPtr _handle;

    private WindowsJobObject(IntPtr handle) => _handle = handle;

    /// <summary>
    /// Creates a job object. Returns <see langword="null"/> when the platform refuses (for example an
    /// old Windows version or a sandbox), in which case the caller falls back to explicit
    /// <c>TerminateProcess</c> only.
    /// </summary>
    internal static WindowsJobObject? TryCreate()
    {
        var handle = WindowsNative.CreateJobObjectW(IntPtr.Zero, null);
        if (handle == IntPtr.Zero)
        {
            return null;
        }

        var info = new WindowsNative.JobObjectExtendedLimitInformation
        {
            BasicLimitInformation = new WindowsNative.JobObjectBasicLimitInformation
            {
                LimitFlags = WindowsNative.JobObjectLimitKillOnJobClose,
            },
        };

        var length = (uint)Marshal.SizeOf<WindowsNative.JobObjectExtendedLimitInformation>();
        if (!WindowsNative.SetInformationJobObject(handle, WindowsNative.JobObjectExtendedLimitInformationClass, ref info, length))
        {
            WindowsNative.CloseHandle(handle);
            return null;
        }

        return new WindowsJobObject(handle);
    }

    /// <summary>Assigns a process to the job; nested jobs are supported on Windows 8 and later.</summary>
    internal bool TryAssign(IntPtr processHandle) =>
        _handle != IntPtr.Zero && WindowsNative.AssignProcessToJobObject(_handle, processHandle);

    /// <summary>Closes the job handle, which terminates every process still assigned to it.</summary>
    public void Dispose()
    {
        var handle = _handle;
        _handle = IntPtr.Zero;
        if (handle != IntPtr.Zero)
        {
            WindowsNative.CloseHandle(handle);
        }
    }

    /// <summary>Terminates a process handle without going through <see cref="System.Diagnostics.Process"/>.</summary>
    internal static bool Terminate(IntPtr processHandle) => WindowsNative.TerminateProcess(processHandle, 1);
}
