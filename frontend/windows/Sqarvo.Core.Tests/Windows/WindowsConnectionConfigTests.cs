using System.Text.Json;
using Grpc.Core;
using VersionInfo = Sqarvo.Core.Model.VersionInfo;
using Sqarvo.Core.Errors;
using Sqarvo.Core.Model;
using Sqarvo.Core.Tests.Support;
using Sqarvo.Windows.Connections;
using Sqarvo.Windows.Workspace;

namespace Sqarvo.Core.Tests.Windows;

/// <summary>
/// Windows 前端连接面的纯逻辑测试（#169）：<c>connections.json</c> 的 schema 与去重语义、表单
/// 校验、错误分类、状态栏文案。
/// </summary>
/// <remarks>
/// 被测源码来自 <c>frontend/windows/Sqarvo.Windows</c>（由测试工程以源码方式编入，见 csproj）：
/// 这些文件不引用任何 WinUI 类型，所以在 Linux 上能真跑。XAML 与 code-behind 不在覆盖范围内。
/// </remarks>
public sealed class WindowsConnectionConfigTests
{
    private const string Reference = "sqarvo:conn/3f9a1c4e-7b2d-4a6f-9c11-0d2b8e5a71f4";

    private static ConnectionProfile SampleProfile() => new()
    {
        Name = "sqarvo@db.example.com/sqarvo_demo",
        Driver = "mariadb",
        Host = "db.example.com",
        Port = 3307,
        Database = "sqarvo_demo",
        Username = "sqarvo",
        TlsMode = "preferred",
        CredentialRef = Reference,
    };

    [Fact]
    public void StoreWritesTheSchemaSharedWithTheLinuxClient()
    {
        using var directory = new TempDirectory("sqarvo-win-store-");
        var store = new ConnectionStore(directory.Path);

        store.Upsert(SampleProfile());

        using var document = JsonDocument.Parse(File.ReadAllText(store.FilePath));
        var root = document.RootElement;
        Assert.Equal(1, root.GetProperty("version").GetInt32());

        var entry = root.GetProperty("connections")[0];
        foreach (var name in new[]
                 {
                     "name", "driver", "host", "port", "database", "username",
                     "file_path", "read_only", "tls_mode", "credential_ref",
                 })
        {
            Assert.True(entry.TryGetProperty(name, out _), $"配置里缺少字段 {name}");
        }

        var text = root.GetRawText();
        foreach (var forbidden in new[] { "password", "secret", "token", "not-a-real-secret" })
        {
            Assert.DoesNotContain(forbidden, text, StringComparison.OrdinalIgnoreCase);
        }
    }

    [Fact]
    public void StoreDeduplicatesByIdentityAndKeepsTheCredentialReference()
    {
        using var directory = new TempDirectory("sqarvo-win-store-");
        var store = new ConnectionStore(directory.Path);

        store.Upsert(SampleProfile());
        var stored = store.Upsert(SampleProfile() with { Name = "改过的名字", TlsMode = "required", CredentialRef = string.Empty });

        Assert.Equal(Reference, stored.CredentialRef);

        var loaded = store.Load();
        Assert.Single(loaded);
        Assert.Equal("改过的名字", loaded[0].Name);
        Assert.Equal("required", loaded[0].TlsMode);
        Assert.Equal(Reference, loaded[0].CredentialRef);
    }

    [Fact]
    public void StoreKeepsDifferentTargetsSeparately()
    {
        using var directory = new TempDirectory("sqarvo-win-store-");
        var store = new ConnectionStore(directory.Path);

        store.Upsert(SampleProfile());
        store.Upsert(SampleProfile() with { Database = "another_db" });
        store.Upsert(SampleProfile() with { Username = "another_user" });
        store.Upsert(SampleProfile() with { Port = 3306 });
        store.Upsert(SampleProfile() with { Host = "LOCALHOST" }); // 大小写不同于原主机 → 新目标

        Assert.Equal(5, store.Load().Count);
        Assert.Equal(5, store.Load().Select(profile => profile.Key).Distinct(StringComparer.Ordinal).Count());
    }

