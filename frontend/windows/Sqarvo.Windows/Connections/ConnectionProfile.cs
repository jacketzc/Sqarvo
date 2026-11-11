using System.Globalization;
using System.Text.Json.Serialization;

namespace Sqarvo.Windows.Connections;

/// <summary>
/// 一条已保存的连接，对应 <c>%LOCALAPPDATA%\Sqarvo\connections.json</c> 里的一项。
/// </summary>
/// <remarks>
/// <para>
/// 字段与 Linux 客户端 <c>frontend/linux/src/config.rs</c> 的 <c>SavedConnection</c> **一一对应**
/// （JSON 名逐字相同，含 <c>version</c> 文档版本），两个平台读写同一份 schema，装机迁移和
/// 手工排查不需要换脑子。
/// </para>
/// <para>
/// 这里**只有**「怎么连」的信息：名称、驱动、主机、端口、库名、用户名、文件路径、只读开关、
/// TLS 模式，以及操作系统凭据库里的引用 <see cref="CredentialRef"/>（形如
/// <c>sqarvo:conn/&lt;uuid-v4&gt;</c>）。密码、token、连接串永远不写进来
/// （AGENTS.md §9；ADR 0012 / 0016）。
/// </para>
/// </remarks>
public sealed record ConnectionProfile
{
    /// <summary>服务器型驱动器 ID（与后端 <c>ListDrivers</c> 的返回值一致）。</summary>
    public const string MariadbDriverId = "mariadb";

    /// <summary>文件型驱动器 ID（SQLite）。</summary>
    public const string SqliteDriverId = "sqlite";

    /// <summary>用户可见名称，例如 <c>sqarvo@db.example.com/sqarvo_demo</c>。</summary>
    [JsonPropertyName("name")]
    public string Name { get; init; } = string.Empty;

    /// <summary>驱动 ID（来自 <c>SystemService.ListDrivers</c>）。</summary>
    [JsonPropertyName("driver")]
    public string Driver { get; init; } = string.Empty;

    /// <summary>主机（服务器型驱动）。</summary>
    [JsonPropertyName("host")]
    public string Host { get; init; } = string.Empty;

    /// <summary>端口（服务器型驱动）。</summary>
    [JsonPropertyName("port")]
    public int Port { get; init; }

    /// <summary>数据库名（服务器型驱动）；留空表示「连上服务器后浏览所有库」。</summary>
    [JsonPropertyName("database")]
    public string Database { get; init; } = string.Empty;

    /// <summary>用户名（服务器型驱动）。</summary>
    [JsonPropertyName("username")]
    public string Username { get; init; } = string.Empty;

    /// <summary>文件型驱动（SQLite）的数据库文件路径；服务器型驱动为空。</summary>
    [JsonPropertyName("file_path")]
    public string FilePath { get; init; } = string.Empty;

    /// <summary>文件型驱动是否以只读方式打开；服务器型驱动为 <see langword="false"/>。</summary>
    [JsonPropertyName("read_only")]
    public bool ReadOnly { get; init; }

    /// <summary>
    /// TLS 模式，取 <c>disabled</c> / <c>preferred</c> / <c>required</c> / <c>verify-ca</c> /
    /// <c>verify-full</c>；与 <c>proto</c> 的 <c>TlsMode</c> 一一对应。
    /// </summary>
    [JsonPropertyName("tls_mode")]
    public string TlsMode { get; init; } = TlsModeChoice.Preferred;

    /// <summary>
    /// 系统凭据库中的引用；无需凭据的驱动为空。**不是**连接身份——同一目标换密码只覆盖
    /// 这个引用指向的密钥，引用本身保持不变（ADR 0012）。
    /// </summary>
    [JsonPropertyName("credential_ref")]
    public string CredentialRef { get; init; } = string.Empty;

    /// <summary>文件型驱动（SQLite）。</summary>
    [JsonIgnore]
    public bool IsFileDriver => string.Equals(Driver, SqliteDriverId, StringComparison.Ordinal);

    /// <summary>
    /// 连接身份键：同一个「连接目标」共享一个键，落盘时按它去重。
    /// </summary>
    /// <remarks>
    /// <list type="bullet">
    /// <item><description>
    /// 服务器型：<c>driver|host|port|username|database</c>。主机名大小写不敏感，库名留空也参与
    /// 比较（<c>db_a</c> 与 <c>db_b</c> 是两条连接，不会互相覆盖）。
    /// </description></item>
    /// <item><description>
    /// 文件型：<c>driver|file|&lt;规范化绝对路径&gt;</c>；相对路径、<c>./</c> 与大小写差异视为
    /// 同一个文件（与 Linux 端同样的取舍：宁可把 <c>A.db</c> 与 <c>a.db</c> 合并，也不让用户
    /// 每点一次就多一条指向同一个库的连接）。
    /// </description></item>
    /// </list>
    /// 这个键**不是**凭据引用：用引用当身份会让同一目标两次保存各生成一个引用，
    /// 配置与凭据库都堆条目（Linux #27 / #145 的结论，这里照做）。
    /// </remarks>
    [JsonIgnore]
    public string Key => IsFileDriver
        ? string.Concat(Driver, "|file|", NormalizeFilePath(FilePath))
        : string.Concat(
            Driver,
            "|",
            Host.ToLowerInvariant(),
            "|",
            Port.ToString(CultureInfo.InvariantCulture),
            "|",
            Username,
            "|",
            Database);

    /// <summary>
    /// 连接目标的展示文案：服务器型 <c>user@host:port/db</c>（无默认库时 <c>user@host:port</c>），
    /// 文件型是文件路径。两个同名连接靠它区分，不靠猜。
    /// </summary>
    [JsonIgnore]
    public string TargetLabel
    {
        get
        {
            if (IsFileDriver)
            {
                return FilePath;
            }

            var endpoint = string.Concat(Host, ":", Port.ToString(CultureInfo.InvariantCulture));
            var user = Username.Length == 0 ? string.Empty : Username + "@";
            return Database.Length == 0
                ? string.Concat(user, endpoint)
                : string.Concat(user, endpoint, "/", Database);
        }
    }

    /// <summary>
    /// 文件型连接的身份路径：补成绝对路径、消掉 <c>.</c> / <c>..</c>、统一分隔符与小写。
    /// </summary>
    /// <remarks>
    /// 这里做**词法**规范化而不是 <c>FileInfo</c> 解析：连接对话框允许输入尚不存在的新库文件，
    /// 解析成真实路径会失败；而且符号链接解析与否不影响「同一路径的两种写法」这个目标。
    /// </remarks>
    /// <param name="path">原始路径。</param>
    /// <returns>规范化后的身份路径（小写、<c>/</c> 分隔）；空输入返回空串。</returns>
    public static string NormalizeFilePath(string path)
    {
        if (string.IsNullOrWhiteSpace(path))
        {
            return string.Empty;
        }

        string full;
        try
        {
            full = Path.GetFullPath(path.Trim());
        }
        catch (Exception error) when (error is ArgumentException or NotSupportedException or PathTooLongException)
        {
            // 取不到当前目录或路径非法时退回原样：至少不会把两条不同文件判成同一个。
            full = path.Trim();
        }

        return full.Replace('\\', '/').TrimEnd('/').ToLowerInvariant();
    }
}