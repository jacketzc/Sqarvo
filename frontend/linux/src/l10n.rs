//! 运行时文案目录（ADR 0031：界面国际化，S1/#199 机制层）。
//!
//! - [`lookup`] / [`lookup_one`] 与 `LANG_ZH` / `LANG_EN` / `L10N_KEY_COUNT` 由
//!   `build.rs` 构建期从仓库根 `l10n/*.toml` 生成进 `OUT_DIR/l10n_catalog.rs`：
//!   静态 match 查找、无运行时解析，生成失败 = 构建失败（决策 3）；
//! - 回落链：当前语言 → `en` → `zh` → key 本身，永不空白（决策 4）；
//! - 插值只做 `{name}` 命名占位、复数只做 one/other 两态（决策 5）：[`tf`] / [`t_n`]；
//! - 语言偏好三档（跟随系统 / 简体中文 / English）复刻 [`crate::theme::Scheme`] 的
//!   形态，持久化走 `window.json` 的 `language` 字段，**切换重启生效**（决策 6，
//!   菜单接线与 respawn 见 `ui.rs`）；
//! - 日志行（`eprintln!`）不进 catalog，保持中文（决策 1 的配套口径）。

use std::cell::Cell;
use std::sync::atomic::{AtomicU8, Ordering};

include!(concat!(env!("OUT_DIR"), "/l10n_catalog.rs"));

/// 语言偏好（主菜单「视图 → 语言」）。
///
/// 三档存进 `window.json` 的 `language` 字段（`system` / `zh` / `en`，与 `theme`
/// 同款兼容口径：缺 = 默认，加字段不升版本）：
///
/// - `System`（默认）：跟随系统，映射见 [`system_lang`]；
/// - `Zh` / `En`：明确选择，不再被系统偏好改动。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    /// 跟随系统。
    #[default]
    System,
    /// 简体中文。
    Zh,
    /// English。
    En,
}

impl Lang {
    /// 菜单顺序：跟随系统 → 简体中文 → English。
    pub const ALL: [Self; 3] = [Self::System, Self::Zh, Self::En];

    /// `window.json` / `--lang` 里的偏好 id。
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Zh => "zh",
            Self::En => "en",
        }
    }

    /// 解析偏好 id；无法识别时返回 `None`（调用方回退跟随系统，不静默改写文件）。
    ///
    /// 与 `theme::Scheme::from_id` 同形态：容忍大小写与首尾空白。
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        match id.trim().to_ascii_lowercase().as_str() {
            "system" => Some(Self::System),
            "zh" => Some(Self::Zh),
            "en" => Some(Self::En),
            _ => None,
        }
    }

    /// 日志里的人话名字（菜单文案走 [`menu_key`] + [`t`]，不进这里；
    /// 日志保持中文，见模块头）。
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::System => "跟随系统",
            Self::Zh => "简体中文",
            Self::En => "English",
        }
    }

    /// 「视图 → 语言」小节条目的 catalog key（S1 首批条目，`l10n/shell.toml`）。
    #[must_use]
    pub fn menu_key(self) -> &'static str {
        match self {
            Self::System => "shell.menu.lang_system",
            Self::Zh => "shell.menu.lang_zh",
            Self::En => "shell.menu.lang_en",
        }
    }

    /// 把偏好折叠成生效语言：`System` 按 [`system_lang`] 解析，其余原样。
    #[must_use]
    pub fn effective(self) -> Lang {
        match self {
            Self::System => system_lang(),
            other => other,
        }
    }

    /// catalog 查找用的 locale 编号（`System` 必须先经 [`effective`] 折叠）。
    fn locale(self) -> u8 {
        match self {
            Self::Zh => LANG_ZH,
            // En 与（折叠后不应出现的）System 都走英文：缺 zh 条目的回落链仍兜底。
            Self::En | Self::System => LANG_EN,
        }
    }
}

