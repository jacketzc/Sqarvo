using System.Globalization;
using ProtoTlsMode = Sqarvo.Services.V1.TlsMode;
using ConnectionParams = Sqarvo.Services.V1.ConnectionParams;

namespace Sqarvo.Windows.Connections;

/// <summary>驱动族：决定「哪些字段有意义、怎么进 <c>ConnectionParams</c>」。</summary>
public enum DriverFamily
{
    /// <summary>服务器型（MariaDB）：主机 / 端口 / 库 / 用户 / TLS / 密码。</summary>
    Server,

    /// <summary>文件型（SQLite）：文件路径 + 只读开关。</summary>
    File,

    /// <summary>前端还不认识的驱动：不猜字段，如实说明没有表单。</summary>
    Unknown,
}

/// <summary>连接对话框里读出来的原始输入（纯数据，便于脱离 WinUI 校验）。</summary>
/// <remarks>
/// 全部按字符串保存：端口由校验层解析，空串保持「用户没填」的语义，不在这里填充默认值——
/// 默认值属于界面（新建时的初值）。
/// </remarks>
public sealed record ConnectionInput
{
    /// <summary>下拉里选中的驱动 ID。</summary>
    public string DriverId { get; init; } = string.Empty;

    /// <summary>主机。</summary>
    public string Host { get; init; } = string.Empty;

    /// <summary>端口（文本，便于校验「不是数字」）。</summary>
    public string Port { get; init; } = string.Empty;

    /// <summary>数据库名（可留空）。</summary>
    public string Database { get; init; } = string.Empty;

    /// <summary>用户名。</summary>
    public string Username { get; init; } = string.Empty;

    /// <summary>本次输入的密码明文；只在内存里存在，绝不写配置/日志。</summary>
    public string Password { get; init; } = string.Empty;

    /// <summary>TLS 档位标识。</summary>
    public string TlsMode { get; init; } = TlsModeChoice.Preferred;

    /// <summary>已保存的凭据引用（改密码时为新值，不改时复用）；没有时为 <see langword="null"/>。</summary>
    public string? CredentialRef { get; init; }

    /// <summary>打印时不带密码（避免日志/崩溃报告里出现明文）。</summary>
    /// <returns>脱敏后的描述。</returns>
    public override string ToString() =>
        $"ConnectionInput {{ Driver = {DriverId}, Host = {Host}, Port = {Port}, Database = {Database}, "
        + $"Username = {Username}, TlsMode = {TlsMode}, Password = <redacted> }}";
}

/// <summary>校验结果：要么给出可落盘的条目，要么给出可直接展示的中文原因。</summary>
/// <param name="Profile">校验通过时的条目（不含密码）。</param>
/// <param name="Error">校验失败时可直接展示的原因。</param>
public sealed record ConnectionValidation(ConnectionProfile? Profile, string? Error)
{
    /// <summary>是否通过。</summary>
    public bool IsValid => Profile is not null;
}

/// <summary>
/// 连接表单的「语义层」：字段校验、默认会话名、<c>ConnectionParams</c> 映射。
/// </summary>
/// <remarks>
/// 只覆盖 MariaDB（服务器型）这一族；SQLite 的文件选择与只读开关属于后续工作包，这里如实说明
/// 「没有表单」，而不是给一份填了也会被后端拒绝的表单（与 Linux <c>driver_form</c> 同一规则）。
/// </remarks>
public static class ConnectionForm
{
    /// <summary>驱动 ID → 驱动族。</summary>
    /// <param name="driverId">驱动 ID（来自 <c>ListDrivers</c>）。</param>
    /// <returns>驱动族；不认识的驱动为 <see cref="DriverFamily.Unknown"/>。</returns>
    public static DriverFamily FamilyFor(string driverId) => driverId switch
    {
        ConnectionProfile.MariadbDriverId => DriverFamily.Server,
        ConnectionProfile.SqliteDriverId => DriverFamily.File,
        _ => DriverFamily.Unknown,
    };

