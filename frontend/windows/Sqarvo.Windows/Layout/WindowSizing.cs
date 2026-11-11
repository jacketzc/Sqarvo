using Sqarvo.Windows.Theming;
using System;
using System.Collections.Generic;

namespace Sqarvo.Windows.Layout;

/// <summary>
/// 窗口尺寸算术（纯函数）：按**逻辑工作区**分档取推荐尺寸、按工作区收敛最小尺寸、
/// 判断档位在当前显示器上是否可用。
/// </summary>
/// <remarks>
/// <para>
/// 规则来源：docs/ui-specification/scaling-and-window-sizing.md §5.2 / §5.3 / §5.4。
/// 这里是唯一实现，<c>--self-test-window</c> 直接断言本类的边界值，不经过 UI。
/// </para>
/// <para>
/// 单位约定：入参与返回值全部是 <b>DIP</b>（与 GTK 的逻辑像素同一套数字）；
/// 只有 <see cref="DipsToPhysical"/> / <see cref="PhysicalToDips"/> 两处跨到物理像素，
/// 且必须在喂给 <c>AppWindow.Resize</c> 之前完成。
/// </para>
/// <para>
/// 取整：一律 <see cref="MidpointRounding.AwayFromZero"/>，与 Rust 的 <c>f64::round</c>
/// 一致（C# 默认的 half-to-even 会让三端差 1px）。
/// </para>
/// </remarks>
public static class WindowSizing
{
    /// <summary>窗口宽上限 = 工作区宽 - 该值。</summary>
    public const int WidthMargin = 32;

    /// <summary>窗口高上限 = 工作区高 - 该值。</summary>
    public const int HeightMargin = 48;

    /// <summary>100% 档的最小窗口宽（= metrics.min_window_width[compact]）。</summary>
    public const int MinBaseWidth = 960;

    /// <summary>100% 档的最小窗口高（= metrics.min_window_height[compact]）。</summary>
    public const int MinBaseHeight = 640;

    /// <summary>档位对应的密度系数（来自生成文件，不在这里另写一份）。</summary>
    public static double ScaleOf(DensityTier tier) => tier switch
    {
        DensityTier.Comfortable => Metrics.ComfortableScale,
        DensityTier.Large => Metrics.LargeScale,
        _ => Metrics.CompactScale,
    };

    /// <summary>档位的最小窗口宽（<b>未</b>按工作区收敛；可用性守卫用它）。</summary>
    public static int MinWindowWidth(DensityTier tier) => RoundInt(MinBaseWidth * ScaleOf(tier));

    /// <summary>档位的最小窗口高（未按工作区收敛）。</summary>
    public static int MinWindowHeight(DensityTier tier) => RoundInt(MinBaseHeight * ScaleOf(tier));

    /// <summary>
    /// 最小窗口按工作区收敛：<c>min(960·T, W-32) × min(640·T, H-48)</c>。
    /// </summary>
    /// <remarks>
    /// 不收敛的话 <c>960×640</c> 在 1920×1080@200%（逻辑 960×516）上比屏幕还高，
    /// 底部状态栏永远看不到——这条与档位无关，是独立缺陷（spec §5.4，P0）。
    /// </remarks>
    public static (int Width, int Height) MinimumWindow(DensityTier tier, int workWidth, int workHeight)
        => (Math.Min(MinWindowWidth(tier), Math.Max(1, workWidth - WidthMargin)),
            Math.Min(MinWindowHeight(tier), Math.Max(1, workHeight - HeightMargin)));

    /// <summary>
    /// 档位可用性守卫：<c>工作区逻辑宽 ≥ min_window_width(T)</c> 且高同理。
    /// </summary>
    /// <remarks>
    /// 这里用**未收敛**的最小窗口：否则任何屏幕都能声称自己支持 150%（spec §2.2）。
    /// </remarks>
    public static bool TierAvailable(DensityTier tier, int workWidth, int workHeight)
        => workWidth >= MinWindowWidth(tier) && workHeight >= MinWindowHeight(tier);

