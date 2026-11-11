namespace Sqarvo.Core.Errors;

/// <summary>
/// The backend endpoint description is missing required data, malformed, or unreadable.
/// </summary>
public sealed class SqarvoEndpointException : SqarvoException
{
    /// <summary>Initialises the exception.</summary>
    /// <param name="kind">Either <see cref="SqarvoErrorKind.EndpointInvalid"/> or <see cref="SqarvoErrorKind.EndpointUnreadable"/>.</param>
    /// <param name="path">Endpoint file path, when known.</param>
    /// <param name="detail">What exactly is wrong (never echoes the token).</param>
    /// <param name="innerException">Underlying cause, when there is one.</param>
    public SqarvoEndpointException(
        SqarvoErrorKind kind,
        string? path,
        string detail,
        Exception? innerException = null)
        : base(kind, FormatMessage(path, detail), innerException)
    {
        Path = path;
        Detail = detail;
    }

    /// <summary>Endpoint file path, when known.</summary>
    public string? Path { get; }

    /// <summary>Description of the violated expectation.</summary>
    public string Detail { get; }

    private static string FormatMessage(string? path, string detail) =>
        path is null ? $"后端端点描述非法：{detail}" : $"后端端点描述非法（{path}）：{detail}";
}