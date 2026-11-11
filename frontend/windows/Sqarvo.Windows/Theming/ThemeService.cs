using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.Win32;
using System;
using System.Runtime.InteropServices;
using Windows.UI;
using Windows.UI.ViewManagement;

namespace Sqarvo.Windows.Theming;

/// <summary>用户在设置里选的界面主题。</summary>
public enum AppThemePreference
{
    /// <summary>跟随系统（默认；未选过时用这一档）。</summary>
    System = 0,

    /// <summary>浅色。</summary>
    Light = 1,

    /// <summary>暗色。</summary>
    Dark = 2,
}

/// <summary>
/// 主题三档（Light / Dark / HighContrast）的落点：启动期一个入口、运行期另一个入口。
/// </summary>
/// <remarks>
/// <para>
/// WinUI 3 的主题有两个时机，混用会抛异常或「改了没反应」：
/// </para>
/// <list type="bullet">
/// <item><c>Application.RequestedTheme</c> 只能在启动时（App 构造里）设置一次，
/// 运行中再设会抛 <see cref="NotSupportedException"/>；没选过主题就不要写，保持跟随系统。</item>
/// <item>运行中切换走元素级 <c>FrameworkElement.RequestedTheme</c>
/// （<see cref="ElementTheme.Default"/> / <c>Light</c> / <c>Dark</c>）。</item>
/// <item>标题栏不在 XAML 主题的作用域内，必须单独处理，否则暗色窗口顶上是白的
/// （Linux/macOS 没有这条，最容易漏）。跟随系统时用
/// <c>TitleBarTheme.UseDefaultAppMode</c>，让系统自己改，不依赖我们的通知。</item>
/// </list>
/// <para>
/// 高对比与强调色：<see cref="AccessibilitySettings.HighContrast"/> 在未打包的 WinUI 3
/// 应用里可能订阅不到事件（实测 <c>HighContrastChanged</c> 的订阅抛 COMException），
/// 因此这里做了两件事：每项各自独立 try/catch，以及高对比的兜底读取
/// （注册表 <c>HKCU\Control Panel\Accessibility\HighContrast\Flags</c> 的最低位）。
/// 订阅失败只影响「标签文字实时刷新」，不影响控件跟随系统主题——那是 WinUI 自己的事。
/// </para>
/// </remarks>
public static class ThemeService
{
    /// <summary>DWMWA_USE_IMMERSIVE_DARK_MODE（Win10 2004 起是 20）。</summary>
    private const int DwmUseImmersiveDarkMode = 20;

    /// <summary>Win10 2004 之前的编号。</summary>
    private const int DwmUseImmersiveDarkModeLegacy = 19;

    private static AccessibilitySettings? _accessibility;
    private static UISettings? _uiSettings;
    private static bool _started;

    /// <summary>系统主题 / 高对比 / 强调色变化（可能在非 UI 线程触发，订阅方自行回到 UI 线程）。</summary>
    public static event EventHandler? SystemAppearanceChanged;

    /// <summary>当前主题偏好。</summary>
    public static AppThemePreference Preference { get; private set; } = AppThemePreference.System;

    /// <summary>系统外观事件是否订阅成功（false = 运行中标签文字不自动刷新，主题仍生效）。</summary>
    public static bool IsListening { get; private set; }

    /// <summary>
    /// 启动期应用主题偏好。**必须**在第一个窗口创建之前调用。
    /// </summary>
    public static void Initialize(Application application, AppThemePreference preference)
    {
        ArgumentNullException.ThrowIfNull(application);

        Preference = preference;

        // 只在明确选了 Light/Dark 时写；System 不写（跟随系统）。
        if (preference == AppThemePreference.Dark)
        {
            application.RequestedTheme = ApplicationTheme.Dark;
        }
        else if (preference == AppThemePreference.Light)
        {
            application.RequestedTheme = ApplicationTheme.Light;
        }
    }

    /// <summary>
    /// 订阅系统外观变化。**不能**在 App 构造里调（那一刻 WinRT 未就绪，订阅会抛
    /// <c>E_UNEXPECTED</c>）；第一次调用放在 <c>OnLaunched</c>。
    /// </summary>
    public static void StartListening()
    {
        if (_started)
        {
            return;
        }

        _started = true;

        // 两项分开订阅：WinUI 3 未打包应用里高对比事件的订阅可能不可用，
        // 不能让它把强调色订阅一起拖垮。
        try
        {
            _accessibility = new AccessibilitySettings();
            _accessibility.HighContrastChanged += OnSystemAppearanceChanged;
            IsListening = true;
        }
        catch (Exception error)
        {
            Diagnostics.ConsoleBridge.WriteDiagnostic($"[theme] 高对比事件不可用（{error.GetType().Name}），改用注册表读取");
        }

        try
        {
            _uiSettings = new UISettings();
            _uiSettings.ColorValuesChanged += OnSystemAppearanceChanged;
            IsListening = true;
        }
        catch (Exception error)
        {
            Diagnostics.ConsoleBridge.WriteDiagnostic($"[theme] 强调色事件不可用（{error.GetType().Name}）");
        }
    }

    private static void OnSystemAppearanceChanged(AccessibilitySettings sender, object args)
        => RaiseSystemAppearanceChanged();

    private static void OnSystemAppearanceChanged(UISettings sender, object args)
        => RaiseSystemAppearanceChanged();

