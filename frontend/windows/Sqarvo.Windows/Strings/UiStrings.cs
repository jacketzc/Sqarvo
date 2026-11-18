// 新字符串目录（有界版本）：本目录只覆盖 Wave 1 新增 / 修改的用户可见文案。
// 存量内联文案（菜单旧项、tooltip、状态栏等未触碰的文本）待后续工作包迁移，本目录先覆盖新增字符串。
//
// 这里刻意用纯 C# 常量而不是 Microsoft.Windows.ApplicationModel.Resources.ResourceLoader：
// 本工作包范围为有界迁移，不引入 PRI/x:Uid 接线（需改 csproj 与全部静态 XAML Text），
// 用常量目录零依赖、零 API 风险。Ui.resw 是与本表一一对应的目录源文件，
// 后续接入 ResourceLoader / x:Uid 时可直接对照，无须再回头找散落的内联文案。
namespace Sqarvo.Windows.Strings;

/// <summary>Wave 1 新增用户可见文案的 key → 中文值目录。</summary>
public static class UiStrings
{
    // ------------------------------------------------------------ key（与 Ui.resw 的 name 一致）
    public const string ViewMenuText = "View.Menu.Text";
    public const string ViewThemeText = "View.Theme.Text";
    public const string ViewThemeSystem = "View.Theme.System";
    public const string ViewThemeLight = "View.Theme.Light";
    public const string ViewThemeDark = "View.Theme.Dark";
    public const string ViewDensityText = "View.Density.Text";
    public const string ViewDensityCompact = "View.Density.Compact";
    public const string ViewDensityComfortable = "View.Density.Comfortable";
    public const string ViewDensityLarge = "View.Density.Large";

    public const string DialogSubmitTitle = "Dialog.Submit.Title";
    public const string DialogSubmitConfirm = "Dialog.Submit.Confirm";
    public const string DialogSubmitCancelled = "Dialog.Submit.Cancelled";
    public const string DialogDeleteTitle = "Dialog.Delete.Title";
    public const string DialogDeleteBody = "Dialog.Delete.Body";
    public const string DialogDeleteConfirm = "Dialog.Delete.Confirm";
    public const string DialogDeleteCancelled = "Dialog.Delete.Cancelled";
    public const string DialogCancel = "Dialog.Cancel";

    public const string RowDeleteTooltip = "Row.Delete.Tooltip";

    // 数据提交正文模板（与 Workspace/DataModel.cs 的纯逻辑文案镜像；那一层保持零 WinUI / 零新依赖，
    // 便于 Linux 上的 win-data-probe 单独编译）。占位符顺序：{0}=行数/删除行数，{1}=阈值/说明。
    public const string DataSubmitRiskRows = "Data.Submit.RiskRows";
    public const string DataSubmitWithDeletes = "Data.Submit.WithDeletes";

    /// <summary>按 key 取中文值；未知 key 返回 key 本身（不编造文案，便于在诊断里发现漏项）。</summary>
    public static string Get(string key) => key switch
    {
        ViewMenuText => "视图",
        ViewThemeText => "深色模式",
        ViewThemeSystem => "跟随系统",
        ViewThemeLight => "浅色",
        ViewThemeDark => "深色",
        ViewDensityText => "界面密度",
        ViewDensityCompact => "紧凑",
        ViewDensityComfortable => "舒适",
        ViewDensityLarge => "大",

        DialogSubmitTitle => "确认提交行变更？",
        DialogSubmitConfirm => "确认提交",
        DialogSubmitCancelled => "已取消提交，未写入数据库",
        DialogDeleteTitle => "删除选中行？",
        DialogDeleteBody => "标记选中的行会随提交生效；提交前可用 Ctrl+Z 撤销。是否标记删除？",
        DialogDeleteConfirm => "标记删除",
        DialogDeleteCancelled => "已取消删除，未做任何标记",
        DialogCancel => "取消",

        RowDeleteTooltip => "标记删除选中行（随提交生效，提交前可撤销）",

        // 与 DataModel.cs 中同两条纯逻辑文案逐字一致。
        DataSubmitRiskRows => "本次提交 {0} 行{1}，超过风险阈值 100 行。请在确认对话框中复核后提交。",
        DataSubmitWithDeletes => "本次提交包含删除 {0} 行（删除不可撤销，随提交生效）。请在确认对话框中复核后提交。",

        _ => key,
    };

    /// <summary>带占位符的文案：string.Format 的薄封装。</summary>
    public static string Get(string key, params object[] args) => string.Format(Get(key), args);
}
