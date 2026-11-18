using System;
using System.Globalization;

namespace Sqarvo.Windows.Localization;

/// <summary>语言偏好与本次进程所用的资源语言。</summary>
internal static class LanguageService
{
    private static readonly string SystemLanguage =
        CultureInfo.CurrentUICulture.Name.StartsWith("zh", StringComparison.OrdinalIgnoreCase) ? "zh" : "en";

    public static string Preference { get; private set; } = "system";
    public static string Effective { get; private set; } = SystemLanguage;

    public static void Initialize(string? saved, string? overrideLanguage, bool qa)
    {
        Preference = saved is "zh" or "en" ? saved : "system";
        Effective = overrideLanguage is "zh" or "en" ? overrideLanguage : qa ? "zh" : Resolve(Preference);
    }

    public static string Resolve(string preference) => preference == "system" ? SystemLanguage : preference;

    public static void SetPreference(string preference) => Preference = preference;
}
