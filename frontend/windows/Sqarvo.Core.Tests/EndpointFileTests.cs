using Sqarvo.Core.Errors;
using Sqarvo.Core.Ipc;
using Sqarvo.Core.Tests.Support;

namespace Sqarvo.Core.Tests;

public sealed class EndpointFileTests
{
    [Fact]
    public void ReadsTcpEndpointAndIgnoresUnknownFields()
    {
        using var directory = new TempDirectory();
        var path = directory.WriteFile(
            "run/backend-endpoint.json",
            EndpointJson.Tcp(
                pid: 4242,
                port: 51234,
                extraFields: "\"future_field\": {\"nested\": [1, 2, 3]}, \"another\": null"));

        var endpoint = EndpointFile.Read(path);

        Assert.NotNull(endpoint);
        Assert.Equal(TransportKind.Tcp, endpoint.Transport);
        Assert.Equal("127.0.0.1", endpoint.Host);
        Assert.Equal(51234, endpoint.Port);
        Assert.Equal(4242, endpoint.Pid);
        Assert.Equal("v1", endpoint.ProtocolVersion);
        Assert.Equal("0.1.0", endpoint.ServiceVersion);
        Assert.Equal(64, endpoint.TokenLength);
        Assert.Null(endpoint.SocketPath);
        Assert.NotNull(endpoint.CreatedAt);
    }

    [Fact]
    public void ReadsUnixEndpoint()
    {
        using var directory = new TempDirectory();
        var path = directory.WriteFile("backend-endpoint.json", EndpointJson.Unix(pid: 77));

        var endpoint = EndpointFile.Read(path);

        Assert.NotNull(endpoint);
        Assert.Equal(TransportKind.Unix, endpoint.Transport);
        Assert.Equal("/run/user/1000/sqarvo/backend.sock", endpoint.SocketPath);
        Assert.Null(endpoint.Host);
        Assert.Null(endpoint.Port);
    }

    [Fact]
    public void MissingFileIsNotAnError()
    {
        using var directory = new TempDirectory();

        Assert.Null(EndpointFile.Read(directory.Combine("absent.json")));
        Assert.True(EndpointFile.TryRead(directory.Combine("absent.json"), out var endpoint, out var error));
        Assert.Null(endpoint);
        Assert.Null(error);
    }

    [Fact]
    public void AcceptsMissingCreatedAtBecauseItIsDiagnosticOnly()
    {
        using var directory = new TempDirectory();
        var path = directory.WriteFile(
            "backend-endpoint.json",
            """
            {
              "transport": "tcp",
              "host": "127.0.0.1",
              "port": 1234,
              "token": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
              "pid": 9,
              "protocol_version": "v1",
              "service_version": "0.1.0"
            }
            """);

        var endpoint = EndpointFile.Read(path);

        Assert.NotNull(endpoint);
        Assert.Null(endpoint.CreatedAt);
        Assert.Equal(64, endpoint.TokenLength);
    }

