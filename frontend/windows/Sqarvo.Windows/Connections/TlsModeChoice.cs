namespace Sqarvo.Windows.Connections;

/// <summary>
/// TLS 模式：配置文件里的稳定标识 + 下拉框标签（对应 <c>proto</c> 的 <c>TlsMode</c>）。
/// </summary>
/// <remarks>
/// 为什么不是布尔「开关」：<c>connections.json</c> 的 <c>tls_mode</c> 有五种取值，契约里同样有
/// 五档。用一个布尔值表示就必须发明一套映射，保存一次就会把用户选的
/// <c>verify-full</c> 悄悄降级成别的档位——配置文件是给人看的，不能写一个和界面不一致的值。
/// </remarks>
/// <param name="Value">配置文件里的稳定标识。</param>
/// <param name="Label">下拉框展示文案。</param>
public sealed record TlsModeChoice(string Value, string Label)
{
    /// <summary>不加密。</summary>
    public const string Disabled = "disabled";

    /// <summary>服务端支持则加密（默认）。</summary>
    public const string Preferred = "preferred";

    /// <summary>必须加密。</summary>
    public const string Required = "required";

    /// <summary>加密并校验 CA。</summary>
    public const string VerifyCa = "verify-ca";

    /// <summary>加密、校验 CA 与主机名。</summary>
    public const string VerifyFull = "verify-full";

    /// <summary>
    /// 驱动能力串：声明该驱动的 <c>verify-ca</c> **只校验证书链、不校验主机名**。
    /// </summary>
    /// <remarks>
    /// 由后端 <c>Capabilities::names()</c> 输出，经 <c>SystemService.ListDrivers</c> 传到前端。
    /// 缺这个串的驱动会把 <c>verify-ca</c> 退化成 <c>verify-full</c>（仍校验主机名），
    /// 界面必须显式标注，不能静默（Linux 端同样的规则）。
    /// </remarks>
    public const string VerifyCaChainOnlyCapability = "tls_verify_ca_chain_only";

    /// <summary>全部档位，顺序与下拉框一致。</summary>
    /// <param name="chainOnlyVerifyCa">驱动是否声明了 <see cref="VerifyCaChainOnlyCapability"/>。</param>
    /// <returns>五档选项；<c>verify-ca</c> 的标签按驱动能力如实标注是否等同 <c>verify-full</c>。</returns>
    public static IReadOnlyList<TlsModeChoice> ChoicesFor(bool chainOnlyVerifyCa) => new[]
    {
        new TlsModeChoice(Disabled, "禁用（disabled）"),
        new TlsModeChoice(Preferred, "优先（preferred）"),
        new TlsModeChoice(Required, "必需（required）"),
        new TlsModeChoice(
            VerifyCa,
            chainOnlyVerifyCa
                ? "校验 CA（verify-ca）"
                : "校验 CA（verify-ca，本驱动等同 verify-full）"),
        new TlsModeChoice(VerifyFull, "完整校验（verify-full）"),
    };

    /// <summary>默认为 <c>preferred</c> 的档位列表（无驱动上下文时使用）。</summary>
    public static IReadOnlyList<TlsModeChoice> All { get; } = ChoicesFor(chainOnlyVerifyCa: false);

    /// <summary>把任意字符串收敛到合法档位；未知值回退 <c>preferred</c>（proto 默认语义），不猜。</summary>
    /// <param name="value">配置文件或调用方给的字符串。</param>
    /// <returns>五个合法值之一。</returns>
    public static string Normalize(string value)
    {
        foreach (var choice in All)
        {
            if (string.Equals(choice.Value, value, StringComparison.Ordinal))
            {
                return choice.Value;
            }
        }

        return Preferred;
    }

    /// <summary>按 <paramref name="value"/> 找下拉框序号；未知值返回 <c>preferred</c> 的序号。</summary>
    /// <param name="value">档位标识。</param>
    /// <param name="chainOnlyVerifyCa">驱动是否声明只校链（只影响标签，不影响序号）。</param>
    /// <returns>0 起的序号。</returns>
    public static int IndexOf(string value, bool chainOnlyVerifyCa = false)
    {
        var choices = ChoicesFor(chainOnlyVerifyCa);
        for (var index = 0; index < choices.Count; index++)
        {
            if (string.Equals(choices[index].Value, value, StringComparison.Ordinal))
            {
                return index;
            }
        }

        return IndexOf(Preferred);
    }

    /// <summary>驱动是否声明了「verify-ca 只校链、不校名」。</summary>
    /// <param name="capabilities">驱动能力串。</param>
    /// <returns><see langword="true"/> 时 <c>verify-ca</c> 真的不校验主机名。</returns>
    public static bool SupportsChainOnlyVerifyCa(IReadOnlyList<string>? capabilities)
    {
        if (capabilities is null)
        {
            return false;
        }

        foreach (var capability in capabilities)
        {
            if (string.Equals(capability, VerifyCaChainOnlyCapability, StringComparison.Ordinal))
            {
                return true;
            }
        }

        return false;
    }
}