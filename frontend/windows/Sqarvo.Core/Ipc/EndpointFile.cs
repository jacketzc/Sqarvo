using System.Text.Json;
using Sqarvo.Core.Errors;

namespace Sqarvo.Core.Ipc;

/// <summary>
/// Reads and validates the backend endpoint description file.
/// </summary>
/// <remarks>
/// <para>
/// The file is written by the backend with an atomic rename, so a reader never observes a partial
/// document. Parsing is intentionally asymmetric: <b>unknown fields are ignored</b> (a newer backend
/// may add fields without breaking this client), while <b>required fields are validated strictly</b>
/// (missing field, wrong JSON type, non-hexadecimal token, out-of-range port are all errors).
/// </para>
/// <para>
/// Error messages never echo the token; only its length is reported.
/// </para>
/// </remarks>
public static class EndpointFile
{
    /// <summary>File name used by the backend inside its runtime directory.</summary>
    public const string FileName = "backend-endpoint.json";

    /// <summary>
    /// Environment variable that overrides the endpoint file path (tests, packaging and custom
    /// runtime directories). The backend itself does not read it; see <c>IPathLayout</c> for how the
    /// child process is pointed at the same location.
    /// </summary>
    public const string PathEnvironmentVariable = "SQARVO_ENDPOINT_FILE";

    private static readonly JsonDocumentOptions ParseOptions = new()
    {
        AllowTrailingCommas = true,
        CommentHandling = JsonCommentHandling.Skip,
    };

    /// <summary>
    /// Reads and validates the endpoint file.
    /// </summary>
    /// <param name="path">Endpoint file path.</param>
    /// <returns>
    /// The validated endpoint, or <see langword="null"/> when the file does not exist (the caller
    /// then decides whether to start a backend).
    /// </returns>
    /// <exception cref="SqarvoEndpointException">
    /// The file exists but cannot be read (<see cref="SqarvoErrorKind.EndpointUnreadable"/>), or its
    /// contents violate the contract (<see cref="SqarvoErrorKind.EndpointInvalid"/>).
    /// </exception>
    public static EndpointInfo? Read(string path)
    {
        ArgumentException.ThrowIfNullOrEmpty(path);

        string payload;
        try
        {
            payload = File.ReadAllText(path);
        }
        catch (Exception exception) when (exception is FileNotFoundException or DirectoryNotFoundException)
        {
            return null;
        }
        catch (Exception exception) when (exception is IOException or UnauthorizedAccessException or NotSupportedException)
        {
            throw new SqarvoEndpointException(
                SqarvoErrorKind.EndpointUnreadable,
                path,
                $"无法读取端点文件：{exception.GetType().Name}",
                exception);
        }

        return Parse(path, payload);
    }

    /// <summary>
    /// Non-throwing variant of <see cref="Read"/>, for polling loops that must distinguish "not ready
    /// yet" from "written but wrong".
    /// </summary>
    /// <param name="path">Endpoint file path.</param>
    /// <param name="endpoint">Validated endpoint, or <see langword="null"/> when the file is absent.</param>
    /// <param name="error">Failure detail when the file exists but is invalid.</param>
    /// <returns><see langword="true"/> when no error occurred (including "file absent").</returns>
    public static bool TryRead(string path, out EndpointInfo? endpoint, out SqarvoException? error)
    {
        try
        {
            endpoint = Read(path);
            error = null;
            return true;
        }
        catch (SqarvoException exception)
        {
            endpoint = null;
            error = exception;
            return false;
        }
    }

    private static EndpointInfo Parse(string path, string payload)
    {
        JsonDocument document;
        try
        {
            document = JsonDocument.Parse(payload, ParseOptions);
        }
        catch (JsonException exception)
        {
            throw new SqarvoEndpointException(
                SqarvoErrorKind.EndpointInvalid,
                path,
                "JSON 解析失败",
                exception);
        }

        using (document)
        {
            var root = document.RootElement;
            if (root.ValueKind != JsonValueKind.Object)
            {
                throw new SqarvoEndpointException(
                    SqarvoErrorKind.EndpointInvalid,
                    path,
                    "顶层必须是 JSON 对象");
            }

            var transportText = RequireString(root, "transport", path);
            var transport = transportText switch
            {
                "unix" => TransportKind.Unix,
                "tcp" => TransportKind.Tcp,
                _ => throw new SqarvoEndpointException(
                    SqarvoErrorKind.EndpointInvalid,
                    path,
                    $"transport 取值非法：{transportText}"),
            };

            var token = RequireString(root, "token", path);
            var protocolVersion = RequireString(root, "protocol_version", path);
            var serviceVersion = RequireString(root, "service_version", path);
            var pid = RequireInt32(root, "pid", path);

            string? socketPath = null;
            string? host = null;
            int? port = null;

            if (transport == TransportKind.Unix)
            {
                socketPath = RequireString(root, "socket_path", path);
            }
            else
            {
                host = RequireString(root, "host", path);
                port = RequireInt32(root, "port", path);
            }

            try
            {
                return new EndpointInfo(
                    transport,
                    token,
                    pid,
                    protocolVersion,
                    serviceVersion,
                    socketPath,
                    host,
                    port,
                    ReadTimestamp(root));
            }
            catch (SqarvoEndpointException exception)
            {
                // The constructor does not know the file path; add it so the user can find the file.
                throw new SqarvoEndpointException(exception.Kind, path, exception.Detail);
            }
        }
    }

    private static string RequireString(JsonElement root, string name, string path)
    {
        if (!root.TryGetProperty(name, out var element) || element.ValueKind != JsonValueKind.String)
        {
            throw new SqarvoEndpointException(
                SqarvoErrorKind.EndpointInvalid,
                path,
                $"缺少必需字段 {name}（或类型不是字符串）");
        }

        var value = element.GetString();
        if (string.IsNullOrWhiteSpace(value))
        {
            throw new SqarvoEndpointException(
                SqarvoErrorKind.EndpointInvalid,
                path,
                $"必需字段 {name} 为空");
        }

        return value;
    }

    private static int RequireInt32(JsonElement root, string name, string path)
    {
        if (!root.TryGetProperty(name, out var element)
            || element.ValueKind != JsonValueKind.Number
            || !element.TryGetInt32(out var value))
        {
            throw new SqarvoEndpointException(
                SqarvoErrorKind.EndpointInvalid,
                path,
                $"缺少必需字段 {name}（或类型不是 32 位整数）");
        }

        return value;
    }

    /// <summary>
    /// Reads <c>created_at</c>. It is diagnostic only, so an unparsable value is ignored rather than
    /// rejecting an otherwise usable endpoint.
    /// </summary>
    private static DateTimeOffset? ReadTimestamp(JsonElement root) =>
        root.TryGetProperty("created_at", out var element)
        && element.ValueKind == JsonValueKind.String
        && element.TryGetDateTimeOffset(out var value)
            ? value
            : null;
}