    [Theory]
    [InlineData("\"transport\": \"tcp\", \"host\": \"127.0.0.1\", \"port\": 1234, \"pid\": 5, \"protocol_version\": \"v1\", \"service_version\": \"0.1.0\"", "token")]
    [InlineData("\"transport\": \"tcp\", \"host\": \"127.0.0.1\", \"port\": 1234, \"token\": \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\", \"protocol_version\": \"v1\", \"service_version\": \"0.1.0\"", "pid")]
    [InlineData("\"transport\": \"tcp\", \"host\": \"127.0.0.1\", \"token\": \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\", \"pid\": 5, \"protocol_version\": \"v1\", \"service_version\": \"0.1.0\"", "port")]
    [InlineData("\"transport\": \"tcp\", \"host\": \"127.0.0.1\", \"port\": 1234, \"token\": \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\", \"pid\": 5, \"service_version\": \"0.1.0\"", "protocol_version")]
    [InlineData("\"transport\": \"tcp\", \"host\": \"127.0.0.1\", \"port\": 1234, \"token\": \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\", \"pid\": 5, \"protocol_version\": \"v1\"", "service_version")]
    [InlineData("\"transport\": \"unix\", \"token\": \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\", \"pid\": 5, \"protocol_version\": \"v1\", \"service_version\": \"0.1.0\"", "socket_path")]
    [InlineData("\"transport\": \"pipe\", \"host\": \"127.0.0.1\", \"port\": 1234, \"token\": \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\", \"pid\": 5, \"protocol_version\": \"v1\", \"service_version\": \"0.1.0\"", "transport")]
    [InlineData("\"transport\": \"tcp\", \"host\": \"127.0.0.1\", \"port\": 70000, \"token\": \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\", \"pid\": 5, \"protocol_version\": \"v1\", \"service_version\": \"0.1.0\"", "port")]
    public void RejectsMissingOrInvalidRequiredField(string body, string expectedField)
    {
        using var directory = new TempDirectory();
        var path = directory.WriteFile("backend-endpoint.json", "{" + body + "}");

        var exception = Assert.Throws<SqarvoEndpointException>(() => EndpointFile.Read(path));

        Assert.Equal(SqarvoErrorKind.EndpointInvalid, exception.Kind);
        Assert.Equal(path, exception.Path);
        Assert.Contains(expectedField, exception.Message, StringComparison.Ordinal);
    }

    [Fact]
    public void RejectsMalformedTokenWithoutEchoingIt()
    {
        using var directory = new TempDirectory();
        var path = directory.WriteFile(
            "backend-endpoint.json",
            EndpointJson.Tcp(pid: 5, token: "not-a-token-but-distinctive"));

        var exception = Assert.Throws<SqarvoEndpointException>(() => EndpointFile.Read(path));

        Assert.Equal(SqarvoErrorKind.EndpointInvalid, exception.Kind);
        Assert.Contains("token 格式非法", exception.Message, StringComparison.Ordinal);
        Assert.DoesNotContain("not-a-token-but-distinctive", exception.Message, StringComparison.Ordinal);
    }

    [Fact]
    public void RejectsBrokenJsonAndNonObjectRoot()
    {
        using var directory = new TempDirectory();
        var broken = directory.WriteFile("broken.json", "{ \"transport\": ");
        var array = directory.WriteFile("array.json", "[1, 2, 3]");

        Assert.Equal(SqarvoErrorKind.EndpointInvalid, Assert.Throws<SqarvoEndpointException>(() => EndpointFile.Read(broken)).Kind);
        Assert.Contains("JSON", Assert.Throws<SqarvoEndpointException>(() => EndpointFile.Read(broken)).Message, StringComparison.Ordinal);
        Assert.Contains("顶层", Assert.Throws<SqarvoEndpointException>(() => EndpointFile.Read(array)).Message, StringComparison.Ordinal);
    }

    [Fact]
    public void RedactsTokenInDiagnostics()
    {
        using var directory = new TempDirectory();
        var token = new string('b', 64);
        var path = directory.WriteFile("backend-endpoint.json", EndpointJson.Tcp(pid: 5, token: token));

        var endpoint = EndpointFile.Read(path)!;

        Assert.DoesNotContain(token, endpoint.ToString(), StringComparison.Ordinal);
        Assert.DoesNotContain(token, endpoint.RedactedSummary, StringComparison.Ordinal);
        Assert.Contains("<redacted>", endpoint.RedactedSummary, StringComparison.Ordinal);
        Assert.Equal(token, endpoint.Token);
    }

    [Fact]
    public void TryReadReportsInvalidFileWithoutThrowing()
    {
        using var directory = new TempDirectory();
        var path = directory.WriteFile("backend-endpoint.json", EndpointJson.Tcp(pid: 0));

        Assert.False(EndpointFile.TryRead(path, out var endpoint, out var error));
        Assert.Null(endpoint);
        var exception = Assert.IsType<SqarvoEndpointException>(error);
        Assert.Equal(SqarvoErrorKind.EndpointInvalid, exception.Kind);
    }
}