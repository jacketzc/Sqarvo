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
// 与 `theme::SCHEME` 同理放 `thread_local`：GTK 对象只在主线程用，菜单文案也只在
// 主线程取。未显式 `set_current` 时生效值按 `Zh`——zh 是 source of truth，单测里
// 直接构造菜单也拿到与迁移前逐字节相同的中文；生产路径总在启动时 resolve，
// 不受这个默认影响。
thread_local! {
    static PREFERENCE: Cell<Lang> = const { Cell::new(Lang::System) };
    static EFFECTIVE: Cell<Lang> = const { Cell::new(Lang::Zh) };
}

/// 当前语言偏好（菜单勾选态读这个，三档原样）。
#[must_use]
pub fn preference() -> Lang {
    PREFERENCE.with(Cell::get)
}

/// 当前生效语言（只会是 `Zh` / `En`，文案查找读这个）。
#[must_use]
pub fn current() -> Lang {
    EFFECTIVE.with(Cell::get)
}

/// 设语言偏好并折叠生效值。必须在构造任何缓存文案的 UI（菜单等）**之前**调用
/// （启动路径），菜单点选时再调一次。
pub fn set_current(preference: Lang) {
    PREFERENCE.with(|slot| slot.set(preference));
    let effective = preference.effective();
    EFFECTIVE.with(|slot| slot.set(effective));
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
}
