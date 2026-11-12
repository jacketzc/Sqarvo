<#
.SYNOPSIS
    从 docs/ui-specification/metrics.toml 生成 Metrics.g.cs。

.DESCRIPTION
    三端（Linux GTK / Windows WinUI 3 / macOS SwiftUI）的布局度量唯一真值是
    docs/ui-specification/metrics.toml（见 ADR 0026）。Windows 侧在**构建期**把它生成为
    C# 常量，不在运行时做「基准 × scale」：

        Rust  f64::round(12.5) -> 13   （half-away-from-zero）
        C#    Math.Round(12.5) -> 12   （half-to-even，默认）

    同一次乘法在两个语言里会差 1px，而且只在特定档位出现、肉眼几乎发现不了。所以这里
    只做「读整数、拼常量」，一行算术都不写。

    生成失败（文件缺失 / 三档键集不一致 / §3.3 不变量不成立）必须让构建变红：
    宁可构建失败，也不要静默漂移。

.PARAMETER TomlPath
    metrics.toml 的绝对路径。

.PARAMETER OutPath
    生成目标（通常在 obj/ 下，不提交）。

.NOTES
    只支持本文件实际用到的 TOML 子集：全局 `key = value`、`[section]` 表头、
    整数 / 浮点 / 布尔 / 单行字符串 / 单行数组，值后可带 `#` 注释。
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$TomlPath,
    [Parameter(Mandatory = $true)][string]$OutPath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Fail([string]$message) {
    Write-Host "gen-metrics: $message" -ForegroundColor Red
    exit 1
}

if (-not (Test-Path -LiteralPath $TomlPath)) {
    Fail "找不到度量真值 $TomlPath（缺 docs/ui-specification/metrics.toml，先 git pull）"
}

# ---------------------------------------------------------------------------
# 1. 读 TOML（只覆盖本文件用到的子集）
# ---------------------------------------------------------------------------

function ConvertFrom-TomlValue([string]$text, [string]$context) {
    $value = $text.Trim()

    if ($value -match '^\[(?<items>.*)\]$') {
        $items = $Matches.items.Trim()
        if ($items -eq '') { return , @() }
        return , @($items.Split(',') | ForEach-Object { ConvertFrom-TomlValue $_.Trim() $context })
    }
    if ($value -match '^"(?<s>.*)"$') { return $Matches.s }
    if ($value -eq 'true') { return $true }
    if ($value -eq 'false') { return $false }
    if ($value -match '^-?[0-9]+$') { return [long]$value }
    if ($value -match '^-?[0-9]+\.[0-9]+$') { return [double]::Parse($value, [Globalization.CultureInfo]::InvariantCulture) }
    Fail "无法解析 $context 的值 '$text'"
}

$global = [ordered]@{}
$sections = [ordered]@{}
$current = $null

$lineNumber = 0
foreach ($rawLine in [IO.File]::ReadAllLines($TomlPath, [Text.Encoding]::UTF8)) {
    $lineNumber++
    $line = $rawLine
    $hashIndex = $line.IndexOf('#')
    if ($hashIndex -ge 0) { $line = $line.Substring(0, $hashIndex) }
    $line = $line.Trim()
    if ($line -eq '') { continue }

    if ($line -match '^\[(?<name>[^\]]+)\]$') {
        $name = $Matches.name.Trim()
        $current = [ordered]@{}
        $sections[$name] = $current
        continue
    }

    if ($line -notmatch '^(?<key>[A-Za-z0-9_]+)\s*=\s*(?<value>.+)$') {
        Fail "第 $lineNumber 行无法解析：'$rawLine'"
    }
    $key = $Matches.key
    $value = ConvertFrom-TomlValue $Matches.value "第 $lineNumber 行"
    if ($null -eq $current) { $global[$key] = $value } else { $current[$key] = $value }
}

# ---------------------------------------------------------------------------
# 2. 取三档，校验
# ---------------------------------------------------------------------------

$tierOrder = @('compact', 'comfortable', 'large')
$tierDisplay = @{ compact = 'Compact'; comfortable = 'Comfortable'; large = 'Large' }

foreach ($tier in $tierOrder) {
    if (-not $sections.Contains("tier.$tier")) {
        Fail "metrics.toml 缺少 [tier.$tier]：三档必须齐全（ADR 0026）"
    }
}

$tiers = @{}
foreach ($tier in $tierOrder) {
    $table = $sections["tier.$tier"]
    $sizes = [ordered]@{}
    foreach ($key in $table.Keys) {
        if ($key -in @('id', 'label', 'scale')) { continue }
        if ($table[$key] -isnot [long]) {
            Fail "[tier.$tier].$key 必须是整数（三端只读整数真值，不做运行时换算）"
        }
        $sizes[$key] = [int]$table[$key]
    }
    $tiers[$tier] = [pscustomobject]@{
        Id     = $tier
        Label  = [string]$table['label']
        Scale  = [double]$table['scale']
        Sizes  = $sizes
    }
}

