using Sqarvo.Core.Model;

namespace Sqarvo.Windows.Workspace;

/// <summary>
/// 本地后端的就绪状态（**唯一**的后端状态位：菜单、工具条、状态栏、对象树都读它）。
/// </summary>
/// <remarks>
/// 转移规则（全在 <see cref="BackendSession"/> 里发生，别处不允许写这个枚举）：
/// <list type="bullet">
/// <item><description><see cref="Starting"/> → <see cref="Ready"/>：<c>AcquireAsync</c> 成功，且
/// <c>GetVersion</c> / <c>ListDrivers</c> / <c>GetHealth</c> 全部返回。</description></item>
/// <item><description><see cref="Starting"/> → <see cref="Failed"/>：握手抛异常（找不到后端、端点超时、
/// 协议不兼容……），原因与错误码留在 <see cref="BackendState.Message"/> /
/// <see cref="BackendState.Code"/>。</description></item>
/// <item><description><see cref="Ready"/> → <see cref="Disconnected"/>：5 秒健康探测失败（通道断了、
/// 后端退出、token 失效）。界面回到「未连接」并展示原因，不假装还连着。</description></item>
/// <item><description><see cref="Disconnected"/> / <see cref="Failed"/> 是终态：本步不自动重启后端
/// （重启策略属于后续工作包，静默重启会让「为什么断了」变得不可见）。</description></item>
/// </list>
/// </remarks>
public enum BackendReadiness
{
    /// <summary>正在启动/握手：还不知道后端是否可用。</summary>
    Starting = 0,

    /// <summary>已就绪：可以发 RPC。</summary>
    Ready = 1,

    /// <summary>曾经就绪、现在断了（健康探测失败）；<see cref="BackendState.Message"/> 说明原因。</summary>
    Disconnected = 2,

    /// <summary>从未就绪：握手失败；<see cref="BackendState.Message"/> 说明原因。</summary>
    Failed = 3,
}

/// <summary>
/// 后端状态的一次快照：界面只读它渲染，不各自维护一套布尔量。
/// </summary>
public sealed record BackendState
{
    /// <summary>初始快照（<see cref="BackendReadiness.Starting"/>，还没有任何握手结果）。</summary>
    public static BackendState Initial { get; } = new()
    {
        Readiness = BackendReadiness.Starting,
        Message = "正在启动本地后端…",
    };

    /// <summary>当前状态位。</summary>
    public BackendReadiness Readiness { get; init; } = BackendReadiness.Starting;

    /// <summary>可直接展示的说明：就绪时是后端版本，失败时是「原因：详情 [错误码]」。</summary>
    public string Message { get; init; } = string.Empty;

    /// <summary>失败时的稳定错误码；就绪时为空。</summary>
    public string Code { get; init; } = string.Empty;

    /// <summary>后端版本（仅 <see cref="BackendReadiness.Ready"/> 时非空）。</summary>
    public VersionInfo? Version { get; init; }

    /// <summary>握手时 <c>SystemService.ListDrivers</c> 的结果（驱动清单的唯一来源）。</summary>
    public IReadOnlyList<DriverSummary> Drivers { get; init; } = Array.Empty<DriverSummary>();

    /// <summary>后端自报的已注册驱动数（来自健康快照，最多滞后一个探测周期）。</summary>
    public uint RegisteredDrivers { get; init; }

    /// <summary>后端自报的已打开数据库会话数（来自健康快照）。</summary>
    public uint ActiveSessions { get; init; }

    /// <summary>唯一的「后端就绪」判断：菜单、工具条、状态栏都读这一个值。</summary>
    public bool IsReady => Readiness == BackendReadiness.Ready;

    /// <summary>
    /// 状态栏右格的文案：真实计数，未就绪时不写假数字（断连后计数归零，因为那份快照已经作废）。
    /// </summary>
    public string StatusLine => Readiness switch
    {
        BackendReadiness.Ready => $"已连接 · 驱动 {RegisteredDrivers} · 浏览会话 {ActiveSessions}",
        BackendReadiness.Starting => "连接中 · 驱动 0 · 浏览会话 0",
        _ => "未连接 · 驱动 0 · 浏览会话 0",
    };

    /// <summary>「关于」对话框用的一行状态描述（只写真实值）。</summary>
    public string Describe() => Readiness switch
    {
        BackendReadiness.Ready when Version is { } version =>
            $"已连接后端 {version.ServiceVersion}（协议 {version.ProtocolVersion}，{version.BuildProfile}），驱动 {RegisteredDrivers} 个",
        BackendReadiness.Starting => "正在启动本地后端…",
        BackendReadiness.Disconnected => $"已断开：{Message}",
        _ => $"后端未就绪：{Message}",
    };
}