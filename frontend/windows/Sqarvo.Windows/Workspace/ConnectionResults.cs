using Sqarvo.Windows.Connections;

namespace Sqarvo.Windows.Workspace;

/// <summary>
/// <c>ConnectionService.TestConnection</c> 的可展示结果。
/// </summary>
/// <param name="Reachable">后端是否判定可达（后端如实返回，不推断）。</param>
/// <param name="LatencyMillis">往返延迟（毫秒）。</param>
/// <param name="ServerVersion">服务端版本；后端拿不到时为空串，界面必须显示「未知」。</param>
/// <param name="Warnings">后端给出的非致命告警（<c>码：文案</c>）。</param>
public sealed record ConnectionTestResult(
    bool Reachable,
    ulong LatencyMillis,
    string ServerVersion,
    IReadOnlyList<string> Warnings);

/// <summary>一次 <c>ConnectionService.Connect</c> 的结果（会话身份 + 驱动能力位）。</summary>
/// <param name="ConnectionId">连接 ID（<c>Disconnect</c> 用它）。</param>
/// <param name="SessionId">会话 ID。</param>
/// <param name="DriverId">后端确认的驱动 ID。</param>
/// <param name="ServerVersion">服务端版本；拿不到时为空串。</param>
/// <param name="Capabilities">驱动能力位名称（如 <c>transactions</c>）。</param>
public sealed record ConnectionOpenResult(
    string ConnectionId,
    string SessionId,
    string DriverId,
    string ServerVersion,
    IReadOnlyList<string> Capabilities);

/// <summary>
/// 「保存并连接」的完整结果：连接已经建立，配置可能没写成功（连接仍然可用）。
/// </summary>
/// <param name="Profile">落盘（或准备落盘）的条目，已带最终凭据引用。</param>
/// <param name="Open">会话身份。</param>
/// <param name="LatencyMillis">连接前测试得到的延迟。</param>
/// <param name="ConfigWarning">配置写入失败的原因；成功时为 <see langword="null"/>。</param>
public sealed record ConnectionAttempt(
    ConnectionProfile Profile,
    ConnectionOpenResult Open,
    ulong LatencyMillis,
    string? ConfigWarning);