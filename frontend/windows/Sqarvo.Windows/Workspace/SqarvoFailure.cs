using Sqarvo.Core.Errors;

namespace Sqarvo.Windows.Workspace;

/// <summary>
/// 一次失败的可展示分类：主标题说明**用户能做什么**，副行是后端文案与稳定错误码。
/// </summary>
/// <remarks>
/// <para>
/// 为什么不合并成一句「连接失败」：认证失败要改密码、凭据缺失要重新保存、凭据库不可用要先解锁、
/// 驱动缺失是安装/版本问题、超时可以重试——处理方式完全不同（与 Linux 端
/// <c>ConnectionFailure</c> 同一分类法）。界面按 <see cref="Headline"/> 分档，按
/// <see cref="Code"/> 分支，绝不匹配文案。
/// </para>
/// <para>
/// 文案里**没有密码**：<c>Sqarvo.Core</c> 的异常从不携带密码，这里再截断到单行并限长，
/// 避免把后端多行诊断糊到状态栏上。
/// </para>
/// </remarks>
/// <param name="Headline">一句话结论（界面主标题）。</param>
/// <param name="Detail">后端或本地提供的详细原因（已截断、单行）。</param>
/// <param name="Code">稳定错误码：后端 <c>sqarvo-error-code</c> 原值，或本地错误分类。</param>
/// <param name="Retryable">原样重试是否可能成功。</param>
public sealed record SqarvoFailure(string Headline, string Detail, string Code, bool Retryable)
{
    /// <summary>状态栏/日志用的一行式描述。</summary>
    /// <returns><c>标题：详情 [错误码]</c>。</returns>
    public override string ToString() => $"{Headline}：{Detail} [{Code}]";

    /// <summary>
    /// 把任意异常映射成可展示的失败分类。
    /// </summary>
    /// <param name="error">捕获到的异常。</param>
    /// <returns>分类结果；不会抛异常。</returns>
    public static SqarvoFailure FromException(Exception error)
    {
        ArgumentNullException.ThrowIfNull(error);

        return error switch
        {
            OperationCanceledException => new SqarvoFailure(
                "已取消",
                "操作在完成前被取消；没有对数据库做任何修改。",
                "cancelled",
                Retryable: true),
            SqarvoClientException client => FromClient(client),
            SqarvoException sqarvo => FromLocal(sqarvo),
            _ => new SqarvoFailure("操作失败", Sanitize(error.Message), error.GetType().Name, Retryable: false),
        };
    }

    private static SqarvoFailure FromClient(SqarvoClientException error)
    {
        var code = string.IsNullOrWhiteSpace(error.ErrorCode) ? Describe(error.Kind).Code : error.ErrorCode!;
        var headline = HeadlineForCode(code);
        return new SqarvoFailure(
            headline ?? Describe(error.Kind).Headline,
            Sanitize(error.Message),
            code,
            error.IsRetryable);
    }

    private static SqarvoFailure FromLocal(SqarvoException error)
    {
        var (headline, code) = Describe(error.Kind);
        return new SqarvoFailure(headline, Sanitize(error.Message), code, error.IsRetryable);
    }

    /// <summary>后端稳定错误码 → 主标题；不认识的错误码返回 <see langword="null"/>（按状态码兜底）。</summary>
    private static string? HeadlineForCode(string code) => code switch
    {
        "mariadb.auth_failed" => "认证失败：用户名或密码错误",
        "credential.not_found" => "凭据不存在：请重新输入密码",
        "credential.unavailable" => "系统凭据库不可用（不会回退明文保存）",
        "driver.not_found" => "驱动不存在：后端没有该驱动",
        "capability.unsupported" => "驱动不支持该操作",
        "config.invalid" => "参数不合法",
        "transport.timeout" => "连接超时",
        "mariadb.connect_timeout" => "连接超时",
        _ => null,
    };

    /// <summary>
    /// 本地失败分类（端点文件、后端进程、协议、传输）：标题说明问题，码是稳定的机器可读标识。
    /// </summary>
    private static (string Headline, string Code) Describe(SqarvoErrorKind kind) => kind switch
    {
        SqarvoErrorKind.EndpointInvalid => ("端点文件内容非法", "endpoint.invalid"),
        SqarvoErrorKind.EndpointUnreadable => ("端点文件无法读取", "endpoint.unreadable"),
        SqarvoErrorKind.ProtocolIncompatible => ("前端与后端协议版本不兼容", "protocol.incompatible"),
        SqarvoErrorKind.ProtocolUnparsable => ("后端协议版本无法解析", "protocol.unparsable"),
        SqarvoErrorKind.BackendBinaryNotFound => ("找不到后端可执行文件", "backend.binary_not_found"),
        SqarvoErrorKind.BackendSpawnFailed => ("启动后端进程失败", "backend.spawn_failed"),
        SqarvoErrorKind.BackendExitedEarly => ("后端进程提前退出", "backend.exited_early"),
        SqarvoErrorKind.BackendStartTimeout => ("等待后端端点超时", "backend.start_timeout"),
        SqarvoErrorKind.BackendNotAcquired => ("本地后端尚未就绪", "backend.not_acquired"),
        SqarvoErrorKind.ShutdownFailed => ("后端未能退出", "backend.shutdown_failed"),
        SqarvoErrorKind.TransportFailed => ("本地通道不可用", "transport.failed"),
        SqarvoErrorKind.Unauthenticated => ("本地后端会话未授权", "unauthenticated"),
        SqarvoErrorKind.Cancelled => ("已取消", "cancelled"),
        SqarvoErrorKind.DeadlineExceeded => ("连接超时", "timeout"),
        SqarvoErrorKind.InvalidArgument => ("参数不合法", "config.invalid"),
        SqarvoErrorKind.NotFound => ("对象不存在", "not_found"),
        SqarvoErrorKind.AlreadyExists => ("对象已存在", "already_exists"),
        SqarvoErrorKind.PermissionDenied => ("没有权限", "permission_denied"),
        SqarvoErrorKind.FailedPrecondition => ("当前状态不允许该操作", "failed_precondition"),
        SqarvoErrorKind.ResourceExhausted => ("资源上限已到", "resource_exhausted"),
        SqarvoErrorKind.Unimplemented => ("后端或驱动未实现该能力", "unimplemented"),
        SqarvoErrorKind.Aborted => ("操作被中止（并发冲突）", "aborted"),
        SqarvoErrorKind.OutOfRange => ("取值超出范围", "out_of_range"),
        SqarvoErrorKind.Internal => ("后端内部错误", "internal"),
        SqarvoErrorKind.Unavailable => ("后端暂时不可用", "unavailable"),
        SqarvoErrorKind.DataLoss => ("数据丢失或损坏", "data_loss"),
        _ => ("操作失败", "error." + kind),
    };

    /// <summary>截断并清理消息：只保留首行、去掉首尾空白、限长 300 字符。</summary>
    private static string Sanitize(string? message)
    {
        var firstLine = message?.Split('\n')[0].Trim() ?? string.Empty;
        if (firstLine.Length == 0)
        {
            return "（未提供错误消息）";
        }

        return firstLine.Length > 300 ? $"{firstLine[..300]}…" : firstLine;
    }
}