    /// <summary>
    /// 把请求的档位收敛到当前显示器可用的档位：不可用时**往下一档**回退，
    /// 一路退到 compact（不回退成「静默钳到 100% 却不说话」——调用方负责记日志）。
    /// </summary>
    public static DensityTier ResolveTier(DensityTier requested, int workWidth, int workHeight)
    {
        var tier = requested;
        while (true)
        {
            if (TierAvailable(tier, workWidth, workHeight))
            {
                return tier;
            }

            if (tier == DensityTier.Compact)
            {
                return DensityTier.Compact;
            }

            tier--;
        }
    }

    /// <summary>
    /// 100% 档的基准推荐尺寸（spec §5.2），已按下限与工作区上限收敛。
    /// </summary>
    public static (int Width, int Height) BaseRecommendation(int workWidth, int workHeight)
    {
        double width;
        double height;

        if (workWidth < 1200)
        {
            width = workWidth - 24;
            height = workHeight - 24;
        }
        else if (workWidth < 1700)
        {
            width = workWidth - 56;
            height = Math.Min(0.84 * workHeight, 860);
        }
        else if (workWidth < 2200)
        {
            width = Math.Min(workWidth - 120, 1400);
            height = Math.Min(0.80 * workHeight, 900);
        }
        else
        {
            width = Math.Min(workWidth - 160, 1680);
            height = Math.Min(0.78 * workHeight, 1000);
        }

        var floorWidth = Math.Min(MinBaseWidth, workWidth - WidthMargin);
        var floorHeight = Math.Min(MinBaseHeight, workHeight - HeightMargin);
        width = Math.Max(width, floorWidth);
        height = Math.Max(height, floorHeight);

        // 上限：任何情况下都要留出 WidthMargin / HeightMargin。
        width = Math.Min(width, workWidth - WidthMargin);
        height = Math.Min(height, workHeight - HeightMargin);

        return (RoundInt(width), RoundInt(height));
    }

    /// <summary>
    /// 某档位的推荐窗口（DIP）：基准 × 系数，再钳到 [最小窗口, 工作区 - 边距]。
    /// </summary>
    public static (int Width, int Height) Recommended(DensityTier tier, int workWidth, int workHeight)
    {
        var (baseWidth, baseHeight) = BaseRecommendation(workWidth, workHeight);
        var scale = ScaleOf(tier);

        var maxWidth = Math.Max(1, workWidth - WidthMargin);
        var maxHeight = Math.Max(1, workHeight - HeightMargin);
        var (minWidth, minHeight) = MinimumWindow(tier, workWidth, workHeight);

        var width = Math.Clamp(RoundInt(baseWidth * scale), minWidth, Math.Max(minWidth, maxWidth));
        var height = Math.Clamp(RoundInt(baseHeight * scale), minHeight, Math.Max(minHeight, maxHeight));

        return (width, height);
    }

    /// <summary>DIP → 物理像素（喂 <c>AppWindow</c> 之前乘一次）。</summary>
    public static int DipsToPhysical(double dips, double scale) => RoundInt(dips * scale);

    /// <summary>物理像素 → DIP（读 <c>DisplayArea.WorkArea</c> 之后除一次）。</summary>
    public static double PhysicalToDips(int pixels, double scale) => scale <= 0 ? pixels : pixels / scale;

    /// <summary>与 Rust <c>f64::round</c> 对齐的取整（half away from zero）。</summary>
    public static int RoundInt(double value) => (int)Math.Round(value, MidpointRounding.AwayFromZero);

