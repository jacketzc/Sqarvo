namespace Sqarvo.Core.Model;

/// <summary>Backend version information returned by <c>SystemService.GetVersion</c>.</summary>
/// <param name="ServiceVersion">Backend service version, for example <c>0.1.0</c>.</param>
/// <param name="ProtocolVersion">Protocol version, for example <c>v1</c>.</param>
/// <param name="BuildProfile"><c>debug</c> or <c>release</c>; diagnostics only.</param>
public sealed record VersionInfo(string ServiceVersion, string ProtocolVersion, string BuildProfile)
{
    /// <summary>Formats the version for display in an about box or status bar.</summary>
    public override string ToString() => $"{ServiceVersion}（协议 {ProtocolVersion}，{BuildProfile}）";
}