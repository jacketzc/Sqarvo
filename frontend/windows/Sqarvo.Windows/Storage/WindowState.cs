using Sqarvo.Windows.Theming;
using System;

namespace Sqarvo.Windows.Storage;

/// <summary>
/// 窗口级偏好（<c>%LOCALAPPDATA%\Sqarvo\window.json</c>）的文档模型，schema 与 Linux 的
/// <c>window.json</c> v2 一致（见 docs/ui-specification/scaling-and-window-sizing.md §7）。
/// </summary>
/// <remarks>
/// <para>
/// 为什么不用 <c>ApplicationData</c>：未打包（<c>WindowsPackageType=None</c>）的应用没有包标识，
/// 拿不到 <c>ApplicationData.Current</c>。三端因此共用同一份手写 JSON，字段名与语义一致。
/// </para>
/// <para>
/// 宽度类字段一律存 <b>compact 档的归一化值</b>（实际值 ÷ 档位 scale，四舍五入）。不归一化
/// 就会在切档时来回钳制：1.5× 下存了 630，切回 1.0× 被 <c>inspector_max = 520</c> 钳成 520，
/// 再切回 1.5× 就变成 520，看起来像设置被吃掉。
/// </para>
/// <para>
/// <see cref="Theme"/> 是 Windows 独有的扩展字段（Linux 没有应用内主题设置，GTK 跟随系统）。
/// Linux 的读取器忽略未知字段，因此两端可以共用同一个文件而不互相破坏。
/// </para>
/// </remarks>
public sealed class WindowState
{
    /// <summary>当前文档版本（v1 只有 version / last_inspector_width / inspector_width）。</summary>
    public const int CurrentVersion = 2;

    /// <summary>构造一份「全默认」的文档：不读文件、或文件坏了时用它。</summary>
    public WindowState()
    {
        Tier = Metrics.DefaultTierId;
        LastInspectorWidth = Metrics.CompactInspectorWidth;
        LastSidebarWidth = Metrics.CompactSidebarWidth;
        Theme = "system";
    }

    /// <summary>文档版本。</summary>
    public int Version { get; set; } = CurrentVersion;

    /// <summary>密度档 id（<c>compact</c> / <c>comfortable</c> / <c>large</c>）。</summary>
    public string Tier { get; set; } = Metrics.DefaultTierId;

    /// <summary>信息面板宽度（compact 归一化值）。</summary>
    public int LastInspectorWidth { get; set; } = Metrics.CompactInspectorWidth;

    /// <summary>侧栏宽度（compact 归一化值）。</summary>
    public int LastSidebarWidth { get; set; } = Metrics.CompactSidebarWidth;

    /// <summary>上次窗口客户区宽度（DIP）；null 表示没有可用记忆。</summary>
    public int? WindowWidth { get; set; }

    /// <summary>上次窗口客户区高度（DIP）。</summary>
    public int? WindowHeight { get; set; }

    /// <summary>上次窗口左上角 x（物理像素，按 Windows 习惯记——恢复前会做落在显示器内的校验）。</summary>
    public int? WindowX { get; set; }

    /// <summary>上次窗口左上角 y（物理像素）。</summary>
    public int? WindowY { get; set; }

    /// <summary>上次是否最大化。</summary>
    public bool Maximized { get; set; }

    /// <summary>主题偏好（<c>system</c> / <c>light</c> / <c>dark</c>，Windows 扩展字段）。</summary>
    public string Theme { get; set; } = "system";

    /// <summary>是否有可用的窗口几何记忆。</summary>
    public bool HasGeometry => WindowWidth is > 0 && WindowHeight is > 0;

    /// <summary>主题偏好（强类型）。</summary>
    public AppThemePreference ThemePreference => ThemeService.ParsePreference(Theme);

    /// <summary>
    /// 把归一化宽度还原成指定档位的实际宽度（乘 scale 后四舍五入，并钳到该档位区间）。
    /// </summary>
    public static int Denormalize(int normalized, double scale, int minimum, int maximum)
    {
        var scaled = (int)Math.Round(normalized * scale, MidpointRounding.AwayFromZero);
        return Math.Clamp(scaled, minimum, maximum);
    }

    /// <summary>把实际宽度归一化成 compact 值（÷ scale，四舍五入）。</summary>
    public static int Normalize(int actual, double scale)
        => scale <= 0 ? actual : (int)Math.Round(actual / scale, MidpointRounding.AwayFromZero);

    /// <summary>当前档位下的信息面板宽度（已钳到该档位的 min/max）。</summary>
    public int InspectorWidthFor(DensityTier tier)
    {
        var (scale, minimum, maximum) = tier switch
        {
            DensityTier.Comfortable => (Metrics.ComfortableScale, Metrics.ComfortableInspectorMin, Metrics.ComfortableInspectorMax),
            DensityTier.Large => (Metrics.LargeScale, Metrics.LargeInspectorMin, Metrics.LargeInspectorMax),
            _ => (Metrics.CompactScale, Metrics.CompactInspectorMin, Metrics.CompactInspectorMax),
        };
        return Denormalize(LastInspectorWidth, scale, minimum, maximum);
    }

    /// <summary>当前档位下的侧栏宽度（已钳到该档位的 min/max）。</summary>
    public int SidebarWidthFor(DensityTier tier)
    {
        var (scale, minimum, maximum) = tier switch
        {
            DensityTier.Comfortable => (Metrics.ComfortableScale, Metrics.ComfortableSidebarMin, Metrics.ComfortableSidebarMax),
            DensityTier.Large => (Metrics.LargeScale, Metrics.LargeSidebarMin, Metrics.LargeSidebarMax),
            _ => (Metrics.CompactScale, Metrics.CompactSidebarMin, Metrics.CompactSidebarMax),
        };
        return Denormalize(LastSidebarWidth, scale, minimum, maximum);
    }
}