    /// <summary>
    /// 纯函数边界自检（<c>--self-test-window</c>）：返回失败项，空列表表示全通过。
    /// </summary>
    /// <remarks>
    /// 覆盖分档边界 <c>W = 1199 / 1200 / 1699 / 1700 / 2199 / 2200</c>，
    /// 以及需求里的两条硬约束：窗口不超工作区、最小窗口按工作区收敛。
    /// </remarks>
    public static IReadOnlyList<string> SelfTest()
    {
        var failures = new List<string>();

        void Check(string name, bool ok, string detail)
        {
            if (!ok)
            {
                failures.Add($"{name}（{detail}）");
            }
        }

        const int workHeight = 1000;

        foreach (var workWidth in new[] { 1199, 1200, 1699, 1700, 2199, 2200 })
        {
            foreach (var tier in new[] { DensityTier.Compact, DensityTier.Comfortable, DensityTier.Large })
            {
                var (width, height) = Recommended(tier, workWidth, workHeight);
                var (minWidth, minHeight) = MinimumWindow(tier, workWidth, workHeight);

                Check(
                    $"W={workWidth} {tier} 窗口宽不超工作区",
                    width <= workWidth - WidthMargin && width >= minWidth,
                    $"窗口 {width} 最小 {minWidth} 上限 {workWidth - WidthMargin}");
                Check(
                    $"W={workWidth} {tier} 窗口高不超工作区",
                    height <= workHeight - HeightMargin && height >= minHeight,
                    $"窗口 {height} 最小 {minHeight} 上限 {workHeight - HeightMargin}");
            }
        }

        // 分档边界的具体取值（100% 档、H=1000：高被 H-48=952 的封顶压住）。
        Check("1199 → (W-32, H-48)", Recommended(DensityTier.Compact, 1199, workHeight) == (1167, 952),
            Recommended(DensityTier.Compact, 1199, workHeight).ToString());
        Check("1200 → (W-56, 0.84·H)", Recommended(DensityTier.Compact, 1200, workHeight) == (1144, 840),
            Recommended(DensityTier.Compact, 1200, workHeight).ToString());
        Check("1699 → (W-56, min(0.84·H, 860))", Recommended(DensityTier.Compact, 1699, workHeight) == (1643, 840),
            Recommended(DensityTier.Compact, 1699, workHeight).ToString());
        Check("1700 → (min(W-120,1400), min(0.80·H,900))", Recommended(DensityTier.Compact, 1700, workHeight) == (1400, 800),
            Recommended(DensityTier.Compact, 1700, workHeight).ToString());
        Check("2199 → 1400 宽（封顶）", Recommended(DensityTier.Compact, 2199, workHeight).Width == 1400,
            Recommended(DensityTier.Compact, 2199, workHeight).Width.ToString());
        Check("2200 → (min(W-160,1680), min(0.78·H,1000))", Recommended(DensityTier.Compact, 2200, workHeight) == (1680, 780),
            Recommended(DensityTier.Compact, 2200, workHeight).ToString());

        // 最小窗口按工作区收敛：1920×1080@200% 的逻辑工作区只有 960×516。
        Check("1920x516 的最小窗口收敛到 (960, 468)", MinimumWindow(DensityTier.Compact, 1920, 516) == (960, 468),
            MinimumWindow(DensityTier.Compact, 1920, 516).ToString());
        Check("1366x614 的最小窗口收敛到 (960, 566)", MinimumWindow(DensityTier.Compact, 1366, 614) == (960, 566),
            MinimumWindow(DensityTier.Compact, 1366, 614).ToString());

        // 可用性守卫：1080p@150% 的逻辑工作区放不下 125% 档。
        Check("1280x698 上 large 不可用", !TierAvailable(DensityTier.Large, 1280, 698), "应为不可用");
        Check("1280x698 上 comfortable 不可用", !TierAvailable(DensityTier.Comfortable, 1280, 698), "应为不可用");
        Check("1280x698 回退到 compact", ResolveTier(DensityTier.Large, 1280, 698) == DensityTier.Compact,
            ResolveTier(DensityTier.Large, 1280, 698).ToString());
        Check("1920x1032 上 large 可用", TierAvailable(DensityTier.Large, 1920, 1032), "应可用");

        return failures;
    }
}