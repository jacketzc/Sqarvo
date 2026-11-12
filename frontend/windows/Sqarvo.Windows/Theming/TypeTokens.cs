using Windows.UI.Text;

namespace Sqarvo.Windows.Theming;

/// <summary>
/// 字重四档：唯一来源是 <see cref="Metrics"/>（metrics.toml 的 <c>font_weight_*</c>）。
/// </summary>
/// <remarks>
/// <para>
/// XAML 的 <c>FontWeight</c> 属性吃 <see cref="FontWeight"/> 结构体，而度量生成物里的
/// <c>Metrics.FontWeight*</c> 是 <see cref="int"/>（400 / 500 / 600 / 700）。所以不能直接
/// <c>{StaticResource Size.FontWeightTitle}</c>——那是一个 <see cref="double"/> 尺寸键。
/// 这里把四档包成 <see cref="FontWeight"/>，两处消费：
/// </para>
/// <list type="bullet">
///   <item><description>XAML：<see cref="MetricsResources"/> 在启动时把它们注册成
///     <c>Weight.Ui / Cell / Title / Heading</c> 资源，页面写
///     <c>FontWeight="{StaticResource Weight.Title}"</c>；</description></item>
///   <item><description>code-behind：控件树里直接取 <see cref="Ui"/> / <see cref="Cell"/> /
///     <see cref="Title"/> / <see cref="Heading"/>。</description></item>
/// </list>
/// <para>
/// 每档都按 <see cref="Metrics"/> 的当前档位现取（字重不随密度变，但这样不会因为
/// 静态字段在切档前初始化而冻住旧值）。
/// </para>
/// </remarks>
internal static class TypeTokens
{
    /// <summary>正文 / 按钮 / 说明字（font_weight_ui）。</summary>
    public static FontWeight Ui => From(Metrics.FontWeightUi);

    /// <summary>数据单元格（font_weight_cell）。</summary>
    public static FontWeight Cell => From(Metrics.FontWeightCell);

    /// <summary>列头 / 页内标题（font_weight_title）。</summary>
    public static FontWeight Title => From(Metrics.FontWeightTitle);

    /// <summary>卡片 · 面板 · 对象树连接与分组名（font_weight_heading）。</summary>
    public static FontWeight Heading => From(Metrics.FontWeightHeading);

    private static FontWeight From(int weight) => new() { Weight = (ushort)weight };
}