using System;
using System.IO;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace Sqarvo.Windows.Storage;

/// <summary>
/// <c>%LOCALAPPDATA%\Sqarvo\window.json</c> 的读写（未打包应用拿不到 <c>ApplicationData</c>）。
/// </summary>
/// <remarks>
/// 三条规则来自 spec §7，读代码时不容易看出来，写在这里：
/// <list type="number">
/// <item>读失败（文件不存在 / 半个 JSON / 手改坏）一律回退默认值，**不弹窗、不抛给用户**：
/// 窗口状态是便利功能，不是数据。</item>
/// <item>写用「临时文件 + 替换」，进程崩溃不会留下半个 JSON。</item>
/// <item>文件里不含任何凭据：它与 <c>connections.json</c>（用户数据）分开，只放窗口级偏好。</item>
/// </list>
/// </remarks>
public static class WindowStateStore
{
    private static readonly JsonSerializerOptions SerializerOptions = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
        WriteIndented = true,
        ReadCommentHandling = JsonCommentHandling.Skip,
        AllowTrailingCommas = true,
    };

    /// <summary>默认路径：<c>%LOCALAPPDATA%\Sqarvo\window.json</c>。</summary>
    public static string FilePath => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        "Sqarvo",
        "window.json");

    /// <summary>读取；任何失败都回退默认值，并把原因记到 stderr 级别的诊断里（不弹窗）。</summary>
    public static WindowState Load()
    {
        var path = FilePath;
        try
        {
            if (!File.Exists(path))
            {
                return new WindowState();
            }

            var text = File.ReadAllText(path);
            var state = JsonSerializer.Deserialize<WindowState>(text, SerializerOptions);
            if (state is null)
            {
                return new WindowState();
            }

            // v1 文档没有 tier / 侧栏 / 几何：缺省字段保持构造时的默认值即可。
            if (state.Version < WindowState.CurrentVersion)
            {
                state.Version = WindowState.CurrentVersion;
            }

            return state;
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or JsonException or NotSupportedException)
        {
            Diagnostics.ConsoleBridge.WriteDiagnostic($"[state] 读取 {path} 失败（{error.GetType().Name}），本次按默认值处理");
            return new WindowState();
        }
    }

    /// <summary>写入（临时文件 + 替换）。失败只记诊断，不打断退出流程。</summary>
    public static void Save(WindowState state)
    {
        ArgumentNullException.ThrowIfNull(state);

        var path = FilePath;
        var directory = Path.GetDirectoryName(path);
        string? temporary = null;
        try
        {
            if (!string.IsNullOrEmpty(directory))
            {
                Directory.CreateDirectory(directory);
            }

            state.Version = WindowState.CurrentVersion;
            temporary = path + ".tmp";
            var text = JsonSerializer.Serialize(state, SerializerOptions);
            File.WriteAllText(temporary, text);
            File.Move(temporary, path, overwrite: true);
            temporary = null;
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or NotSupportedException)
        {
            Diagnostics.ConsoleBridge.WriteDiagnostic($"[state] 写入 {path} 失败（{error.GetType().Name}）");
        }
        finally
        {
            if (temporary is not null)
            {
                try
                {
                    File.Delete(temporary);
                }
                catch (Exception)
                {
                    // 临时文件删不掉不影响正确性：下次写入会覆盖它。
                }
            }
        }
    }
}