using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Sqarvo.Core.Model;
using Sqarvo.Windows.Connections;
using Sqarvo.Windows.Workspace;
using System;
using System.Collections.Generic;
using System.IO;
using System.Threading;
using System.Threading.Tasks;

namespace Sqarvo.Windows.Views;

/// <summary>
/// 新建连接对话框（#169）：驱动来自 <c>SystemService.ListDrivers</c>，MariaDB 字段 + TLS + 测试连接。
/// </summary>
/// <remarks>
/// <para>
/// 流程与 Linux 客户端一致：**先测试再建会话**（用户立刻看到延迟与服务端版本，认证失败时也不会
/// 留下半个会话），成功后写配置。顺序是：填密码 → <c>StoreCredential</c> 拿 <c>credential_ref</c>
/// → <c>TestConnection</c> → <c>Connect</c> → 落盘。密码只在 <see cref="PasswordBox"/> 里存在，
/// 不进条目、不进日志、不进错误文案。
/// </para>
/// <para>
/// 所有 RPC 都带 <see cref="CancellationToken"/>：对话框关闭即取消（<see cref="_lifetime"/>）。
/// 失败按 <see cref="SqarvoFailure"/> 的分类展示主标题 + 后端文案 + 稳定错误码，不合并成
/// 「连接失败」。
/// </para>
/// </remarks>
public sealed partial class ConnectionDialog : ContentDialog
{
    private readonly BackendSession _session;
    private readonly ConnectionStore _store;
    private readonly CancellationTokenSource _lifetime = new();

    private string? _credentialRef;
    private bool _busy;
    private bool _initialized;

    /// <summary>状态行当前显示的是「本驱动没有表单」这条提示（切回支持的表单时撤掉它）。</summary>
    private bool _formUnavailable;

    /// <summary>创建对话框。</summary>
    /// <param name="session">已就绪的后端会话（驱动清单与 RPC 都从这里来）。</param>
    /// <param name="store">连接配置存储（保存时落盘）。</param>
    public ConnectionDialog(BackendSession session, ConnectionStore store)
    {
        ArgumentNullException.ThrowIfNull(session);
        ArgumentNullException.ThrowIfNull(store);

        _session = session;
        _store = store;

        InitializeComponent();

        Loaded += OnDialogLoaded;
        Closed += OnDialogClosed;
    }

    /// <summary>保存并连接成功后的结果；用户取消或失败时为 <see langword="null"/>。</summary>
    public ConnectionAttempt? Attempt { get; private set; }

    private void OnDialogLoaded(object sender, RoutedEventArgs e)
    {
        if (_initialized)
        {
            return;
        }

        _initialized = true;

        var choices = new List<DriverChoice>();
        foreach (var driver in _session.State.Drivers)
        {
            choices.Add(new DriverChoice(driver));
        }

        DriverCombo.ItemsSource = choices;

        if (choices.Count == 0)
        {
            ShowFailure(
                "后端没有注册任何驱动",
                "SystemService.ListDrivers 返回空列表：这个后端构建里没有可用驱动，无法新建连接。",
                "driver.none");
            DriverCombo.IsEnabled = false;
            TestButton.IsEnabled = false;
            IsPrimaryButtonEnabled = false;
            return;
        }

        var selected = choices.FindIndex(choice => ConnectionForm.FamilyFor(choice.Id) == DriverFamily.Server);
        DriverCombo.SelectedIndex = selected >= 0 ? selected : 0;

        // 新建时的界面默认值（不是从配置里读的假数据）：本机回环 + MariaDB 常用端口。
        HostBox.Text = "127.0.0.1";
        PortBox.Text = "3306";

        ApplyDriver();
    }

    private void OnDialogClosed(ContentDialog sender, ContentDialogClosedEventArgs args)
    {
        // 关闭即取消：在途的 RPC 不会继续占着通道。
        // 只取消不释放：在途调用可能还在读这个令牌，释放会让它们抛 ObjectDisposedException。
        _lifetime.Cancel();
    }

    private async void OnPrimaryClick(ContentDialog sender, ContentDialogButtonClickEventArgs args)
    {
        var deferral = args.GetDeferral();

        // 先假设不关：只有「测试 → 连接 → 落盘」都成功才放行。
        args.Cancel = true;

        try
        {
            if (_busy)
            {
                return;
            }

            var attempt = await SaveAndConnectAsync();
            if (attempt is null)
            {
                return;
            }

            Attempt = attempt;
            args.Cancel = false;
        }
        finally
        {
            deferral.Complete();
        }
    }