    [Fact]
    public void HostIsCaseInsensitiveButEmptyDatabaseStillCounts()
    {
        var upper = SampleProfile() with { Host = "db.example.com".ToUpperInvariant() };
        var lower = SampleProfile();
        Assert.Equal(lower.Key, upper.Key);

        var withoutDatabase = SampleProfile() with { Database = string.Empty };
        Assert.NotEqual(lower.Key, withoutDatabase.Key);
    }

    [Fact]
    public void StoreMergesDuplicateEntriesOnReadKeepingTheLastOne()
    {
        using var directory = new TempDirectory("sqarvo-win-store-");
        var path = directory.Combine("connections.json");
        var older = SampleProfile() with { Name = "旧的" };
        var newer = SampleProfile() with { Name = "新的", CredentialRef = "sqarvo:conn/0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d" };
        var body = $$"""
        {
          "version": 1,
          "connections": [{{JsonSerializer.Serialize(older)}},{{JsonSerializer.Serialize(newer)}}]
        }
        """;
        directory.WriteFile("connections.json", body);

        var store = new ConnectionStore(directory.Path);
        var loaded = store.Load();

        Assert.Single(loaded);
        Assert.Equal("新的", loaded[0].Name);
        Assert.Contains("重复连接", store.LastLoadNotice);
        Assert.True(File.Exists(path));
    }

    [Fact]
    public void StoreFallsBackToAnEmptyListOnACorruptFileWithoutOverwritingIt()
    {
        using var directory = new TempDirectory("sqarvo-win-store-");
        var path = directory.WriteFile("connections.json", "{ not json");

        var store = new ConnectionStore(directory.Path);
        var loaded = store.Load();

        Assert.Empty(loaded);
        Assert.False(string.IsNullOrWhiteSpace(store.LastLoadNotice));
        Assert.Equal("{ not json", File.ReadAllText(path));
    }

    [Fact]
    public void MissingFileReadsAsEmptyWithoutANotice()
    {
        using var directory = new TempDirectory("sqarvo-win-store-");

        var store = new ConnectionStore(Path.Combine(directory.Path, "does-not-exist"));

        Assert.Empty(store.Load());
        Assert.Null(store.LastLoadNotice);
        Assert.False(Directory.Exists(store.DirectoryPath));
    }

    [Fact]
    public void UpsertLeavesNoTemporaryFileBehind()
    {
        using var directory = new TempDirectory("sqarvo-win-store-");

        var store = new ConnectionStore(directory.Path);
        store.Upsert(SampleProfile());

        var files = Directory.GetFiles(directory.Path).Select(Path.GetFileName).ToArray();
        Assert.Equal(new[] { ConnectionStore.FileName }, files);
    }

    [Fact]
    public void RemoveByKeyDeletesOnlyTheNamedConnectionAndLeavesNoMatchUntouched()
    {
        using var directory = new TempDirectory("sqarvo-win-store-");
        var store = new ConnectionStore(directory.Path);
        store.Upsert(SampleProfile());
        store.Upsert(SampleProfile() with { Database = "keep_me" });

        Assert.True(store.RemoveByKey(SampleProfile().Key));
        var remaining = store.Load();
        Assert.Single(remaining);
        Assert.Equal("keep_me", remaining[0].Database);

        var before = File.ReadAllText(store.FilePath);
        Assert.False(store.RemoveByKey("sqlite|file|/nowhere.db"));
        Assert.Equal(before, File.ReadAllText(store.FilePath));
    }

    [Fact]
    public void FindByIdentityReturnsTheMostRecentEntry()
    {
        using var directory = new TempDirectory("sqarvo-win-store-");
        var store = new ConnectionStore(directory.Path);
        store.Upsert(SampleProfile());
        store.Upsert(SampleProfile() with { Name = "最近一次", CredentialRef = "sqarvo:conn/11111111-2222-4333-8444-555555555555" });

        var found = store.FindByIdentity(SampleProfile());

        Assert.NotNull(found);
        Assert.Equal("最近一次", found!.Name);
        Assert.Null(store.FindByIdentity(SampleProfile() with { Database = "elsewhere" }));
    }