// 偏好与生效语言（启动路径 `ui::activate` 在**建菜单之前**写入一次）。
//
// 偏好与「本线程预览生效值」与 `theme::SCHEME` 同理放 `thread_local`：只有主线程
// 的菜单与对话框构建读它们。**进程启动时 resolve 的生效语言是原子全局**（#200
// 遗留修复）：config.rs 保存失败等状态消息在 worker 线程构造，thread_local 会让
// 它们永远读到默认中文。分工——
// · `set_startup_language`：启动路径**唯一**一次写全局，先于任何线程创建
//   （线程创建自带 happens-before），worker 之后只读这个值；
// · `set_current`：菜单点选 / 「稍后」回滚，只写主线程的预览覆盖，**不动全局**
//   ——与 ADR 0031 决策 6「切换重启生效」一致：不重启，worker 消息保持旧语言；
//   重启（respawn 新进程）后新启动 resolve 接管。
// 未显式设置时生效值按 `Zh`——zh 是 source of truth，单测里直接构造菜单也拿到
// 与迁移前逐字节相同的中文；生产路径总在启动时 resolve，不受这个默认影响。
thread_local! {
    static PREFERENCE: Cell<Lang> = const { Cell::new(Lang::System) };
    static PREVIEW: Cell<Option<Lang>> = const { Cell::new(None) };
}

/// 进程级生效语言（只存 `LANG_ZH` / `LANG_EN`；启动写一次后只读，`Relaxed` 足够）。
static EFFECTIVE: AtomicU8 = AtomicU8::new(LANG_ZH);

/// 当前语言偏好（菜单勾选态读这个，三档原样）。
#[must_use]
pub fn preference() -> Lang {
    PREFERENCE.with(Cell::get)
}

/// 当前生效语言（只会是 `Zh` / `En`）。主线程被菜单点选预览覆盖时用预览值，
/// 其余线程读启动 resolve 的进程级值。
#[must_use]
pub fn current() -> Lang {
    match PREVIEW.with(Cell::get) {
        Some(preview) => preview,
        None if EFFECTIVE.load(Ordering::Relaxed) == LANG_EN => Lang::En,
        None => Lang::Zh,
    }
}

/// 启动路径设置语言偏好并写进程级生效值。**只允许在 `ui::activate` 建菜单之前
/// 调用一次**（先于任何 worker / 线程创建），单测里模拟启动同理。
pub fn set_startup_language(preference: Lang) {
    PREFERENCE.with(|slot| slot.set(preference));
    let effective = preference.effective();
    EFFECTIVE.store(effective.locale(), Ordering::Relaxed);
    PREVIEW.with(|slot| slot.set(None));
    log_language(preference, effective);
}

/// 菜单点选 / 回滚：更新本线程偏好与生效预览，**不改进程级生效语言**（重启才
/// 全局接管，ADR 0031 决策 6）。
pub fn set_current(preference: Lang) {
    PREFERENCE.with(|slot| slot.set(preference));
    let effective = preference.effective();
    PREVIEW.with(|slot| slot.set(Some(effective)));
    log_language(preference, effective);
}

fn log_language(preference: Lang, effective: Lang) {
    eprintln!(
        "[l10n] 语言偏好：{}（{}；生效={}）",
        preference.label(),
        preference.id(),
        effective.label()
    );
}

/// 「跟随系统」的映射（ADR 0031 决策 6）：环境变量 `LC_ALL` / `LC_MESSAGES` /
/// `LANG` 任一根值以 `zh` 开头 → 中文；否则 → English。
///
/// 解析不到（三个变量都没设、空值、`C` / `POSIX`）按 **English** 处理——映射按
/// 「非中文即英文」写死，只服务中英双语；第三语言出现时必须整体重审
/// （ADR 0031「重估条件」）。
#[must_use]
pub fn system_lang() -> Lang {
    for name in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        let Ok(value) = std::env::var(name) else {
            continue;
        };
        let value = value.trim().to_ascii_lowercase();
        if value.is_empty() {
            continue;
        }
        if value.starts_with("zh") {
            return Lang::Zh;
        }
    }
    Lang::En
}