    private static void RaiseSystemAppearanceChanged()
        => SystemAppearanceChanged?.Invoke(null, EventArgs.Empty);

    /// <summary>系统当前是否处于高对比模式。</summary>
    public static bool HighContrast
    {
        get
        {
            try
            {
                if (_accessibility is not null)
                {
                    return _accessibility.HighContrast;
                }
            }
            catch (Exception)
            {
                // 落到注册表兜底。
            }

            return RegistryHighContrast();
        }
    }

    /// <summary>注册表兜底：<c>Flags</c> 最低位为 1 表示高对比开启。</summary>
    private static bool RegistryHighContrast()
    {
        try
        {
            var flags = Registry.GetValue(
                @"HKEY_CURRENT_USER\Control Panel\Accessibility\HighContrast",
                "Flags",
                0);
            return flags is int value && (value & 1) != 0;
        }
        catch (Exception)
        {
            return false;
        }
    }

    /// <summary>系统强调色（<c>#rrggbb</c>；读不到时返回 <c>(unknown)</c>，不编一个颜色）。</summary>
    public static string AccentHex
    {
        get
        {
            try
            {
                _uiSettings ??= new UISettings();
                return FormatColor(_uiSettings.GetColorValue(UIColorType.Accent));
            }
            catch (Exception)
            {
                return "(unknown)";
            }
        }
    }

    private static string FormatColor(Color color) => $"#{color.R:X2}{color.G:X2}{color.B:X2}";

    /// <summary>解析成元素级主题（运行中切换用这个，不碰 <c>Application.RequestedTheme</c>）。</summary>
    public static ElementTheme ResolveElementTheme()
        => Preference switch
        {
            AppThemePreference.Dark => ElementTheme.Dark,
            AppThemePreference.Light => ElementTheme.Light,
            _ => ElementTheme.Default,
        };

    /// <summary>把当前偏好应用到窗口内容与标题栏。</summary>
    public static void ApplyToWindow(Window window)
    {
        ArgumentNullException.ThrowIfNull(window);

        if (window.Content is FrameworkElement root)
        {
            root.RequestedTheme = ResolveElementTheme();
        }

        ApplyTitleBar(window);
    }

    /// <summary>
    /// 应用标题栏主题，返回实际写入的值（<c>Dark</c> / <c>Light</c> / <c>UseDefaultAppMode</c>），
    /// 供诊断输出。
    /// </summary>
    public static string ApplyTitleBar(Window window)
    {
        ArgumentNullException.ThrowIfNull(window);

        // 跟随系统时交给系统自己改（这样系统切主题时标题栏会跟着走，不需要我们的通知）。
        if (Preference == AppThemePreference.System)
        {
            try
            {
                window.AppWindow.TitleBar.PreferredTheme = TitleBarTheme.UseDefaultAppMode;
                return "UseDefaultAppMode";
            }
            catch (Exception)
            {
                // 旧系统 / 旧 SDK 没有该 API：用 DWM 兜底，按当前有效明暗写死一次。
            }
        }

        var dark = Preference == AppThemePreference.Dark
            || (Preference == AppThemePreference.System && IsDarkEffective(window));

        try
        {
            window.AppWindow.TitleBar.PreferredTheme = dark ? TitleBarTheme.Dark : TitleBarTheme.Light;
            return dark ? "Dark" : "Light";
        }
        catch (Exception)
        {
            // 同上：回退 DWM 属性，不把异常抛给用户。
        }

        var handle = WinRT.Interop.WindowNative.GetWindowHandle(window);
        var value = dark ? 1 : 0;
        if (DwmSetWindowAttribute(handle, DwmUseImmersiveDarkMode, ref value, sizeof(int)) != 0)
        {
            DwmSetWindowAttribute(handle, DwmUseImmersiveDarkModeLegacy, ref value, sizeof(int));
        }

        return dark ? "Dark" : "Light";
    }

    /// <summary>当前生效的是不是暗色（标题栏回退路径与诊断输出共用同一个判断）。</summary>
    public static bool IsDarkEffective(Window window)
    {
        if (Preference == AppThemePreference.Dark)
        {
            return true;
        }

        if (Preference == AppThemePreference.Light)
        {
            return false;
        }

        // System：以元素的 ActualTheme 为准（它已经把系统主题算进去了）。
        if (window.Content is FrameworkElement root && root.ActualTheme != ElementTheme.Default)
        {
            return root.ActualTheme == ElementTheme.Dark;
        }

        return Application.Current?.RequestedTheme == ApplicationTheme.Dark;
    }

    /// <summary>诊断输出用的偏好名（<c>System</c> / <c>Light</c> / <c>Dark</c>）。</summary>
    public static string PreferenceName => Preference switch
    {
        AppThemePreference.Dark => "Dark",
        AppThemePreference.Light => "Light",
        _ => "System",
    };

    /// <summary>把持久化里的主题名解析成偏好；无法识别时回退 System（跟随系统）。</summary>
    public static AppThemePreference ParsePreference(string? name) => name?.Trim().ToLowerInvariant() switch
    {
        "dark" => AppThemePreference.Dark,
        "light" => AppThemePreference.Light,
        _ => AppThemePreference.System,
    };

    [DllImport("dwmapi.dll", SetLastError = true)]
    private static extern int DwmSetWindowAttribute(IntPtr hwnd, int attribute, ref int value, int size);
}