    /// <summary>按驱动族校验输入并组装条目。</summary>
    /// <remarks>
    /// <list type="bullet">
    /// <item><description>库名**不是**必填：留空表示「连上服务器后浏览所有库」，原样保留空串。</description></item>
    /// <item><description>密码不 trim：前后空格可能是密码的一部分。</description></item>
    /// <item><description>已有凭据引用时密码可留空（复用已保存的密钥）。</description></item>
    /// <item><description>条目里**不含**密码：密码只经 <c>CredentialService.StoreCredential</c> 进系统凭据库。</description></item>
    /// </list>
    /// </remarks>
    /// <param name="input">对话框读数。</param>
    /// <returns>通过时给出条目，失败时给出原因。</returns>
    public static ConnectionValidation Validate(ConnectionInput input)
    {
        ArgumentNullException.ThrowIfNull(input);

        var family = FamilyFor(input.DriverId);
        if (family == DriverFamily.Unknown)
        {
            return new ConnectionValidation(
                null,
                $"此版本前端没有驱动「{input.DriverId}」的连接表单（后端已注册该驱动）：请升级前端，或换一个驱动");
        }

        if (family == DriverFamily.File)
        {
            return new ConnectionValidation(
                null,
                "此版本只提供 MariaDB 连接表单：文件型驱动（SQLite）的连接表单随后续工作包提供");
        }

        var host = input.Host.Trim();
        if (host.Length == 0)
        {
            return new ConnectionValidation(null, "请填写主机");
        }

        var portText = input.Port.Trim();
        if (!ushort.TryParse(portText, NumberStyles.Integer, CultureInfo.InvariantCulture, out var port)
            || port == 0)
        {
            return new ConnectionValidation(null, $"端口必须是 1-65535 之间的数字（当前：{portText}）");
        }

        var username = input.Username.Trim();
        if (username.Length == 0)
        {
            return new ConnectionValidation(null, "请填写用户名");
        }

        var reference = string.IsNullOrWhiteSpace(input.CredentialRef) ? string.Empty : input.CredentialRef;
        if (input.Password.Length == 0 && reference.Length == 0)
        {
            return new ConnectionValidation(null, "请输入密码");
        }

        var database = input.Database.Trim();
        var profile = new ConnectionProfile
        {
            Name = DefaultName(host, username, database),
            Driver = input.DriverId,
            Host = host,
            Port = port,
            Database = database,
            Username = username,
            TlsMode = TlsModeChoice.Normalize(input.TlsMode),
            CredentialRef = reference,
        };

        return new ConnectionValidation(profile, null);
    }

    /// <summary>
    /// 默认会话名：<c>用户@主机/库</c>；没有默认库时是 <c>用户@主机</c>。
    /// </summary>
    /// <remarks>
    /// 没有默认库时**不拼斜杠**：<c>user@host/</c> 这种残缺名字看不出「库为空」，只会让用户以为
    /// 库名丢了（与 Linux 端同一条规则）。
    /// </remarks>
    /// <param name="host">主机。</param>
    /// <param name="username">用户名。</param>
    /// <param name="database">库名，可为空。</param>
    /// <returns>会话显示名。</returns>
    public static string DefaultName(string host, string username, string database)
    {
        var trimmed = database.Trim();
        return trimmed.Length == 0 ? $"{username}@{host}" : $"{username}@{host}/{trimmed}";
    }

    /// <summary>把条目映射成 gRPC 连接参数（密码不在其中，只有 <c>credential_ref</c>）。</summary>
    /// <param name="profile">已通过校验、且已带上凭据引用的条目。</param>
    /// <returns>可直接交给 <c>ConnectionService</c> 的参数。</returns>
    public static ConnectionParams ToParams(ConnectionProfile profile)
    {
        ArgumentNullException.ThrowIfNull(profile);

        return new ConnectionParams
        {
            DriverId = profile.Driver,
            Host = profile.Host,
            Port = (uint)profile.Port,
            Database = profile.Database,
            Username = profile.Username,
            CredentialRef = profile.CredentialRef,
            TlsMode = ToProto(profile.TlsMode),
        };
    }

    /// <summary>配置文件里的档位标识 → proto 枚举；未知值按 <c>preferred</c>（proto 默认语义）。</summary>
    /// <param name="value">档位标识。</param>
    /// <returns>proto 枚举值。</returns>
    public static ProtoTlsMode ToProto(string value) => value switch
    {
        TlsModeChoice.Disabled => ProtoTlsMode.Disabled,
        TlsModeChoice.Required => ProtoTlsMode.Required,
        TlsModeChoice.VerifyCa => ProtoTlsMode.VerifyCa,
        TlsModeChoice.VerifyFull => ProtoTlsMode.VerifyFull,
        _ => ProtoTlsMode.Preferred,
    };
}