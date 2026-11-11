using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace Sqarvo.Windows.Connections;

/// <summary>
/// <c>%LOCALAPPDATA%\Sqarvo\connections.json</c> 的读写：非敏感连接配置的唯一落盘入口。
/// </summary>
/// <remarks>
/// <para>
/// 与 Linux 端 <c>frontend/linux/src/config.rs</c> 同一份 schema：
/// <c>{"version": 1, "connections": [ … ]}</c>，字段名逐字一致。
/// </para>
/// <para>
/// <b>读失败不回退到弹窗</b>：文件不存在、JSON 坏了、权限不足，一律返回空列表并把原因留在
/// <see cref="LastLoadNotice"/> 里。连接配置是便利功能，读不到不能挡住用户新建连接；但也不能
/// 假装「读到了」——界面会把这句说明写进状态栏 tooltip，而不是伪造一份列表。
/// </para>
/// <para>
/// <b>写入是「临时文件 + 同目录替换」</b>：进程崩溃不会留下半个 JSON 文件，下一次启动不会因为
/// 写入被打断而丢掉整个连接列表（Linux 端同样的做法，理由相同）。Windows 上
/// <see cref="File.Move(string, string, bool)"/> 走 <c>MoveFileEx(MOVEFILE_REPLACE_EXISTING)</c>，
/// 同一卷内是原子的。
/// </para>
/// <para>
/// 权限：Windows 上 <c>%LOCALAPPDATA%</c> 默认只允许当前用户访问，不再另设 DACL；非 Windows
/// 平台把文件收紧到 0600、目录到 0700（Linux 端的等价语义，测试与本机排查时同样有效）。
/// </para>
/// </remarks>
public sealed class ConnectionStore
{
    /// <summary>配置文件名。</summary>
    public const string FileName = "connections.json";

    /// <summary>当前文档版本；字段增删时提升，并为旧版本保留读取路径。</summary>
    public const int DocumentVersion = 1;

    private static readonly JsonSerializerOptions WriteOptions = new() { WriteIndented = true };

    private static readonly JsonSerializerOptions ReadOptions = new() { PropertyNameCaseInsensitive = true };

    private readonly string _directory;

    /// <summary>使用平台默认目录（<c>%LOCALAPPDATA%\Sqarvo</c>）构造。</summary>
    public ConnectionStore()
        : this(DefaultDirectory())
    {
    }

