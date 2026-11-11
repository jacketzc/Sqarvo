using Sqarvo.Core.Errors;
using Sqarvo.Core.Protocol;

namespace Sqarvo.Core.Tests;

public sealed class ProtocolVersionTests
{
    [Theory]
    [InlineData("v1", 1)]
    [InlineData("v2.7", 2)]
    [InlineData("V3", 3)]
    [InlineData("v10.0.1", 10)]
    [InlineData(" v1 ", 1)]
    public void ParsesMajorVersion(string version, int expected)
    {
        Assert.True(ProtocolVersion.TryGetMajor(version, out var major));
        Assert.Equal(expected, major);
    }

    [Theory]
    [InlineData(null)]
    [InlineData("")]
    [InlineData("v")]
    [InlineData("vX")]
    [InlineData("1")]
    [InlineData("v1beta")]
    [InlineData("v.1")]
    public void RejectsMalformedMajorVersion(string? version)
    {
        Assert.False(ProtocolVersion.TryGetMajor(version, out _));
    }

    [Theory]
    [InlineData("v1")]
    [InlineData("v1.9")]
    public void AcceptsSameMajorVersion(string backendVersion)
    {
        Assert.True(ProtocolVersion.IsCompatible(backendVersion));
        ProtocolVersion.EnsureCompatible(backendVersion);
    }

    [Fact]
    public void RejectsDifferentMajorVersionWithUpgradeHint()
    {
        var exception = Assert.Throws<SqarvoProtocolException>(() => ProtocolVersion.EnsureCompatible("v2"));

        Assert.Equal(SqarvoErrorKind.ProtocolIncompatible, exception.Kind);
        Assert.Equal("v1", exception.ClientVersion);
        Assert.Equal("v2", exception.BackendVersion);
        Assert.Contains("协议版本不兼容", exception.Message, StringComparison.Ordinal);
        Assert.Contains("升级", exception.Message, StringComparison.Ordinal);
        Assert.False(ProtocolVersion.IsCompatible("v0"));
    }

    [Fact]
    public void UnparsableVersionIsACategoryOfItsOwn()
    {
        var exception = Assert.Throws<SqarvoProtocolException>(() => ProtocolVersion.EnsureCompatible("garbage"));

        Assert.Equal(SqarvoErrorKind.ProtocolUnparsable, exception.Kind);
        Assert.Equal("garbage", exception.BackendVersion);
        Assert.False(exception.IsRetryable);
    }
}