using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media.Imaging;
using System;
using System.IO;
using System.Runtime.InteropServices.WindowsRuntime;
using System.Threading.Tasks;
using Windows.Graphics.Imaging;
using Windows.Storage.Streams;

namespace Sqarvo.Windows.Diagnostics;

/// <summary>
/// <c>--screenshot=&lt;路径&gt;</c>：把窗口内容渲染成 PNG，不再依赖外部 GDI 抓屏。
/// </summary>
/// <remarks>
/// <para>
/// 用 <see cref="RenderTargetBitmap"/> 渲染 <see cref="Window.Content"/>：拿到的是 XAML 自己画出来的像素，
/// 因此不受前台窗口、别的程序遮挡、显示器边缘影响——外部 BitBlt 那三个坑都不存在，也不需要审批。
/// </para>
/// <para>
/// 失败只打一行 <c>[screenshot] failed 类型: 消息</c>，不往外抛：这是诊断开关，不能因为它把启动链路打断。
/// 行格式是验收证据的一部分（<c>[screenshot] path=… bytes=… size=WxH</c>），改动要连 README 一起改。
/// </para>
/// </remarks>
internal static class ScreenshotService
{
    /// <summary>等 <paramref name="delaySeconds"/> 秒后渲染窗口内容并存到 <paramref name="path"/>。</summary>
    public static async Task CaptureAsync(Window window, string path, double delaySeconds)
    {
        try
        {
            if (delaySeconds > 0)
            {
                // 延迟发生在 UI 线程的 await 上：期间界面继续绘制、后端继续握手，到点再取一帧。
                await Task.Delay(TimeSpan.FromSeconds(delaySeconds));
            }

            if (window.Content is not FrameworkElement root)
            {
                ConsoleBridge.WriteLine("[screenshot] failed InvalidOperationException: 窗口内容不是 FrameworkElement");

                return;
            }

            var bitmap = new RenderTargetBitmap();
            await bitmap.RenderAsync(root);

            var width = bitmap.PixelWidth;
            var height = bitmap.PixelHeight;
            var pixels = await bitmap.GetPixelsAsync();

            var raw = new byte[width * height * 4];
            using (var reader = DataReader.FromBuffer(pixels))
            {
                reader.ReadBytes(raw);
            }

            using var stream = new InMemoryRandomAccessStream();
            var encoder = await BitmapEncoder.CreateAsync(BitmapEncoder.PngEncoderId, stream);
            encoder.SetPixelData(
                BitmapPixelFormat.Bgra8,
                BitmapAlphaMode.Premultiplied,
                (uint)width,
                (uint)height,
                96d,
                96d,
                raw);
            await encoder.FlushAsync();

            var encoded = new byte[(int)stream.Size];
            stream.Seek(0);
            using (var reader = new DataReader(stream.GetInputStreamAt(0)))
            {
                await reader.LoadAsync((uint)stream.Size);
                reader.ReadBytes(encoded);
            }

            var full = Path.GetFullPath(path);
            var folder = Path.GetDirectoryName(full);
            if (!string.IsNullOrEmpty(folder))
            {
                Directory.CreateDirectory(folder);
            }

            File.WriteAllBytes(full, encoded);
            ConsoleBridge.WriteLine($"[screenshot] path={full} bytes={encoded.Length} size={width}x{height}");
        }
        catch (Exception error)
        {
            // 兜住一切：诊断开关的失败只写一行证据，不抛给调用方。
            ConsoleBridge.WriteLine($"[screenshot] failed {error.GetType().Name}: {error.Message}");
        }
    }
}