    /// <summary>用显式目录构造（测试、便携安装）。</summary>
    /// <param name="directory">存放 <c>connections.json</c> 的目录；不存在时首次写入会创建。</param>
    public ConnectionStore(string directory)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(directory);
        _directory = Path.GetFullPath(directory);
    }

    /// <summary>配置目录的绝对路径。</summary>
    public string DirectoryPath => _directory;

    /// <summary><c>connections.json</c> 的绝对路径。</summary>
    public string FilePath => Path.Combine(_directory, FileName);

    /// <summary>
    /// 最近一次 <see cref="Load"/> 的非致命说明（读取失败原因，或「已合并 N 条重复连接」），
    /// 干净读取时为 <see langword="null"/>。文案可直接展示，不含凭据。
    /// </summary>
    public string? LastLoadNotice { get; private set; }

    /// <summary>平台默认配置目录：<c>%LOCALAPPDATA%\Sqarvo</c>。</summary>
    /// <returns>绝对路径（目录可能尚未创建）。</returns>
    public static string DefaultDirectory()
    {
        var local = Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData);
        if (string.IsNullOrEmpty(local))
        {
            // 极少数没有 LocalAppData 的会话（服务账号、受限容器）退回 HOME 下的 AppData\Local。
            local = Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.UserProfile),
                "AppData",
                "Local");
        }

        return Path.Combine(local, "Sqarvo");
    }

    /// <summary>
    /// 读取全部已保存连接。**不抛异常**：任何故障都退化成空列表。
    /// </summary>
    /// <returns>按文件顺序排列的连接；同一身份出现多次时只保留最后一条（最近一次保存）。</returns>
    public IReadOnlyList<ConnectionProfile> Load()
    {
        LastLoadNotice = null;
        var path = FilePath;

        string text;
        try
        {
            if (!File.Exists(path))
            {
                return Array.Empty<ConnectionProfile>();
            }

            text = File.ReadAllText(path);
        }
        catch (Exception error) when (error is IOException
            or UnauthorizedAccessException
            or NotSupportedException
            or System.Security.SecurityException)
        {
            LastLoadNotice = $"连接配置无法读取（{error.Message}）：{path}；本次按空列表处理，不会覆盖该文件";
            return Array.Empty<ConnectionProfile>();
        }

        Document? document;
        try
        {
            document = JsonSerializer.Deserialize<Document>(text, ReadOptions);
        }
        catch (JsonException error)
        {
            LastLoadNotice =
                $"连接配置不是合法 JSON（{error.Message}）：{path}；本次按空列表处理，不会覆盖已有文件";
            return Array.Empty<ConnectionProfile>();
        }

        if (document?.Connections is not { Count: > 0 } connections)
        {
            return Array.Empty<ConnectionProfile>();
        }

        var (merged, removed) = MergeDuplicates(connections);
        if (removed > 0)
        {
            // 不静默丢数据：旧版本可能已经写进重复条目。
            LastLoadNotice = $"已合并 {removed} 条重复连接（保留最近一次保存的条目）：{path}";
        }

        return merged;
    }

    /// <summary>按连接身份查找已保存的条目（找到最近一次保存的那条）。</summary>
    /// <param name="candidate">只用身份字段做比较的条目；它的其它字段不参与匹配。</param>
    /// <returns>命中的条目，或 <see langword="null"/>。</returns>
    public ConnectionProfile? FindByIdentity(ConnectionProfile candidate)
    {
        ArgumentNullException.ThrowIfNull(candidate);
        var key = candidate.Key;
        var connections = Load();
        for (var index = connections.Count - 1; index >= 0; index--)
        {
            if (string.Equals(connections[index].Key, key, StringComparison.Ordinal))
            {
                return connections[index];
            }
        }

        return null;
    }

    /// <summary>
    /// 插入或更新一条连接，按 <see cref="ConnectionProfile.Key"/> 去重。
    /// </summary>
    /// <param name="profile">要保存的条目。</param>
    /// <returns>真正落盘的条目（命中身份且新条目没带凭据引用时，沿用原条目的引用）。</returns>
    /// <remarks>
    /// 命中身份时只更新非身份字段（名称 / TLS / 引用），不追加重复项；改库名、用户名或端口属于
    /// 另一个目标，各自独立成条。凭据引用由调用方在写入密码**之前**按身份复用
    /// （见 <see cref="FindByIdentity"/>），否则同一连接会在凭据库里堆多份密码。
    /// </remarks>
    /// <exception cref="IOException">目录或文件无法写入。</exception>
    /// <exception cref="UnauthorizedAccessException">没有写入权限。</exception>
    public ConnectionProfile Upsert(ConnectionProfile profile)
    {
        ArgumentNullException.ThrowIfNull(profile);

        var connections = new List<ConnectionProfile>(Load());
        var key = profile.Key;
        var index = connections.FindIndex(existing => string.Equals(existing.Key, key, StringComparison.Ordinal));

        ConnectionProfile stored;
        if (index >= 0)
        {
            var reference = string.IsNullOrEmpty(profile.CredentialRef)
                ? connections[index].CredentialRef
                : profile.CredentialRef;
            stored = profile with { CredentialRef = reference };
            connections[index] = stored;
        }
        else
        {
            stored = profile;
            connections.Add(profile);
        }

        WriteAtomic(new Document { Version = DocumentVersion, Connections = connections });
        return stored;
    }

    /// <summary>按连接身份删除一条已保存连接。</summary>
    /// <param name="identity">要删除的身份键（<see cref="ConnectionProfile.Key"/>）。</param>
    /// <returns>是否真的删掉了条目。</returns>
    /// <remarks>
    /// 没命中时**不重写文件**（权限、mtime 与内容都不动），免得一次误点的「删除」把一份只是
    /// 没命中的配置重新序列化一遍。
    /// </remarks>
    /// <exception cref="IOException">文件无法写入。</exception>
    /// <exception cref="UnauthorizedAccessException">没有写入权限。</exception>
    public bool RemoveByKey(string identity)
    {
        if (string.IsNullOrEmpty(identity) || !File.Exists(FilePath))
        {
            return false;
        }

        var connections = new List<ConnectionProfile>(Load());
        var before = connections.Count;
        connections.RemoveAll(item => string.Equals(item.Key, identity, StringComparison.Ordinal));
        if (connections.Count == before)
        {
            return false;
        }

        WriteAtomic(new Document { Version = DocumentVersion, Connections = connections });
        return true;
    }

    /// <summary>按身份合并重复条目，保留**最后一条**（追加写入在末尾，最后一条即最近一次保存）。</summary>
    private static (List<ConnectionProfile> Connections, int Removed) MergeDuplicates(
        List<ConnectionProfile> connections)
    {
        var merged = new List<ConnectionProfile>(connections.Count);
        var removed = 0;
        foreach (var connection in connections)
        {
            var key = connection.Key;
            var index = merged.FindIndex(existing => string.Equals(existing.Key, key, StringComparison.Ordinal));
            if (index >= 0)
            {
                merged.RemoveAt(index);
                removed++;
            }

            merged.Add(connection);
        }

        return (merged, removed);
    }

    /// <summary>写临时文件（同目录）→ 刷盘 → 替换目标，并在非 Windows 平台收紧权限。</summary>
    private void WriteAtomic(Document document)
    {
        Directory.CreateDirectory(_directory);
        RestrictDirectory(_directory);

        var path = FilePath;
        var temporary = Path.Combine(_directory, "." + FileName + ".tmp");
        var bytes = new UTF8Encoding(encoderShouldEmitUTF8Identifier: false)
            .GetBytes(JsonSerializer.Serialize(document, WriteOptions));

        using (var stream = new FileStream(temporary, FileMode.Create, FileAccess.Write, FileShare.None))
        {
            stream.Write(bytes, 0, bytes.Length);
            stream.Flush(flushToDisk: true);
        }

        RestrictFile(temporary);
        File.Move(temporary, path, overwrite: true);
        RestrictFile(path);
    }

    private static void RestrictDirectory(string path)
    {
        if (OperatingSystem.IsWindows())
        {
            return;
        }

        try
        {
            File.SetUnixFileMode(
                path,
                UnixFileMode.UserRead | UnixFileMode.UserWrite | UnixFileMode.UserExecute);
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or PlatformNotSupportedException)
        {
            // 权限收紧是加固而不是功能：失败时不影响写入，也不打断用户。
        }
    }

    private static void RestrictFile(string path)
    {
        if (OperatingSystem.IsWindows())
        {
            return;
        }

        try
        {
            File.SetUnixFileMode(path, UnixFileMode.UserRead | UnixFileMode.UserWrite);
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or PlatformNotSupportedException)
        {
        }
    }

    /// <summary>配置文件结构；<c>version</c> 始终写出，读取时不因未知版本拒绝（向前兼容）。</summary>
    private sealed class Document
    {
        [JsonPropertyName("version")]
        public int Version { get; set; }

        [JsonPropertyName("connections")]
        public List<ConnectionProfile> Connections { get; set; } = new();
    }
}