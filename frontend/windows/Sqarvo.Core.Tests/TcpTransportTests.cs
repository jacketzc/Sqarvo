using System.Net.Http;
using Sqarvo.Core.Errors;
using Sqarvo.Core.Ipc;

namespace Sqarvo.Core.Tests;

public sealed class TcpTransportTests
{
    private static EndpointInfo Tcp(string host = "127.0.0.1", int port = 51234) =>
        new(TransportKind.Tcp, EndpointJsonToken, pid: 42, protocolVersion: "v1", serviceVersion: "0.1.0", host: host, port: port);

    private static readonly string EndpointJsonToken = new('c', 64);

    [Fact]
    public void BuildsLoopbackHttpAddress()
    {
        var transport = new TcpTransport(Tcp(port: 51234));

        Assert.Equal(TransportKind.Tcp, transport.Kind);
        Assert.Equal("http://127.0.0.1:51234/", transport.ChannelAddress.ToString());
        Assert.Contains("127.0.0.1:51234", transport.Description, StringComparison.Ordinal);
    }

    [Fact]
    public void SupportsIpv6Loopback()
    {
        var transport = new TcpTransport(Tcp(host: "::1", port: 9000));

        Assert.Equal("http://[::1]:9000/", transport.ChannelAddress.ToString());
    }

    [Theory]
    [InlineData("10.0.0.5")]
    [InlineData("192.168.1.10")]
    [InlineData("0.0.0.0")]
    [InlineData("8.8.8.8")]
    public void RejectsNonLoopbackAddresses(string host)
    {
        var exception = Assert.Throws<SqarvoTransportException>(() => new TcpTransport(Tcp(host: host)));

        Assert.Contains("回环", exception.Message, StringComparison.Ordinal);
        Assert.Equal(SqarvoErrorKind.TransportFailed, exception.Kind);
    }

    [Theory]
    [InlineData("localhost")]
    [InlineData("sqarvo.local")]
    public void RejectsHostNamesInsteadOfResolvingThem(string host)
    {
        var exception = Assert.Throws<SqarvoEndpointException>(() => new TcpTransport(Tcp(host: host)));

        Assert.Equal(SqarvoErrorKind.EndpointInvalid, exception.Kind);
        Assert.Contains("IP 字面量", exception.Message, StringComparison.Ordinal);
    }

    [Fact]
    public void HandlerDisablesProxyAndPinsTheLoopbackConnect()
    {
        var transport = new TcpTransport(Tcp());

        using var handler = Assert.IsType<SocketsHttpHandler>(transport.CreateHandler());

        Assert.False(handler.UseProxy);
        Assert.False(handler.AllowAutoRedirect);
        Assert.NotNull(handler.ConnectCallback);
        Assert.Equal(TimeSpan.FromSeconds(5), handler.ConnectTimeout);
        Assert.Equal(Timeout.InfiniteTimeSpan, handler.PooledConnectionIdleTimeout);
    }

    [Fact]
    public void FactorySelectsTcpAndRejectsUnix()
    {
        var tcp = TransportFactory.Create(Tcp());
        Assert.IsType<TcpTransport>(tcp);

        var unixEndpoint = new EndpointInfo(
            TransportKind.Unix,
            EndpointJsonToken,
            pid: 42,
            protocolVersion: "v1",
            serviceVersion: "0.1.0",
            socketPath: "/run/user/1000/sqarvo/backend.sock");

        var exception = Assert.Throws<SqarvoTransportException>(() => TransportFactory.Create(unixEndpoint));
        Assert.Equal(SqarvoErrorKind.TransportFailed, exception.Kind);
        Assert.Contains("Unix", exception.Message, StringComparison.Ordinal);
    }
}