$referenceKeys = @($tiers['compact'].Sizes.Keys)
foreach ($tier in $tierOrder) {
    $keys = @($tiers[$tier].Sizes.Keys)
    $missing = $referenceKeys | Where-Object { $_ -notin $keys }
    $extra = $keys | Where-Object { $_ -notin $referenceKeys }
    if ($missing -or $extra) {
        Fail ("三档键集不一致：[tier.$tier] 缺 {$($missing -join ', ')}，多 {$($extra -join ', ')}；" +
              "加档位/加键必须三档同时补（metrics.toml）")
    }
}

# §3.3 不变量：每档都必须成立，写在生成器里，让构建替我们守住。
$invariantChecks = @(
    @{ Name = 'row_height_tree = min_target = icon_button_height'; Ok = { param($s) $s.row_height_tree -eq $s.min_target -and $s.row_height_tree -eq $s.icon_button_height }; Detail = { param($s) "row_height_tree=$($s.row_height_tree) min_target=$($s.min_target) icon_button_height=$($s.icon_button_height)" } },
    @{ Name = 'font_title > font_ui'; Ok = { param($s) $s.font_title -gt $s.font_ui }; Detail = { param($s) "font_title=$($s.font_title) font_ui=$($s.font_ui)" } },
    @{ Name = 'font_data_cell >= 13 且 >= font_ui'; Ok = { param($s) $s.font_data_cell -ge 13 -and $s.font_data_cell -ge $s.font_ui }; Detail = { param($s) "font_data_cell=$($s.font_data_cell) font_ui=$($s.font_ui)" } },
    @{ Name = 'font_panel <= font_ui'; Ok = { param($s) $s.font_panel -le $s.font_ui }; Detail = { param($s) "font_panel=$($s.font_panel) font_ui=$($s.font_ui)" } },
    @{ Name = 'icon_button_width - icon_box >= 4'; Ok = { param($s) ($s.icon_button_width - $s.icon_box) -ge 4 }; Detail = { param($s) "icon_button_width=$($s.icon_button_width) icon_box=$($s.icon_box)" } },
    @{ Name = 'icon_button_height <= row_height_tree'; Ok = { param($s) $s.icon_button_height -le $s.row_height_tree }; Detail = { param($s) "icon_button_height=$($s.icon_button_height) row_height_tree=$($s.row_height_tree)" } },
    @{ Name = 'sidebar_min < sidebar_width < sidebar_max'; Ok = { param($s) $s.sidebar_min -lt $s.sidebar_width -and $s.sidebar_width -lt $s.sidebar_max }; Detail = { param($s) "sidebar_min=$($s.sidebar_min) sidebar_width=$($s.sidebar_width) sidebar_max=$($s.sidebar_max)" } },
    @{ Name = 'inspector_min < inspector_width < inspector_max'; Ok = { param($s) $s.inspector_min -lt $s.inspector_width -and $s.inspector_width -lt $s.inspector_max }; Detail = { param($s) "inspector_min=$($s.inspector_min) inspector_width=$($s.inspector_width) inspector_max=$($s.inspector_max)" } },
    @{ Name = 'row_height_data > row_height_tree'; Ok = { param($s) $s.row_height_data -gt $s.row_height_tree }; Detail = { param($s) "row_height_data=$($s.row_height_data) row_height_tree=$($s.row_height_tree)" } }
)

foreach ($tier in $tierOrder) {
    $sizes = $tiers[$tier].Sizes
    foreach ($check in $invariantChecks) {
        if (-not (& $check.Ok $sizes)) {
            Fail "不变量「$($check.Name)」在 [tier.$tier] 不成立（$(& $check.Detail $sizes)）：见 scaling-and-window-sizing.md §3.3"
        }
    }
}

# ---------------------------------------------------------------------------
# 3. 生成 C#
# ---------------------------------------------------------------------------

function ConvertTo-PascalCase([string]$key) {
    $parts = $key.Split('_') | Where-Object { $_ -ne '' }
    return ($parts | ForEach-Object { $_.Substring(0, 1).ToUpperInvariant() + $_.Substring(1) } ) -join ''
}

function Format-Number([double]$value) {
    $text = $value.ToString('0.######', [Globalization.CultureInfo]::InvariantCulture)
    return $text
}