    [Fact]
    public void FileDriverPathsThatNormalizeToTheSameFileShareOneIdentity()
    {
        Assert.Equal(
            ConnectionProfile.NormalizeFilePath("./a/../a.db"),
            ConnectionProfile.NormalizeFilePath("a.db"));

        var first = new ConnectionProfile { Driver = "sqlite", FilePath = "/tmp/a.db" };
        var second = new ConnectionProfile { Driver = "sqlite", FilePath = "/tmp/./A.DB" };

        Assert.Equal(first.Key, second.Key);
        Assert.StartsWith("sqlite|file|", first.Key, StringComparison.Ordinal);
    }

    [Fact]
    public void FormValidationRejectsUnusableInputAndKeepsUsableDefaults()
    {
        static ConnectionInput Valid() => new()
        {
            DriverId = "mariadb",
            Host = "db.example.com",
            Port = "3307",
            Database = "sqarvo_demo",
            Username = "sqarvo",
            Password = "placeholder-not-a-real-secret",
        };

        var accepted = ConnectionForm.Validate(Valid());
        Assert.True(accepted.IsValid, accepted.Error);
        Assert.Equal("sqarvo@db.example.com/sqarvo_demo", accepted.Profile!.Name);
        Assert.Equal(ConnectionForm.Validate(Valid()).Profile!.Key, accepted.Profile.Key);

        Assert.Equal("请填写主机", ConnectionForm.Validate(Valid() with { Host = "   " }).Error);
        Assert.Equal("请填写用户名", ConnectionForm.Validate(Valid() with { Username = string.Empty }).Error);
        Assert.Equal("请输入密码", ConnectionForm.Validate(Valid() with { Password = string.Empty }).Error);
        Assert.Contains("端口", ConnectionForm.Validate(Valid() with { Port = "0" }).Error!, StringComparison.Ordinal);
        Assert.Contains("端口", ConnectionForm.Validate(Valid() with { Port = "3306a" }).Error!, StringComparison.Ordinal);

        // 空库名是合法值（连上服务器再挑库），不能补成别的库名。
        var withoutDatabase = ConnectionForm.Validate(Valid() with { Database = "  " });
        Assert.True(withoutDatabase.IsValid);
        Assert.Equal(string.Empty, withoutDatabase.Profile!.Database);
        Assert.Equal("sqarvo@db.example.com", withoutDatabase.Profile.Name);

        // 已保存凭据时密码可空。
        Assert.True(ConnectionForm.Validate(Valid() with { Password = string.Empty, CredentialRef = Reference }).IsValid);

        // 前端不认识的驱动 / 文件型驱动：如实拒绝，不猜字段。
        Assert.Contains("没有驱动", ConnectionForm.Validate(Valid() with { DriverId = "postgresql" }).Error!, StringComparison.Ordinal);
        Assert.Contains("SQLite", ConnectionForm.Validate(Valid() with { DriverId = "sqlite" }).Error!, StringComparison.Ordinal);
    }

    [Fact]
    public void FormInputNeverCarriesThePasswordIntoParameters()
    {
        var validation = ConnectionForm.Validate(new ConnectionInput
        {
            DriverId = "mariadb",
            Host = "127.0.0.1",
            Port = "3306",
            Username = "sqarvo",
            Password = "placeholder-not-a-real-secret",
            CredentialRef = Reference,
        });

        Assert.True(validation.IsValid, validation.Error);

        var parameters = ConnectionForm.ToParams(validation.Profile!);
        Assert.Equal(Reference, parameters.CredentialRef);
        Assert.Equal("mariadb", parameters.DriverId);
        Assert.Equal(3306u, parameters.Port);
        Assert.Equal(Sqarvo.Services.V1.TlsMode.Preferred, parameters.TlsMode);

        // 结构里根本没有密码字段；序列化后也不能出现明文。
        var text = validation.Profile!.ToString();
        Assert.DoesNotContain("placeholder-not-a-real-secret", text, StringComparison.Ordinal);
        Assert.DoesNotContain("placeholder-not-a-real-secret", new ConnectionInput
        {
            Password = "placeholder-not-a-real-secret",
        }.ToString(), StringComparison.Ordinal);
    }

