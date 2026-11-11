using System.Text;

namespace Sqarvo.Core.Tests.Support;

/// <summary>Temporary directory that is removed when disposed.</summary>
internal sealed class TempDirectory : IDisposable
{
    internal TempDirectory(string prefix = "sqarvo-core-test-")
    {
        Path = System.IO.Path.Combine(System.IO.Path.GetTempPath(), prefix + Guid.NewGuid().ToString("N")[..12]);
        Directory.CreateDirectory(Path);
    }

    internal string Path { get; }

    internal string Combine(params string[] parts) =>
        System.IO.Path.Combine(new[] { Path }.Concat(parts).ToArray());

    /// <summary>Writes the parent directory and the file, so tests can use "&lt;root&gt;/sqarvo/..." layouts.</summary>
    internal string WriteFile(string relativePath, string contents)
    {
        var full = Combine(relativePath.Split('/'));
        Directory.CreateDirectory(System.IO.Path.GetDirectoryName(full)!);
        File.WriteAllText(full, contents, new UTF8Encoding(encoderShouldEmitUTF8Identifier: false));
        return full;
    }

    public void Dispose()
    {
        try
        {
            if (Directory.Exists(Path))
            {
                Directory.Delete(Path, recursive: true);
            }
        }
        catch (IOException)
        {
        }
        catch (UnauthorizedAccessException)
        {
        }
    }
}

/// <summary>Endpoint JSON builder for tests (keeps the field names in one place).</summary>
internal static class EndpointJson
{
    internal static string Tcp(
        int pid,
        string host = "127.0.0.1",
        int port = 51234,
        string protocolVersion = "v1",
        string? token = null,
        string serviceVersion = "0.1.0",
        string? extraFields = null) =>
        $$"""
        {
          "transport": "tcp",
          "host": "{{host}}",
          "port": {{port}},
          "token": "{{token ?? ValidToken}}",
          "pid": {{pid}},
          "protocol_version": "{{protocolVersion}}",
          "service_version": "{{serviceVersion}}",
          "created_at": "2026-11-11T04:28:13.418504664Z"{{(extraFields is null ? string.Empty : "," + extraFields)}}
        }
        """;

    internal static string Unix(
        int pid,
        string socketPath = "/run/user/1000/sqarvo/backend.sock",
        string protocolVersion = "v1") =>
        $$"""
        {
          "transport": "unix",
          "socket_path": "{{socketPath}}",
          "token": "{{ValidToken}}",
          "pid": {{pid}},
          "protocol_version": "{{protocolVersion}}",
          "service_version": "0.1.0",
          "created_at": "2026-11-11T04:28:13.418504664Z"
        }
        """;

    internal static readonly string ValidToken = new('a', 64);
}