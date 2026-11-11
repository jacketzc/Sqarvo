using Microsoft.UI.Xaml;
using System;

namespace Sqarvo.Windows.Theming;

/// <summary>
/// 把生成的三档度量写成 XAML 资源键（<c>Size.*</c>）。
/// </summary>
/// <remarks>
/// <para>
/// 为什么是「运行时写资源字典」而不是「生成一份 XAML」：XAML 编译发生在生成步骤之前，
/// 生成出来的 xaml 文件不会进入 <c>XamlTypeInfo</c>；而 <see cref="Metrics"/> 里的常量在
/// 编译期就已经存在，写进 <see cref="Application.Resources"/> 后 <c>{StaticResource Size.X}</c>
/// 在页面解析时能解析到。尺寸数字仍然只有一个来源（metrics.toml → 生成器），页面 XAML
/// 里一个尺寸字面量都没有。
/// </para>
/// <para>
/// 写资源的时机只有一个：<c>App</c> 构造里、创建第一个窗口之前。切密度档的做法是
/// 「重建页面」（先 <see cref="Apply"/> 新档位、再重新构造页面），不是指望资源字典改动
/// 会自动让已解析的 <c>StaticResource</c> 重算——WinUI 不会（ADR 0026 决策 5）。
/// </para>
/// </remarks>
internal static class MetricsResources
{
    /// <summary>把当前档位的全部尺寸键写入给定资源字典（覆盖同名键）。</summary>
    public static void Apply(ResourceDictionary target)
        {
            ArgumentNullException.ThrowIfNull(target);
            Metrics.ForEachSizeToken((key, value) => target[key] = value);

            // 1px 描边不随档位缩放，所以它不在三档 token 里；但界面里确实要用同一个 key，
            // 这里按「不缩放的尺寸 token」补一条，避免 XAML 里出现字面量 1。
            target["Size.BorderWidthPxNeverScaled"] = (double)Metrics.BorderWidthPxNeverScaled;
        }

    /// <summary>移除已写入的尺寸键（切档重建前调用，避免旧档位的残留键被 StaticResource 命中）。</summary>
    public static void Clear(ResourceDictionary target)
    {
        ArgumentNullException.ThrowIfNull(target);
        Metrics.ForEachSizeToken((key, _) => target.Remove(key));
    }
}