    private async void OnTestClick(object sender, RoutedEventArgs e)
    {
        if (_busy)
        {
            return;
        }

        var validation = ConnectionForm.Validate(ReadInput());
        if (!validation.IsValid)
        {
            ShowFailure("请先补全字段", validation.Error!, "input.invalid");
            return;
        }

        if (!_session.State.IsReady)
        {
            ShowFailure("本地后端未就绪", _session.State.Message, _session.State.Code);
            return;
        }

        SetBusy(true);
        try
        {
            var profile = validation.Profile!;
            if (!await PrepareCredentialAsync(profile).ConfigureAwait(true))
            {
                return;
            }

            profile = profile with { CredentialRef = _credentialRef! };

            // 测试阶段不覆盖已保存的密码之外的东西：这里只验证「这套参数 + 这个密码」能不能连上。
            var test = await _session.TestConnectionAsync(profile, _lifetime.Token).ConfigureAwait(true);
            if (!test.Reachable)
            {
                ShowFailure(
                    "连接不可达",
                    "后端把 TestConnection 判为不可达（reachable=false）；请检查主机、端口与网络。",
                    "connection.unreachable");
                return;
            }

            var warnings = test.Warnings.Count == 0
                ? string.Empty
                : $"；告警：{string.Join("；", test.Warnings)}";
            ShowSuccess($"连接成功：延迟 {test.LatencyMillis} ms · 服务端版本 {VersionText(test.ServerVersion)}{warnings}");
        }
        catch (Exception error)
        {
            ShowFailure(SqarvoFailure.FromException(error));
        }
        finally
        {
            SetBusy(false);
        }
    }

    /// <summary>保存并连接：凭据 → 测试 → 会话 → 落盘（与 Linux「保存并连接」同序）。</summary>
    private async Task<ConnectionAttempt?> SaveAndConnectAsync()
    {
        var validation = ConnectionForm.Validate(ReadInput());
        if (!validation.IsValid)
        {
            ShowFailure("请先补全字段", validation.Error!, "input.invalid");
            return null;
        }

        if (!_session.State.IsReady)
        {
            ShowFailure("本地后端未就绪", _session.State.Message, _session.State.Code);
            return null;
        }

        SetBusy(true);
        try
        {
            var profile = validation.Profile!;
            if (!await PrepareCredentialAsync(profile).ConfigureAwait(true))
            {
                return null;
            }

            profile = profile with { CredentialRef = _credentialRef! };

            var test = await _session.TestConnectionAsync(profile, _lifetime.Token).ConfigureAwait(true);
            if (!test.Reachable)
            {
                ShowFailure(
                    "连接不可达",
                    "后端把 TestConnection 判为不可达（reachable=false）；请检查主机、端口与网络。",
                    "connection.unreachable");
                return null;
            }

            var open = await _session.ConnectAsync(profile, _lifetime.Token).ConfigureAwait(true);

            // 连接已经建立；配置写不进去是「便利功能失效」，不是连接失败，分开报告。
            string? configWarning = null;
            try
            {
                await Task.Run(() => _store.Upsert(profile)).ConfigureAwait(true);
            }
            catch (Exception error) when (error is IOException or UnauthorizedAccessException or NotSupportedException)
            {
                configWarning = $"连接配置写入失败：{error.Message}";
            }

            PasswordBox.Password = string.Empty;

            var warning = configWarning is null ? string.Empty : $"；{configWarning}（本次连接仍然可用）";
            ShowSuccess(
                $"已连接：延迟 {test.LatencyMillis} ms · 服务端版本 {VersionText(test.ServerVersion)}{warning}");

            return new ConnectionAttempt(profile, open, test.LatencyMillis, configWarning);
        }
        catch (Exception error)
        {
            ShowFailure(SqarvoFailure.FromException(error));
            return null;
        }
        finally
        {
            SetBusy(false);
        }
    }

    /// <summary>
    /// 确保本次尝试有可用的 <c>credential_ref</c>：填了密码就写进系统凭据库（同一目标覆盖原引用），
    /// 没填则要求已有引用。
    /// </summary>
    /// <returns>是否有可用引用（失败时已在状态行说明原因）。</returns>
    private async Task<bool> PrepareCredentialAsync(ConnectionProfile profile)
    {
        var password = PasswordBox.Password;
        if (password.Length > 0)
        {
            // 身份相同就覆盖原引用，避免同一连接在凭据库里堆多份密码（Linux #145 的结论）。
            var existing = await Task.Run(() => _store.FindByIdentity(profile)).ConfigureAwait(true);
            var reuse = !string.IsNullOrEmpty(existing?.CredentialRef) ? existing!.CredentialRef : _credentialRef;
            _credentialRef = await _session.StoreCredentialAsync(reuse, password, _lifetime.Token)
                .ConfigureAwait(true);
        }

        if (string.IsNullOrEmpty(_credentialRef))
        {
            ShowFailure("请输入密码", "本次没有可用的凭据引用：请填写密码后重试。", "input.invalid");
            return false;
        }

        return true;
    }

