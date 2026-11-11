using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;

namespace Sqarvo.Windows.Diagnostics;

/// <summary>
/// 让 WinExe（没有控制台子系统）也能把诊断行打到调用方的终端上。
/// </summary>
/// <remarks>
/// <para>
/// <c>OutputType=WinExe</c> 是必须的：改成 <c>Exe</c> 会在正常启动时闪一个黑框。
/// 但 <c>--report-*</c> 的验收证据要求「打印一行原文」，所以这里按需
/// <c>AttachConsole(ATTACH_PARENT_PROCESS)</c> 借父进程的控制台。
/// </para>
/// <para>
/// 拿不到控制台（双击启动、从资源管理器启动）时静默降级：诊断输出不是产品功能，
/// 不能因为它失败而弹窗或打断启动。UTF-8 是必须的——这些行里有中文。
/// </para>
/// </remarks>
internal static class ConsoleBridge
{
    private const int AttachParentProcess = -1;
    private const int StdOutputHandle = -11;
    private const int StdErrorHandle = -12;
    private static bool _ready;

    /// <summary>尽力接上控制台（幂等）。</summary>
    public static void Ensure()
    {
        if (_ready)
        {
            return;
        }

        _ready = true;
        try
        {
            if (GetConsoleWindow() == IntPtr.Zero)
            {
                AttachConsole(AttachParentProcess);
            }

            if (!HasUsableHandle(StdOutputHandle))
            {
                return;
            }

            var utf8 = new UTF8Encoding(encoderShouldEmitUTF8Identifier: false);
            Console.SetOut(new StreamWriter(Console.OpenStandardOutput(), utf8) { AutoFlush = true });
            if (HasUsableHandle(StdErrorHandle))
            {
                Console.SetError(new StreamWriter(Console.OpenStandardError(), utf8) { AutoFlush = true });
            }
        }
        catch (Exception)
        {
            // 没有控制台 / 句柄不可写：诊断输出降级为「什么都不打」。
        }
    }

    private static bool HasUsableHandle(int which)
    {
        var handle = GetStdHandle(which);
        return handle != IntPtr.Zero && handle != new IntPtr(-1);
    }

    /// <summary>打印一行（自动接控制台）。</summary>
    public static void WriteLine(string line)
    {
        Ensure();
        try
        {
            Console.Out.WriteLine(line);
            Console.Out.Flush();
        }
        catch (Exception)
        {
            // 同上：静默降级。
        }
    }

    /// <summary>
    /// 打印一行内部诊断（走 <b>stderr</b>：stdout 只留给 <c>[theme]</c> / <c>[metrics]</c>
    /// 这类验收证据行，混在一起会让「取一行原文」变得不可靠）。
    /// </summary>
    public static void WriteDiagnostic(string message)
    {
        Ensure();
        try
        {
            Console.Error.WriteLine(message);
            Console.Error.Flush();
        }
        catch (Exception)
        {
            // 没有控制台：静默降级。
        }
    }

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool AttachConsole(int processId);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern IntPtr GetStdHandle(int handle);

    [DllImport("kernel32.dll")]
    private static extern IntPtr GetConsoleWindow();
}