# 不用 Get-FileHash：GitHub 的 windows-latest 上这一步实测报「找不到 Get-FileHash」
# （CommandNotFoundException，2026-11-12 CI 第一次跑就红）。改用 .NET 直接算，不依赖 cmdlet。
$sha256 = [Security.Cryptography.SHA256]::Create()
try {
    $bytes = [IO.File]::ReadAllBytes($TomlPath)
    $sha = (([BitConverter]::ToString($sha256.ComputeHash($bytes))) -replace '-', '').ToLowerInvariant()
}
finally {
    $sha256.Dispose()
}

# [meta] 段：默认档位、参考窗口、不缩放的 1px 描边。
if (-not $sections.Contains('meta')) {
    Fail "metrics.toml 缺少 [meta] 段"
}
$meta = $sections['meta']
$defaultTier = [string]$meta['default_tier']
if ($defaultTier -eq '') { $defaultTier = 'compact' }
$referenceWindow = @($meta['layout_reference_window'])
if ($referenceWindow.Count -ne 2) {
    Fail "[meta].layout_reference_window 必须是 [宽, 高] 两个整数"
}
$borderNeverScaled = $meta['border_width_px_never_scaled']
if ($null -eq $borderNeverScaled) { $borderNeverScaled = 1 }
$tomlVersion = $global['version']
if ($null -eq $tomlVersion) { $tomlVersion = 1 }

$builder = [Text.StringBuilder]::new()
$newline = "`r`n"

function Add-Line([string]$text = '') {
    [void]$builder.Append($text).Append($newline)
}

Add-Line '// <auto-generated>'
Add-Line '//   本文件由 frontend/windows/tools/gen-metrics.ps1 从 docs/ui-specification/metrics.toml 生成。'
Add-Line '//   不要手改：改数字请改 metrics.toml（三端同一份真值），改生成规则请改生成器。'
Add-Line "//   metrics.toml sha256 = $sha"
Add-Line '//'
Add-Line '//   为什么不在运行时算：C# 的 Math.Round 默认 half-to-even，Rust 的 f64::round 是'
Add-Line '//   half-away-from-zero，同一份「基准 × scale」在两端会差 1px（见 ADR 0026 §决策 3）。'
Add-Line '// </auto-generated>'
Add-Line '#nullable enable'
Add-Line
Add-Line 'using System;'
Add-Line
Add-Line 'namespace Sqarvo.Windows.Theming;'
Add-Line
Add-Line '/// <summary>界面密度档（三档，见 ADR 0026）。</summary>'
Add-Line 'public enum DensityTier'
Add-Line '{'
Add-Line '    /// <summary>紧凑（100%）：默认档。</summary>'
Add-Line '    Compact = 0,'
Add-Line
Add-Line '    /// <summary>舒适（125%）。</summary>'
Add-Line '    Comfortable = 1,'
Add-Line
Add-Line '    /// <summary>大（150%）。</summary>'
Add-Line '    Large = 2,'
Add-Line '}'
Add-Line
Add-Line '/// <summary>'
Add-Line '/// 三档布局度量真值：构建期从 metrics.toml 生成，运行时只查表、不做乘法。'
Add-Line '/// 同时是 XAML 资源键 <c>Size.*</c> 的唯一来源（见 <see cref="ForEachSizeToken"/>）。'
Add-Line '/// </summary>'
Add-Line 'public static partial class Metrics'
Add-Line '{'
Add-Line '    /// <summary>metrics.toml 的 version 字段。</summary>'
Add-Line "    public const int TomlVersion = $tomlVersion;"
Add-Line
Add-Line '    /// <summary>生成时 metrics.toml 的 sha256（用于核对生成文件是否过期）。</summary>'
Add-Line "    public const string SourceSha256 = `"$sha`";"
Add-Line
Add-Line '    /// <summary>默认档位 id（metrics.toml 的 [meta] default_tier）。</summary>'
Add-Line "    public const string DefaultTierId = `"$defaultTier`";"
Add-Line
Add-Line '    /// <summary>1px 描边在所有档位都保持 1px（见 scaling-and-window-sizing.md §3.2）。</summary>'
Add-Line "    public const int BorderWidthPxNeverScaled = $borderNeverScaled;"
Add-Line
Add-Line '    /// <summary>规范参考窗口（原型/截图基线，不是默认窗口）。</summary>'
$reference = $referenceWindow
Add-Line "    public const int ReferenceWindowWidth = $($reference[0]);"
Add-Line "    public const int ReferenceWindowHeight = $($reference[1]);"
Add-Line