/// 取一条界面文案。回落链：当前语言 → `en` → `zh` → key 本身（决策 4，永不空白）。
///
/// key 只可能是编译期字面量（catalog 由 `l10n/*.toml` 生成），所以签名收
/// `&'static str`：回落「key 本身」时才能继续返回 `&'static str`，界面构造期
/// 缓存文案不需要任何拷贝。
#[must_use]
pub fn t(key: &'static str) -> &'static str {
    lookup(current().locale(), key)
        .or_else(|| lookup(LANG_EN, key))
        .or_else(|| lookup(LANG_ZH, key))
        .unwrap_or(key)
}

/// 取一条带 `{name}` 命名占位符的文案并填参（决策 5；只替换给定的名字）。
#[must_use]
pub fn tf(key: &'static str, args: &[(&str, String)]) -> String {
    expand(t(key), args)
}

/// 复数条目（one/other 两态）：中文恒用 `zh`；英文 `n == 1` 且有 `en_one` 用 one
/// 形态，否则 other 形态（缺 `en_one` = 该 key 无复数，schema 见 `l10n/README.md`）。
#[must_use]
pub fn t_n(key: &'static str, n: i64) -> &'static str {
    if current() == Lang::Zh {
        return t(key);
    }
    if n == 1
        && let Some(one) = lookup_one(LANG_EN, key)
    {
        return one;
    }
    t(key)
}

/// 复数 + 命名占位：先按 [`t_n`] 选形态，再按 [`tf`] 填参（「已选中 {n} 行」类）。
#[must_use]
pub fn tf_n(key: &'static str, n: i64, args: &[(&str, String)]) -> String {
    expand(t_n(key, n), args)
}

/// 命名占位替换（`tf` 的纯函数内核，单测直接打这里）。
#[allow(dead_code)]
fn expand(template: &str, args: &[(&str, String)]) -> String {
    let mut out = template.to_owned();
    for (name, value) in args {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 生效语言的进程级部分（#200 遗留修复）：启动写入后，spawn 的线程里
    /// `t()` 必须返回同一语言。另证菜单 `set_current` 只覆盖本线程预览、
    /// 不影响其他线程（重启才全局接管）。改动全局生效值，标 `#[ignore]` 单跑。
    #[test]
    #[ignore = "改动进程级生效语言全局，须单独运行：cargo test l10n -- --ignored"]
    fn effective_language_is_visible_from_worker_threads() {
        set_startup_language(Lang::En);
        let child =
            std::thread::spawn(|| (crate::l10n::t("shell.menu.file"), crate::l10n::current()));
        let (from_child, child_lang) = child.join().expect("子线程不应 panic");
        assert_eq!(
            from_child, "File",
            "worker 线程应读到启动语言（英文），实际 {from_child}"
        );
        assert_eq!(child_lang, Lang::En);
        // 菜单点选只写本线程预览：本线程读到中文预览，另一个新线程仍读全局英文。
        set_current(Lang::Zh);
        let again = std::thread::spawn(|| crate::l10n::t("shell.menu.file"))
            .join()
            .unwrap();
        assert_eq!(t("shell.menu.file"), "文件", "本线程预览覆盖为中文");
        assert_eq!(
            again, "File",
            "set_current 不改进程级全局，其他线程仍读英文"
        );
        set_startup_language(Lang::Zh);
        assert_eq!(
            t("shell.menu.file"),
            "文件",
            "启动写入清掉预览后恢复默认查找"
        );
    }

    #[test]
    fn lang_ids_round_trip_with_whitespace_and_case() {
        for lang in Lang::ALL {
            let messy = format!("  {}  ", lang.id().to_uppercase());
            assert_eq!(Lang::from_id(&messy), Some(lang));
        }
        assert_eq!(Lang::from_id("de"), None);
        assert_eq!(Lang::from_id(""), None);
    }

    #[test]
    fn shell_entries_carry_the_language_menu_copy() {
        set_current(Lang::Zh);
        assert_eq!(t("shell.menu.view_language"), "语言");
        assert_eq!(t("shell.menu.lang_system"), "跟随系统");
        assert_eq!(t("shell.menu.lang_zh"), "简体中文");
        assert_eq!(t("shell.menu.lang_en"), "English");
        assert_eq!(preference(), Lang::Zh);
        assert_eq!(current(), Lang::Zh);
        set_current(Lang::En);
        assert_eq!(t("shell.menu.view_language"), "Language");
        // 语言名自译：英文界面下「简体中文」条目仍是自译名。
        assert_eq!(t("shell.menu.lang_zh"), "简体中文");
    }

    #[test]
    fn unknown_key_falls_back_to_itself() {
        set_current(Lang::En);
        assert_eq!(t("shell.menu.nope"), "shell.menu.nope");
        set_current(Lang::Zh);
        assert_eq!(t("shell.menu.nope"), "shell.menu.nope");
    }

    #[test]
    fn system_preference_collapses_to_env_mapping() {
        set_current(Lang::System);
        assert_eq!(current(), system_lang());
        assert!(matches!(system_lang(), Lang::Zh | Lang::En));
        // 无论映射到哪边，首批条目两端都有译文——永不空白。
        assert_ne!(t("shell.menu.view_language"), "shell.menu.view_language");
    }

    #[test]
    fn expand_replaces_named_placeholders_only() {
        assert_eq!(
            expand("共 {n} 行（{n}）", &[("n", "12".to_owned())]),
            "共 12 行（12）"
        );
        assert_eq!(
            expand("{missing}", &[("other", "x".to_owned())]),
            "{missing}"
        );
        assert_eq!(expand("无占位", &[("n", "1".to_owned())]), "无占位");
    }

    #[test]
    fn plural_without_one_form_is_just_other() {
        set_current(Lang::En);
        assert_eq!(t_n("shell.menu.view_language", 1), "Language");
        assert_eq!(t_n("shell.menu.view_language", 7), "Language");
        set_current(Lang::Zh);
        // 中文恒用 zh：n 不改变取值。
        assert_eq!(t_n("shell.menu.view_language", 0), "语言");
    }

    #[test]
    fn tf_n_picks_plural_form_then_fills_placeholder() {
        // 状态栏「已选中 N 行」：登记了 en_one 的 key 才走 one 形态（#199 首批复数条目）。
        set_current(Lang::En);
        let one = tf_n("status_bar.selected_rows", 1, &[("n", "1".to_owned())]);
        assert!(one.starts_with("1 row selected"), "{one}");
        let many = tf_n("status_bar.selected_rows", 12, &[("n", "12".to_owned())]);
        assert!(many.starts_with("12 rows selected"), "{many}");
        // 中文恒单式：n 只填占位，不换形态。
        set_current(Lang::Zh);
        let zh = tf_n("status_bar.selected_rows", 12, &[("n", "12".to_owned())]);
        assert!(zh.starts_with("已选中 12 行"), "{zh}");
    }

    #[test]
    fn dialogs_entries_carry_the_s2_dialog_copy() {
        // S2/#200：对话框与连接区条目——zh 与迁移前字面量逐字节一致（比对脚本另证），
        // 这里钉代表性条目与占位/复数行为，防止 key 改名或 zh 漂移。
        set_current(Lang::Zh);
        assert_eq!(t("dialogs.title_new"), "新建连接");
        assert_eq!(t("dialogs.close_guard_heading"), "有未提交的更改");
        assert_eq!(t("tabs.close_btn_discard"), "放弃更改并关闭");
        assert_eq!(t("startup.desc_handshake"), "正在与数据库握手…");
        assert_eq!(t("dialogs.logs_title"), "Sqarvo 日志");
        assert_eq!(
            tf(
                "tabs.limit_body",
                &[("MAX_TABS", "8".into()), ("open", "8".into())]
            ),
            "标签上限 8 个（当前已开 8 个）：请先关闭一个标签再新建"
        );
        // 复数条目：zh 恒一条；en n==1 走 en_one。
        assert_eq!(
            tf_n("tabs.close_body_dirty", 1, &[("dirty", "1".into())]),
            "该标签有 1 行未提交的更改。关闭会丢失这些更改：可以选择先保存，或直接放弃。"
        );
        set_current(Lang::En);
        assert_eq!(t("dialogs.close_guard_heading"), "Uncommitted Changes");
        let one = tf_n("tabs.close_body_dirty", 1, &[("dirty", "1".into())]);
        assert!(one.contains("1 row of uncommitted changes"), "{one}");
        let many = tf_n("tabs.close_body_dirty", 3, &[("dirty", "3".into())]);
        assert!(many.contains("3 rows of uncommitted changes"), "{many}");
    }

    #[test]
    fn data_page_entries_carry_the_s4_grid_copy() {
        // S4/#202 Lane A：数据网格区条目——zh 与迁移前字面量逐字节一致（比对脚本另证），
        // 这里钉代表性条目与占位/复数行为，防止 key 改名或 zh 漂移。
        set_current(Lang::Zh);
        assert_eq!(t("data_page.pager_previous"), "上一页");
        assert_eq!(t("data_page.badge_no_sort_filter"), "驱动不支持排序/筛选");
        assert_eq!(
            t("data_page.row_insert_tooltip"),
            "新行 · 尚未提交（双击单元格填值）"
        );
        assert_eq!(
            t("data_page.badge_no_primary_key_tip"),
            "无主键表：不能稳定定位行，因此只读；浏览、排序、筛选与计数仍可用"
        );
        assert_eq!(
            tf("data_page.row_number_tooltip", &[("number", "42".into())]),
            "第 42 行"
        );
        assert_eq!(
            tf(
                "data_page.cc_check_tooltip",
                &[("name", "id".into()), ("type", "INTEGER".into())]
            ),
            "id（INTEGER）· 取消勾选只隐藏这一列，不影响数据与导出口径"
        );
        // 复数条目：zh 恒一条；en n==1 走 en_one。
        assert_eq!(
            tf_n(
                "data_page.page_rows",
                1,
                &[("rows", "1".into()), ("suffix", String::new())]
            ),
            "本页 1 行"
        );
        set_current(Lang::En);
        assert_eq!(t("data_page.cc_label"), "Column Visibility");
        assert_eq!(t("data_page.fs_apply"), "Apply");
        assert_eq!(t("data_page.header_row_number"), "Row");
        let one = tf_n(
            "data_page.page_rows",
            1,
            &[("rows", "1".into()), ("suffix", " · more".into())],
        );
        assert_eq!(one, "1 row on this page · more");
        let many = tf_n("data_page.disconnected_status", 7, &[("rows", "7".into())]);
        assert_eq!(many, "Disconnected · 7 rows loaded (read-only)");
        set_current(Lang::Zh);
    }

    #[test]
    fn data_page_entries_carry_the_s4_editor_submit_copy() {
        // S4/#202 Lane B：编辑器与提交面条目——zh 与迁移前字面量逐字节一致（比对脚本
        // 另证），这里钉代表性条目、占位与**脏行计数两态**（1 row / N rows），
        // 防止 key 改名或 zh 漂移。
        set_current(Lang::Zh);
        assert_eq!(
            tf(
                "data_page.editor_title",
                &[
                    ("column_name", "name".into()),
                    ("type_label", "TEXT".into())
                ]
            ),
            "编辑 name · TEXT"
        );
        assert_eq!(
            tf(
                "data_page.insert_started",
                &[(
                    "summary",
                    "新行：显式 1 列 · 省略 2 列（交给数据库默认值）".into()
                )]
            ),
            "新行：显式 1 列 · 省略 2 列（交给数据库默认值） · 双击单元格填值，填完点「提交更改」"
        );
        assert_eq!(
            t("data_page.val_bool"),
            "{text} 不是合法的布尔值（1/0、true/false、是/否）"
        );
        assert_eq!(
            t("data_page.dot_note_active"),
            "事务中：提交事务之前，改动只在当前会话可见"
        );
        // 脏行计数标签（状态圆点 tooltip 的两态，QA --qa-expect-status-dot 读的就是这句）。
        let zh = tf_n(
            "data_page.dirty_plain",
            1,
            &[
                ("dirty", "1".into()),
                ("pending", "1".into()),
                ("insert_note", String::new()),
            ],
        );
        assert_eq!(zh, "脏行 1 · 待提交 1 条");
        set_current(Lang::En);
        assert_eq!(t("data_page.btn_apply"), "Apply");
        assert_eq!(t("data_page.txn_commit_btn"), "Commit Transaction");
        assert_eq!(t("data_page.txn_claim_begin"), "Begin transaction");
        assert_eq!(
            t("data_page.iso_default"),
            "Default (decided by the database)"
        );
        let one = tf_n(
            "data_page.dirty_plain",
            1,
            &[
                ("dirty", "1".into()),
                ("pending", "1".into()),
                ("insert_note", String::new()),
            ],
        );
        assert_eq!(one, "1 dirty row · 1 to commit");
        let many = tf_n(
            "data_page.dirty_plain",
            2,
            &[
                ("dirty", "2".into()),
                ("pending", "3".into()),
                ("insert_note", " · 1 new row".into()),
            ],
        );
        assert_eq!(many, "2 dirty rows · 3 to commit · 1 new row");
        let deletes = tf_n(
            "data_page.dirty_with_deletes",
            1,
            &[
                ("dirty", "1".into()),
                ("pending", "1".into()),
                ("deletes", "1".into()),
                ("insert_note", String::new()),
            ],
        );
        assert_eq!(deletes, "1 dirty row · 1 to commit (incl. 1 deletion)");
        let progress = tf_n(
            "data_page.submit_progress",
            1,
            &[
                ("changes", "1".into()),
                ("batch", "query_12".into()),
                ("note", String::new()),
            ],
        );
        assert_eq!(
            progress,
            "Submitting 1 change (batch query_12): click Cancel Commit to abort at a row boundary"
        );
        // 关窗守卫逐表摘要（DirtyTabSummary::line 的模板与两态）。
        assert_eq!(
            tf(
                "data_page.dirty_summary_line",
                &[
                    ("table", "users".into()),
                    ("parts", "1 row uncommitted".into())
                ]
            ),
            "users: 1 row uncommitted"
        );
        assert_eq!(
            t_n("data_page.dirty_summary_updates", 1),
            "1 row uncommitted"
        );
        assert_eq!(
            tf_n("data_page.deleted_marked", 2, &[("deleted", "2".into())]),
            "2 rows marked for deletion (can be undone before the batch commit)"
        );
        set_current(Lang::Zh);
    }

    #[test]
    fn data_page_entries_carry_the_s4_ddl_metadata_copy() {
        // S4 Lane C：DDL 状态页 / MetadataError 副文本 / 类型不可编辑原因——zh 与
        // 迁移前字面量逐字节一致（比对脚本另证），这里钉代表性条目与占位行为，
        // 防止 key 改名或 zh 漂移。
        set_current(Lang::Zh);
        assert_eq!(t("data_page.ddl_loading_title"), "正在读取 DDL…");
        assert_eq!(t("data_page.ddl_empty_title"), "服务端返回了空 DDL");
        assert_eq!(
            t("data_page.ddl_no_session_desc"),
            "对象所属连接的会话已断开：在对象树里点该连接重新连接后再试。"
        );
        assert_eq!(
            tf(
                "data_page.ddl_synth_warning",
                &[("detail", "缺 CHECK 约束".into())]
            ),
            "⚠ 这份 DDL 由客户端拼装，不是数据库原文；信息有损失 · 缺 CHECK 约束"
        );
        // MetadataError 全族 20 条：headline/hint 成对钉完，防任一臂漏接。
        for (head, hint) in [
            (
                "data_page.err_not_found_head",
                "data_page.err_not_found_hint",
            ),
            (
                "data_page.err_permission_denied_head",
                "data_page.err_permission_denied_hint",
            ),
            (
                "data_page.err_unsupported_head",
                "data_page.err_unsupported_hint",
            ),
            (
                "data_page.err_kind_unsupported_head",
                "data_page.err_kind_unsupported_hint",
            ),
            (
                "data_page.err_filter_invalid_head",
                "data_page.err_filter_invalid_hint",
            ),
            (
                "data_page.err_snapshot_changed_head",
                "data_page.err_snapshot_changed_hint",
            ),
            (
                "data_page.err_page_token_invalid_head",
                "data_page.err_page_token_invalid_hint",
            ),
            (
                "data_page.err_session_lost_head",
                "data_page.err_session_lost_hint",
            ),
            (
                "data_page.err_unavailable_head",
                "data_page.err_unavailable_hint",
            ),
            ("data_page.err_other_head", "data_page.err_other_hint"),
        ] {
            assert_ne!(t(head), head, "{head} 应有 zh 条目");
            assert_ne!(t(hint), hint, "{hint} 应有 zh 条目");
        }
        assert_eq!(t("data_page.err_session_lost_head"), "数据库会话已失效");
        assert_eq!(
            t("data_page.err_unsupported_hint"),
            "这一层由驱动决定是否实现；不代表该库没有对象，可改用「平铺表列表」查看当前库。"
        );
        assert_eq!(
            tf(
                "data_page.type_not_editable",
                &[("column", "blob".into()), ("type", "binary".into())]
            ),
            "列 blob 的类型 binary 不支持行内编辑（二进制 / 数组 / 未映射类型）"
        );
        set_current(Lang::En);
        assert_eq!(
            t("data_page.ddl_empty_desc"),
            "This does not mean the object has no structure: the contract forbids returning fabricated or truncated DDL; try another object or check the server log."
        );
        assert_eq!(
            t("data_page.err_permission_denied_head"),
            "No Permission to Read Metadata"
        );
        assert_eq!(
            t("data_page.err_other_hint"),
            "See the error code and the backend message below."
        );
        set_current(Lang::Zh);
    }
}
