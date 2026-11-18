using Microsoft.UI.Composition.SystemBackdrops;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
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
/// <item>窗口材质（Mica）是 Windows 11 的平台原生能力，走 <c>Window.SystemBackdrop</c>，
/// 不是 XAML 画出来的：它只在「上面没有不透明的东西」时才透出来，所以除了挂材质，
/// 还要把窗口根元素的实色底换成透明。版本门槛与回退见 <see cref="ApplyBackdrop"/>。</item>
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

    /// <summary>最近一次应用到的窗口（启动期由 App 调 ApplyToWindow 时记下，供运行期切换复用）。</summary>
    private static Window? _window;

    /// <summary>
    /// 窗口层注册的即时落盘回调。<see cref="SetPreference"/> 只发通知，
    /// <c>window.json</c> 的唯一写入方仍是持有 <c>WindowState</c> 的 MainWindow——
    /// 这样切主题与切档 / 关窗写盘不会互相覆盖。
    /// </summary>
    public static Action<AppThemePreference>? PreferenceWriter { get; set; }

    /// <summary>系统主题 / 高对比 / 强调色变化（可能在非 UI 线程触发，订阅方自行回到 UI 线程）。</summary>
    public static event EventHandler? SystemAppearanceChanged;

    /// <summary>当前主题偏好。</summary>
    public static AppThemePreference Preference { get; private set; } = AppThemePreference.System;

    /// <summary>系统外观事件是否订阅成功（false = 运行中标签文字不自动刷新，主题仍生效）。</summary>
    public static bool IsListening { get; private set; }

    /// <summary>
    /// 当前窗口材质名（诊断用）：<c>Mica</c> = 已挂 Windows 11 材质；
    /// <c>Solid</c> = 实色底（Windows 10，或材质没挂上）。
    /// </summary>
    public static string BackdropName { get; private set; } = "Solid";

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
    /// 运行期切换主题偏好：写字段 → 即时持久化（经窗口层回调）→ 应用到窗口内容与标题栏。
    /// </summary>
    /// <remarks>
    /// 不碰 <c>Application.RequestedTheme</c>（运行中设会抛 <see cref="NotSupportedException"/>）：
    /// 元素级主题由 <see cref="ApplyToWindow"/> 处理，它内部同时同步标题栏（<see cref="ApplyTitleBar"/>）。
    /// </remarks>
    public static void SetPreference(AppThemePreference pref)
    {
        if (Preference == pref)
        {
            return;
        }

        Preference = pref;

        try
        {
            PreferenceWriter?.Invoke(pref);
        }
        catch (Exception error)
        {
            // 写盘失败不该让界面切换失败；窗口状态是便利功能。
            Diagnostics.ConsoleBridge.WriteDiagnostic(
                $"[theme] 主题偏好落盘失败（{error.GetType().Name}）：{error.Message}");
        }

        if (_window is { } window)
        {
            ApplyToWindow(window);
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

        _window = window;

        if (window.Content is FrameworkElement root)
        {
            root.RequestedTheme = ResolveElementTheme();
        }

        ApplyBackdrop(window);

        ApplyTitleBar(window);
    }

    /// <summary>
    /// 给窗口挂 Windows 11 的 Mica 材质，并把窗口根元素的实色底换成透明。
    /// </summary>
    /// <remarks>
    /// <para>
    /// 材质是平台原生能力（DWM 合成），不是 XAML 画出来的：它只在「上面没有不透明的东西盖着」
    /// 的地方透得出来。所以这一步做两件事——挂 <see cref="MicaBackdrop"/>，再把窗口根元素
    /// （<c>MainWindow.xaml</c> 的 RootGrid）那份不透明底去掉。根元素之外的分层（band 层用
    /// <c>LayerFillColorDefaultBrush</c>、内容层用 <c>CardBackgroundFillColorDefaultBrush</c>）
    /// 本来就是 Explorer 在材质之上的分层口径，一支刷子都不用改。
    /// </para>
    /// <para>
    /// 版本门槛：Mica 本身要 Windows 11（build 22000+）；<c>MicaKind.BaseAlt</c>（资源管理器
    /// 用的那一档）要 22621（22H2），22621 以下用 <c>MicaKind.Base</c>。这里用
    /// <see cref="OperatingSystem.IsWindowsVersionAtLeast"/> 判断，不用
    /// <c>MicaBackdrop.IsSupported()</c>。
    /// </para>
    /// <para>
    /// 回退交给平台，不自己造：Windows 10、用户关掉「透明效果」、硬件不支持、远程桌面时，
    /// WinUI 会自动把材质退成实色主题色；高对比下系统直接抑制材质并改用用户的高对比底色。
    /// 所以 Windows 10 这一路直接返回（什么都不改，实色底原样保留），高对比也不写特判；
    /// OS 版本运行中不会变，因此不需要任何恢复逻辑。
    /// </para>
    /// </remarks>
    private static void ApplyBackdrop(Window window)
    {
        // Windows 10：没有 Mica，也不做任何改动，RootGrid 的实色底原样保留（与改动前一致）。
        if (!OperatingSystem.IsWindowsVersionAtLeast(10, 0, 22000))
        {
            return;
        }

        try
        {
            window.SystemBackdrop = new MicaBackdrop
            {
                Kind = OperatingSystem.IsWindowsVersionAtLeast(10, 0, 22621)
                    ? MicaKind.BaseAlt
                    : MicaKind.Base,
            };
        }
        catch (Exception error)
        {
            // 挂不上就维持实色底：材质是锦上添花，不值得为它让窗口起不来。
            Diagnostics.ConsoleBridge.WriteDiagnostic(
                $"[theme] Mica 材质不可用（{error.GetType().Name}），维持实色底");
            return;
        }

        // 材质要透出来，窗口根元素那份不透明底就得让位：底色改交给窗口背景层
        //（材质本身，或平台在不支持时的实色回退）。
        // Background 不是 FrameworkElement 的属性（只在 Control / Panel 上），
        // 窗口内容根是 MainWindow.xaml 的 RootGrid（Grid → Panel）。
        if (window.Content is Panel root)
        {
            root.Background = new SolidColorBrush(Microsoft.UI.Colors.Transparent);
        }

        BackdropName = "Mica";
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