    [Fact]
    public void TlsChoiceDeclaresVerifyCaDegradationUnlessTheDriverSupportsChainOnly()
    {
        Assert.Equal(5, TlsModeChoice.ChoicesFor(chainOnlyVerifyCa: false).Count);
        Assert.Contains(
            TlsModeChoice.ChoicesFor(chainOnlyVerifyCa: false),
            choice => choice.Value == TlsModeChoice.VerifyCa && choice.Label.Contains("等同 verify-full", StringComparison.Ordinal));
        Assert.Contains(
            TlsModeChoice.ChoicesFor(TlsModeChoice.SupportsChainOnlyVerifyCa(new[] { TlsModeChoice.VerifyCaChainOnlyCapability })),
            choice => choice.Value == TlsModeChoice.VerifyCa && !choice.Label.Contains("等同", StringComparison.Ordinal));

        Assert.Equal("preferred", TlsModeChoice.Normalize("bogus"));
        Assert.Equal(4, TlsModeChoice.IndexOf(TlsModeChoice.VerifyFull));
    }

    [Fact]
    public void FailuresAreClassifiedInsteadOfCollapsedIntoOneMessage()
    {
        var authentication = SqarvoFailure.FromException(new SqarvoClientException(
            SqarvoErrorKind.Unauthenticated,
            "ConnectionService.TestConnection",
            "认证失败",
            StatusCode.Unauthenticated,
            "mariadb.auth_failed"));

        var missingCredential = SqarvoFailure.FromException(new SqarvoClientException(
            SqarvoErrorKind.NotFound,
            "ConnectionService.Connect",
            "凭据不存在",
            StatusCode.NotFound,
            "credential.not_found"));

        Assert.Equal("认证失败：用户名或密码错误", authentication.Headline);
        Assert.Equal("mariadb.auth_failed", authentication.Code);
        Assert.Equal("凭据不存在：请重新输入密码", missingCredential.Headline);
        Assert.NotEqual(authentication.Headline, missingCredential.Headline);

        var local = SqarvoFailure.FromException(
            new SqarvoProcessException(SqarvoErrorKind.BackendBinaryNotFound, "找不到后端"));
        Assert.Equal("backend.binary_not_found", local.Code);

        var cancelled = SqarvoFailure.FromException(new OperationCanceledException());
        Assert.Equal("cancelled", cancelled.Code);
        Assert.Equal("已取消", cancelled.Headline);

        // 长消息截断到单行，避免多行诊断糊到状态栏。
        var longMessage = SqarvoFailure.FromException(new InvalidOperationException(new string('x', 400) + "\n第二行"));
        Assert.DoesNotContain('\n', longMessage.Detail);
        Assert.EndsWith("…", longMessage.Detail, StringComparison.Ordinal);
    }

    [Fact]
    public void StatusLineOnlyShowsRealCounts()
    {
        var ready = new BackendState
        {
            Readiness = BackendReadiness.Ready,
            Message = "后端 0.1.0",
            Version = new VersionInfo("0.1.0", "v1", "debug"),
            RegisteredDrivers = 2,
            ActiveSessions = 1,
        };

        Assert.True(ready.IsReady);
        Assert.Equal("已连接 · 驱动 2 · 浏览会话 1", ready.StatusLine);

        Assert.Equal("连接中 · 驱动 0 · 浏览会话 0", BackendState.Initial.StatusLine);
        Assert.Equal(
            "未连接 · 驱动 0 · 浏览会话 0",
            (ready with { Readiness = BackendReadiness.Disconnected, Message = "通道断了", Code = "transport.failed" }).StatusLine);

        Assert.False((ready with { Readiness = BackendReadiness.Failed }).IsReady);
    }
}