foreach ($tier in $tierOrder) {
    Add-Line "    // ---- [tier.$tier] $($tiers[$tier].Label) ----"
    Add-Line
    Add-Line "    /// <summary>[tier.$tier] 的密度系数（只用于归一化持久化，不用于算像素）。</summary>"
    Add-Line "    public const double $($tierDisplay[$tier])Scale = $(Format-Number $tiers[$tier].Scale);"
    Add-Line
    Add-Line "    /// <summary>[tier.$tier] 的中文标签。</summary>"
    Add-Line "    public const string $($tierDisplay[$tier])Label = `"$($tiers[$tier].Label)`";"
    Add-Line
    foreach ($key in $referenceKeys) {
        Add-Line "    public const int $($tierDisplay[$tier])$(ConvertTo-PascalCase $key) = $($tiers[$tier].Sizes[$key]);"
    }
    Add-Line
}

Add-Line '    private static DensityTier _tier = DensityTier.Compact;'
Add-Line
Add-Line '    /// <summary>当前生效的档位。</summary>'
Add-Line '    public static DensityTier Tier => _tier;'
Add-Line
Add-Line '    /// <summary>切换档位。改完必须重建页面（StaticResource 不会重算，见 ADR 0026）。</summary>'
Add-Line '    public static void SetTier(DensityTier tier) => _tier = tier;'
Add-Line
Add-Line '    /// <summary>清掉运行期状态，回到 metrics.toml 的 default_tier。</summary>'
Add-Line '    public static void ClearValues() => _tier = DensityTier.Compact;'
Add-Line
Add-Line '    /// <summary>当前档位的中文标签。</summary>'
Add-Line '    public static string TierLabel => _tier switch'
Add-Line '    {'
Add-Line '        DensityTier.Comfortable => ComfortableLabel,'
Add-Line '        DensityTier.Large => LargeLabel,'
Add-Line '        _ => CompactLabel,'
Add-Line '    };'
Add-Line
Add-Line '    /// <summary>当前档位的中文 id（用于 window.json 的 tier 字段）。</summary>'
Add-Line '    public static string TierId => _tier switch'
Add-Line '    {'
Add-Line '        DensityTier.Comfortable => "comfortable",'
Add-Line '        DensityTier.Large => "large",'
Add-Line '        _ => "compact",'
Add-Line '    };'
Add-Line
Add-Line '    /// <summary>把持久化里的档位 id 解析成档位；无法识别时回退默认档。</summary>'
Add-Line '    public static DensityTier ParseTier(string? id) => id?.Trim().ToLowerInvariant() switch'
Add-Line '    {'
Add-Line '        "comfortable" => DensityTier.Comfortable,'
Add-Line '        "large" => DensityTier.Large,'
Add-Line '        _ => DensityTier.Compact,'
Add-Line '    };'
Add-Line

foreach ($key in $referenceKeys) {
    $pascal = ConvertTo-PascalCase $key
    Add-Line "    /// <summary>metrics.toml 的 <c>$key</c>（按当前档位取值）。</summary>"
    Add-Line "    public static int $pascal => _tier switch"
    Add-Line '    {'
    Add-Line "        DensityTier.Comfortable => Comfortable$pascal,"
    Add-Line "        DensityTier.Large => Large$pascal,"
    Add-Line "        _ => Compact$pascal,"
    Add-Line '    };'
    Add-Line
}

Add-Line '    /// <summary>'
Add-Line '    /// 把当前档位的尺寸写成 XAML 资源键（<c>Size.&lt;Key&gt;</c>，值为 <see cref="double"/>）。'
Add-Line '    /// Tokens.xaml 与页面 XAML 只引用这些键，不出现字面尺寸。'
Add-Line '    /// </summary>'
Add-Line '    public static void ForEachSizeToken(Action<string, double> sink)'
Add-Line '    {'
Add-Line '        ArgumentNullException.ThrowIfNull(sink);'
foreach ($key in $referenceKeys) {
    $pascal = ConvertTo-PascalCase $key
    Add-Line "        sink(`"Size.$pascal`", $pascal);"
}
Add-Line '    }'
Add-Line '}'

$content = $builder.ToString()

$outDir = Split-Path -Parent $OutPath
if ($outDir -and -not (Test-Path -LiteralPath $outDir)) {
    New-Item -ItemType Directory -Force -Path $outDir | Out-Null
}

# 只在内容变化时写，避免无谓的重编译。
$existing = $null
if (Test-Path -LiteralPath $OutPath) {
    $existing = [IO.File]::ReadAllText($OutPath, [Text.Encoding]::UTF8)
}
if ($existing -ne $content) {
    # UTF8 带 BOM：避免源文件编码猜测（生成内容含中文）。
    [IO.File]::WriteAllText($OutPath, $content, [Text.UTF8Encoding]::new($true))
    Write-Host "gen-metrics: 已生成 $OutPath（$($referenceKeys.Count) 个键 × 3 档）"
}
else {
    Write-Host "gen-metrics: 无变化 $OutPath"
}
exit 0