    private ConnectionInput ReadInput() => new()
    {
        DriverId = (DriverCombo.SelectedItem as DriverChoice)?.Id ?? string.Empty,
        Host = HostBox.Text,
        Port = PortBox.Text,
        Username = UsernameBox.Text,
        Password = PasswordBox.Password,
        Database = DatabaseBox.Text,
        TlsMode = (TlsCombo.SelectedItem as TlsModeChoice)?.Value ?? TlsModeChoice.Preferred,
        CredentialRef = _credentialRef,
    };

    private void OnDriverChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_initialized)
        {
            ApplyDriver();
        }
    }

    /// <summary>按驱动族显示字段、重建 TLS 档位标签、同步按钮可用性。</summary>
    private void ApplyDriver()
    {
        var choice = DriverCombo.SelectedItem as DriverChoice;
        var family = choice is null ? DriverFamily.Unknown : ConnectionForm.FamilyFor(choice.Id);
        var server = family == DriverFamily.Server;

        foreach (var field in new Control[] { HostBox, PortBox, UsernameBox, PasswordBox, DatabaseBox, TlsCombo })
        {
            field.IsEnabled = server && !_busy;
        }

        // TLS 标签取决于驱动能力（verify-ca 是否等同 verify-full），必须在任何读取选中值之前重建。
        var chainOnly = TlsModeChoice.SupportsChainOnlyVerifyCa(choice?.Capabilities);
        var current = (TlsCombo.SelectedItem as TlsModeChoice)?.Value ?? TlsModeChoice.Preferred;
        TlsCombo.ItemsSource = TlsModeChoice.ChoicesFor(chainOnly);
        TlsCombo.SelectedIndex = TlsModeChoice.IndexOf(current, chainOnly);

        TestButton.IsEnabled = server && !_busy;
        IsPrimaryButtonEnabled = server && !_busy;

        if (family == DriverFamily.Server)
        {
            // 切回有表单的驱动时，只撤掉「没有表单」这条提示，不碰真正的测试/连接结果。
            if (_formUnavailable)
            {
                _formUnavailable = false;
                StatusText.Visibility = Visibility.Collapsed;
            }

            return;
        }

        _formUnavailable = true;

        if (family == DriverFamily.File)
        {
            ShowFailure(
                "本版本没有文件型驱动的连接表单",
                $"驱动「{choice!.Id}」是文件型数据库：文件路径与只读开关的表单随后续工作包提供，"
                + "现在新建会得到一份后端无法接受的参数，因此这里不允许保存。",
                "driver.form_missing");
            return;
        }

        ShowFailure(
            "本版本没有该驱动的连接表单",
            $"后端注册了驱动「{choice?.Id ?? "（未选择）"}」，但此版本前端没有它的字段定义："
            + "请升级前端，或换一个驱动。",
            "driver.form_missing");
    }

    private void SetBusy(bool busy)
    {
        _busy = busy;
        TestButton.IsEnabled = !busy && DriverCombo.SelectedItem is DriverChoice;
        IsPrimaryButtonEnabled = !busy && DriverCombo.SelectedItem is DriverChoice;
        ApplyDriver();
    }

    private void ShowFailure(SqarvoFailure failure) =>
        ShowFailure(failure.Headline, $"{failure.Detail} [{failure.Code}]", failure.Code);

    private void ShowFailure(string headline, string detail, string code)
    {
        StatusText.Text = $"失败：{headline}（{code}）\n{detail}";
        StatusText.Visibility = Visibility.Visible;
    }

    private void ShowSuccess(string message)
    {
        StatusText.Text = $"成功：{message}";
        StatusText.Visibility = Visibility.Visible;
    }

    private static string VersionText(string serverVersion) =>
        string.IsNullOrWhiteSpace(serverVersion) ? "未知（后端未返回版本）" : serverVersion;

    /// <summary>驱动下拉的显示项：标签带稳定 ID，便于和日志/后端对照。</summary>
    private sealed record DriverChoice(DriverSummary Driver)
    {
        /// <summary>驱动 ID（<c>ListDrivers</c> 的返回值）。</summary>
        public string Id => Driver.DriverId;

        /// <summary>下拉展示文案：显示名 + 稳定 ID。</summary>
        public string Label => Driver.Label;

        /// <summary>驱动能力串（TLS 标签要用）。</summary>
        public IReadOnlyList<string> Capabilities => Driver.Capabilities;
    }
}