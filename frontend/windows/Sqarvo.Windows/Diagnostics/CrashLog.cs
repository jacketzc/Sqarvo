using System;
using System.IO;
using System.Text;

namespace Sqarvo.Windows.Diagnostics;

/// <summary>
/// 把未处理异常与关键路径异常追加到 <c>%LOCALAPPDATA%\Sqarvo\crash.log</c>。
/// </summary>
/// <remarks>
/// 存在的理由：WinUI 的启动期崩溃（<c>0xc000027b</c>，stowed exception）在事件日志里只有
/// 「出错模块 Microsoft.UI.Xaml.dll」，没有托管堆栈；而这类崩溃往往发生在 XAML 解析或控件实例化，
/// 只有托管堆栈才指得出是哪个资源键、哪个控件。stdout/stderr 在 GUI 进程里不一定有人接，
/// 所以落文件更可靠。文件只追加、单条上限 64 KB，不写任何凭据。
/// </remarks>
internal static class CrashLog
{
    private const int MaxChars = 64 * 1024;

    private static readonly object Gate = new();

    internal static string FilePath
    {
        get
        {
            var local = Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData);
            if (string.IsNullOrEmpty(local))
            {
                local = Path.GetTempPath();
            }

            return Path.Combine(local, "Sqarvo", "crash.log");
        }
    }

    internal static void Write(string origin, Exception? error)
    {
        if (error is null)
        {
            return;
        }

        var text = new StringBuilder()
            .Append('[').Append(DateTimeOffset.Now.ToString("O")).Append("] ").Append(origin).AppendLine()
            .AppendLine(error.ToString())
            .ToString();

        if (text.Length > MaxChars)
        {
            text = text.Substring(0, MaxChars);
        }

        try
        {
            lock (Gate)
            {
                var path = FilePath;
                Directory.CreateDirectory(Path.GetDirectoryName(path)!);
                File.AppendAllText(path, text + Environment.NewLine);
            }
        }
        catch
        {
            // 记日志失败不能反过来把应用弄崩。
        }
    }
}
