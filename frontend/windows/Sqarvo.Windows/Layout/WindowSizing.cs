using Sqarvo.Windows.Theming;
using System;
using System.Collections.Generic;

namespace Sqarvo.Windows.Layout;

/// <summary>
/// 窗口尺寸算术（纯函数）：按**逻辑工作区**分档取推荐尺寸、按工作区收敛最小尺寸、
/// 判断档位在当前显示器上是否可用、由度量推出菜单行高度。
/// </summary>
/// <remarks>
/// <para>
/// 规则来源：docs/ui-specification/scaling-and-window-sizing.md §5.2 / §5.3 / §5.4；
/// 数字来源只有一处——<c>docs/ui-specification/metrics.toml</c>（经 <see cref="Metrics"/> 读）。
/// 本类**不**另写一份基准尺寸：100% 档的最小窗口就是 <c>Metrics.CompactMinWindowWidth</c> /
/// <c>CompactMinWindowHeight</c>，其它档位读各自的档位常量（三档在 TOML 里都写死了整数，
/// 这里只查表，不做「基准 × scale」，见 §3.1）。
/// </para>
/// <para>
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
    /// <summary>窗口宽上限 = 工作区宽 - 该值（规范 §5.4 的 <c>W-32</c>）。</summary>
    public const int WidthMargin = 32;

    /// <summary>窗口高上限 = 工作区高 - 该值（规范 §5.4 的 <c>H-48</c>）。</summary>
    public const int HeightMargin = 48;

    /// <summary>档位对应的密度系数（来自生成文件，不在这里另写一份）。</summary>
    public static double ScaleOf(DensityTier tier) => tier switch
    {
        DensityTier.Comfortable => Metrics.ComfortableScale,
        DensityTier.Large => Metrics.LargeScale,
        _ => Metrics.CompactScale,
    };

    /// <summary>档位的外壳高度（<c>header_height</c>：64 / 80 / 96）。</summary>
    public static int HeaderHeightOf(DensityTier tier) => tier switch
    {
        DensityTier.Comfortable => Metrics.ComfortableHeaderHeight,
        DensityTier.Large => Metrics.LargeHeaderHeight,
        _ => Metrics.CompactHeaderHeight,
    };

    /// <summary>档位的标签条 / 菜单行基准高度（<c>tab_strip_height</c>：24 / 30 / 36）。</summary>
    public static int TabStripHeightOf(DensityTier tier) => tier switch
    {
        DensityTier.Comfortable => Metrics.ComfortableTabStripHeight,
        DensityTier.Large => Metrics.LargeTabStripHeight,
        _ => Metrics.CompactTabStripHeight,
    };

    /// <summary>档位的最小点击区（<c>min_target</c>：24 / 30 / 36）。</summary>
    public static int MinTargetOf(DensityTier tier) => tier switch
    {
        DensityTier.Comfortable => Metrics.ComfortableMinTarget,
        DensityTier.Large => Metrics.LargeMinTarget,
        _ => Metrics.CompactMinTarget,
    };

    /// <summary>
    /// 窗口内菜单行的高度（DIP）：<c>外壳高 − 系统标题栏基准 × 档位</c>。
    /// </summary>
    /// <remarks>
    /// <para>
    /// 三档取值 24 / 30 / 36，与 <c>tab_strip_height</c> 同值——这不是巧合：规范的外壳算术是
    /// 「外壳 64 = 系统标题栏 40 + 菜单行 24」（workspace-model.md §2），按档位换算后
    /// 80 = 50 + 30、96 = 60 + 36。菜单行是**我们的**元素，所以它按档位放大；
    /// 系统标题栏不是我们的（§8.7「仅缩放在本文件管辖范围内的自有元素」）。
    /// </para>
    /// <para>
    /// <see cref="TitleBarBaseDips"/> 由度量相减推出（64 − 24 = 40），代码里因此没有
    /// 「40」这个字面量；实际系统标题栏高度由 <see cref="MeasuredNonClientDips"/> 在运行期
    /// 测量并写进 <c>[monitor]</c> 行，偏离开口径时记一行诊断（不为了让总高等于度量而裁剪
    /// 系统标题栏）。
    /// </para>
    /// </remarks>
    public static int MenuRowHeight(DensityTier tier)
    {
        var scaledTitleBar = RoundInt(TitleBarBaseDips * ScaleOf(tier));
        return Math.Max(MinTargetOf(tier), HeaderHeightOf(tier) - scaledTitleBar);
    }

    /// <summary>
    /// 系统标题栏在 100% 档的高度（DIP），由度量推出而不是写字面量：
    /// compact 外壳 64 − compact 菜单行 24 = 40。
    /// </summary>
    public static int TitleBarBaseDips => Metrics.CompactHeaderHeight - Metrics.CompactTabStripHeight;

    /// <summary>
    /// 实测非客户区高度（DIP）：<c>窗口物理高 ÷ 栅格化比例 − XAML 内容实际高</c>。
    /// </summary>
    /// <remarks>
    /// XAML 的 DIP 与物理像素的唯一换算点就在这里（§8.1）。
    /// 结果 ≤ 0 表示内容还没布局（或读不到），返回 0 让调用方当作「待测」。
    /// </remarks>
    public static double MeasuredNonClientDips(int windowPhysicalHeight, double scale, double contentDips)
    {
        if (windowPhysicalHeight <= 0 || contentDips <= 0)
        {
            return 0;
        }

        var windowDips = PhysicalToDips(windowPhysicalHeight, scale <= 0 ? 1 : scale);
        var nonClient = windowDips - contentDips;
        return nonClient <= 0 ? 0 : nonClient;
    }

    /// <summary>档位的最小窗口宽（<b>未</b>按工作区收敛；可用性守卫用它）。</summary>
    public static int MinWindowWidth(DensityTier tier) => tier switch
    {
        DensityTier.Comfortable => Metrics.ComfortableMinWindowWidth,
        DensityTier.Large => Metrics.LargeMinWindowWidth,
        _ => Metrics.CompactMinWindowWidth,
    };

    /// <summary>档位的最小窗口高（未按工作区收敛）。</summary>
    public static int MinWindowHeight(DensityTier tier) => tier switch
    {
        DensityTier.Comfortable => Metrics.ComfortableMinWindowHeight,
        DensityTier.Large => Metrics.LargeMinWindowHeight,
        _ => Metrics.CompactMinWindowHeight,
    };

    /// <summary>
    /// 最小窗口按工作区收敛：<c>min(min_window_width(T), W-32) × min(min_window_height(T), H-48)</c>。
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

        // 100% 档的下限 = 度量真值里的 min_window_width/height[compact]（不再另写 960/640）。
        var floorWidth = Math.Min(MinWindowWidth(DensityTier.Compact), workWidth - WidthMargin);
        var floorHeight = Math.Min(MinWindowHeight(DensityTier.Compact), workHeight - HeightMargin);
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
    /// 以及需求里的几条硬约束：窗口不超工作区、最小窗口按工作区收敛、
    /// 最小窗口直接等于度量真值、菜单行三档 24 / 30 / 36。
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

        // 最小窗口只有一个口径：就是度量真值（960 / 1200 / 1440 × 640 / 800 / 960），
        // 不再由代码里的基准 × scale 算出来（#168 第 5 条）。
        foreach (var tier in new[] { DensityTier.Compact, DensityTier.Comfortable, DensityTier.Large })
        {
            var expectedWidth = tier switch
            {
                DensityTier.Comfortable => Metrics.ComfortableMinWindowWidth,
                DensityTier.Large => Metrics.LargeMinWindowWidth,
                _ => Metrics.CompactMinWindowWidth,
            };
            var expectedHeight = tier switch
            {
                DensityTier.Comfortable => Metrics.ComfortableMinWindowHeight,
                DensityTier.Large => Metrics.LargeMinWindowHeight,
                _ => Metrics.CompactMinWindowHeight,
            };
            Check(
                $"{tier} 最小窗口 = 度量真值 {expectedWidth}x{expectedHeight}",
                MinWindowWidth(tier) == expectedWidth && MinWindowHeight(tier) == expectedHeight,
                $"{MinWindowWidth(tier)}x{MinWindowHeight(tier)}");
        }

        // 菜单行三档 24 / 30 / 36：与 tab_strip_height 交叉校验（不是拿同一个式子自证）。
        foreach (var tier in new[] { DensityTier.Compact, DensityTier.Comfortable, DensityTier.Large })
        {
            var menuRow = MenuRowHeight(tier);
            Check(
                $"{tier} 菜单行 = tab_strip_height({TabStripHeightOf(tier)})",
                menuRow == TabStripHeightOf(tier),
                $"菜单行 {menuRow}，标签条 {TabStripHeightOf(tier)}");
            Check(
                $"{tier} 菜单行不低于最小点击区",
                menuRow >= MinTargetOf(tier),
                $"菜单行 {menuRow}，最小点击区 {MinTargetOf(tier)}");
        }

        // 非客户区换算（实测用）：物理 2112 / 比例 2 → 1056 DIP；内容 1016 DIP → 非客户区 40。
        Check(
            "非客户区换算 2112px@2x, 内容 1016 DIP → 40",
            Math.Abs(MeasuredNonClientDips(2112, 2, 1016) - 40) < 0.001,
            MeasuredNonClientDips(2112, 2, 1016).ToString("0.##"));

        // 可用性守卫：1080p@150% 的逻辑工作区放不下 125% 档。
        Check("1280x698 上 large 不可用", !TierAvailable(DensityTier.Large, 1280, 698), "应为不可用");
        Check("1280x698 上 comfortable 不可用", !TierAvailable(DensityTier.Comfortable, 1280, 698), "应为不可用");
        Check("1280x698 回退到 compact", ResolveTier(DensityTier.Large, 1280, 698) == DensityTier.Compact,
            ResolveTier(DensityTier.Large, 1280, 698).ToString());
        Check("1920x1032 上 large 可用", TierAvailable(DensityTier.Large, 1920, 1032), "应可用");

        return failures;
    }
}