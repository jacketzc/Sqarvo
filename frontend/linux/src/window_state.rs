//! 窗口状态文件：`$XDG_CONFIG_HOME/sqarvo/Sqarvo/window.json`（#119 / #158）。
//!
//! 只存「窗口级偏好」，与连接配置（`connections.json`）分开：
//!
//! - 连接配置是用户数据（怎么连、连哪里），窗口状态是界面临时偏好；
//! - 把面板宽度写进 `connections.json` 会让「改了一次窗口」看起来像改了一条连接，
//!   而且同一份连接配置在两个不同屏幕大小的机器上会被来回覆盖。
//!
//! 存的是**窗口形状**：信息面板宽度、侧栏宽度、窗口尺寸 / 位置、最大化状态与档位。
//! v0.2.3.2 之前面板宽度按连接记忆，结果启动态（还没有连接）用 280、连上后读回某条
//! 连接的 240，外壳形状在点击连接的一瞬间跳 40px。#155 起改为**窗口级记忆**：启动态、
//! 连接态、切换连接都读同一个值，拖一次就全局生效；旧文件里的按连接宽度只用于一次性
//! 迁移（取最常用的值）。#158 升到 v2，口径见 `scaling-and-window-sizing.md` §7。
//!
//! 文件里不含密码，只有宽度与坐标。读失败、写失败都不影响连接：状态文件是便利功能，
//! 读不到就用规范默认值。写入采用「临时文件 + rename」，进程崩溃不会留下半个 JSON。
//!
//! 宽度类字段一律存 **compact 档的归一化值**（实际 ÷ 档位 scale，四舍五入），读的时候
//! 乘回当前档位：不归一化会在切档时来回钳制。本轮只有 1.0 档，除法仍然是真写出来的。

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 状态文件名（与 `connections.json` 同目录）。
pub const FILE_NAME: &str = "window.json";

/// 当前文档版本（v2：窗口几何 + 侧栏宽度 + 档位，§7）。
const DOCUMENT_VERSION: u32 = 2;

/// 本轮档位名（#160 接三档后由偏好决定；写进文件是为了让读回时能换算宽度）。
const CURRENT_TIER: &str = "compact";

/// 档位名 → 宽度倍率（ADR 0026：compact 100% / comfortable 125% / large 150%）。
fn tier_scale(tier: Option<&str>) -> f64 {
    match tier {
        Some("comfortable") => 1.25,
        Some("large") => 1.5,
        _ => 1.0,
    }
}

/// 实际像素 → 归一化值（÷ 档位）。
fn normalized(width: i32, scale: f64) -> i32 {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    (f64::from(width) / scale).round() as i32
}

/// 归一化值 → 当前档位的实际像素（× 档位）。
fn denormalized(width: i32, scale: f64) -> i32 {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    (f64::from(width) * scale).round() as i32
}

/// 状态文件结构（v2）。
#[derive(Debug, Default, Serialize, Deserialize)]
struct Document {
    #[serde(default)]
    version: u32,
    /// 写入时的密度档名（v2）：宽度字段是这一档的归一化值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tier: Option<String>,
    /// 窗口级信息面板宽度（归一化值）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_inspector_width: Option<i32>,
    /// 窗口级侧栏宽度（归一化值，#158）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_sidebar_width: Option<i32>,
    /// 窗口尺寸 / 位置（归一化宽度、逻辑像素；位置在 GTK4 上只读不写）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    window_width: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    window_height: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    window_x: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    window_y: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    maximized: Option<bool>,
    /// 旧版（按连接记忆）的宽度：只读，用于迁移；下次写入时清空。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    inspector_width: BTreeMap<String, i32>,
}

impl Document {
    /// 生效的面板宽度：优先窗口级值；没有则从旧版按连接记录迁移。
    fn effective_inspector_width(&self, scale: f64) -> Option<i32> {
        if let Some(width) = self.last_inspector_width {
            return Some(crate::layout::clamp_inspector(denormalized(width, scale)));
        }
        // 旧版每条连接一个宽度。取出现次数最多的值（并列时取较小的宽度），
        // 升级后的第一次启动尽量接近用户最常用的宽度。
        let mut counts: BTreeMap<i32, usize> = BTreeMap::new();
        for width in self.inspector_width.values() {
            *counts
                .entry(crate::layout::clamp_inspector(*width))
                .or_default() += 1;
        }
        counts
            .into_iter()
            .max_by_key(|(width, count)| (*count, std::cmp::Reverse(*width)))
            .map(|(width, _)| width)
    }

    /// 生效的侧栏宽度（窗口级，已钳制）。
    fn effective_sidebar_width(&self, scale: f64) -> Option<i32> {
        self.last_sidebar_width
            .map(|width| crate::layout::clamp_sidebar(denormalized(width, scale)))
    }
}

/// 记忆的窗口几何（§7 的 `window_*` / `maximized`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowGeometry {
    pub width: i32,
    pub height: i32,
    /// GTK4 读不到 toplevel 位置，本平台写回时总是 `None`；读到旧文件时仍参与越界校验。
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub maximized: bool,
}

impl WindowGeometry {
    /// 保存的矩形是否仍落在某块显示器几何内（允许 ±[`window_size::GEOMETRY_TOLERANCE`]）。
    ///
    /// 显示器被拔掉是最常见的场景：越界就丢弃几何、回退 §5.2 的推荐尺寸（§7 规则 3）。
    /// 没有位置记录时按「贴着该显示器左上角」校验，只检查尺寸放不放得下。
    #[must_use]
    pub fn fits_in(&self, monitors: &[crate::window_size::Rect]) -> bool {
        let tolerance = crate::window_size::GEOMETRY_TOLERANCE;
        monitors.iter().any(|monitor| {
            let (x, y) = match (self.x, self.y) {
                (Some(x), Some(y)) => (x, y),
                _ => (monitor.x, monitor.y),
            };
            x >= monitor.x - tolerance
                && y >= monitor.y - tolerance
                && x + self.width <= monitor.x + monitor.width + tolerance
                && y + self.height <= monitor.y + monitor.height + tolerance
        })
    }

    /// 日志里的位置部分（没有记录时写明原因，不写假的 `+0+0`）。
    #[must_use]
    pub fn origin_label(&self) -> String {
        match (self.x, self.y) {
            (Some(x), Some(y)) => format!("+{x}+{y}"),
            _ => "（位置未记录）".to_owned(),
        }
    }
}

/// 窗口上次记住的信息面板宽度（已换算到当前档位并钳制；没有记录时为 `None`）。
#[must_use]
pub fn last_inspector_width() -> Option<i32> {
    let dir = crate::config::config_dir().ok()?;
    let document = load_from(&dir.join(FILE_NAME));
    document.effective_inspector_width(tier_scale(document.tier.as_deref()))
}

/// 窗口上次记住的侧栏宽度（#158；没有记录时为 `None`）。
#[must_use]
pub fn last_sidebar_width() -> Option<i32> {
    let dir = crate::config::config_dir().ok()?;
    let document = load_from(&dir.join(FILE_NAME));
    document.effective_sidebar_width(tier_scale(document.tier.as_deref()))
}

/// 上次记住的窗口几何；没有记录（或文件里没有尺寸）时为 `None`。
#[must_use]
pub fn saved_geometry() -> Option<WindowGeometry> {
    let dir = crate::config::config_dir().ok()?;
    saved_geometry_from(&dir.join(FILE_NAME))
}

/// [`saved_geometry`] 的路径参数版本（测试用）。
fn saved_geometry_from(path: &Path) -> Option<WindowGeometry> {
    let document = load_from(path);
    let scale = tier_scale(document.tier.as_deref());
    let width = document
        .window_width
        .map(|value| denormalized(value, scale))?;
    let height = document
        .window_height
        .map(|value| denormalized(value, scale))?;
    Some(WindowGeometry {
        width,
        height,
        x: document.window_x,
        y: document.window_y,
        maximized: document.maximized.unwrap_or(false),
    })
}

/// 记住窗口的信息面板宽度，返回写入路径。
///
/// 宽度先钳制到可拖拽区间：状态文件被人手改坏（例如 0 或 9999）时，
/// 下一帧也不会把工作区挤成 0 宽。旧版按连接的记录在第一次写入时清空。
pub fn remember_inspector_width(width: i32) -> Result<PathBuf, String> {
    let dir = crate::config::config_dir().map_err(|error| error.to_string())?;
    remember_inspector_in(&dir, width)
}

/// [`remember_inspector_width`] 的目录参数版本（测试用，不影响生产路径）。
fn remember_inspector_in(dir: &Path, width: i32) -> Result<PathBuf, String> {
    let width = crate::layout::clamp_inspector(width);
    let path = dir.join(FILE_NAME);
    let mut document = load_from(&path);
    stamp(&mut document);
    document.last_inspector_width = Some(normalized(width, tier_scale(document.tier.as_deref())));
    document.inspector_width.clear();
    write_atomic(dir, &path, &document)?;
    eprintln!(
        "[shell] 信息面板宽度 {width} 已记入 {}（窗口级）",
        path.display()
    );
    Ok(path)
}

/// 记住窗口的侧栏宽度（与信息面板同一套机制、同一个文件），返回写入路径。
pub fn remember_sidebar_width(width: i32) -> Result<PathBuf, String> {
    let dir = crate::config::config_dir().map_err(|error| error.to_string())?;
    remember_sidebar_in(&dir, width)
}

fn remember_sidebar_in(dir: &Path, width: i32) -> Result<PathBuf, String> {
    let width = crate::layout::clamp_sidebar(width);
    let path = dir.join(FILE_NAME);
    let mut document = load_from(&path);
    stamp(&mut document);
    document.last_sidebar_width = Some(normalized(width, tier_scale(document.tier.as_deref())));
    write_atomic(dir, &path, &document)?;
    eprintln!(
        "[shell] 侧栏宽度 {width} 已记入 {}（窗口级）",
        path.display()
    );
    Ok(path)
}

/// 记住窗口几何（尺寸 / 最大化），返回写入路径。
pub fn remember_geometry(geometry: &WindowGeometry) -> Result<PathBuf, String> {
    let dir = crate::config::config_dir().map_err(|error| error.to_string())?;
    remember_geometry_in(&dir, geometry)
}

fn remember_geometry_in(dir: &Path, geometry: &WindowGeometry) -> Result<PathBuf, String> {
    let path = dir.join(FILE_NAME);
    let mut document = load_from(&path);
    stamp(&mut document);
    let scale = tier_scale(document.tier.as_deref());
    document.window_width = Some(normalized(geometry.width, scale));
    document.window_height = Some(normalized(geometry.height, scale));
    // 位置没有可靠来源就不写：宁可缺字段，也不要存一个假的 0,0 让下次启动误判。
    document.window_x = geometry.x;
    document.window_y = geometry.y;
    document.maximized = Some(geometry.maximized);
    write_atomic(dir, &path, &document)?;
    Ok(path)
}

/// 写回时统一升级到当前版本并带上档位名（v1 → v2 的迁移点）。
fn stamp(document: &mut Document) {
    document.version = DOCUMENT_VERSION;
    document.tier = Some(CURRENT_TIER.to_owned());
}

fn load_from(path: &Path) -> Document {
    let Ok(text) = fs::read_to_string(path) else {
        return Document::default();
    };
    match serde_json::from_str::<Document>(&text) {
        Ok(document) => document,
        Err(error) => {
            eprintln!(
                "[shell] 窗口状态无法解析（{}）：{error}；本次按默认值处理，不会覆盖已有文件",
                path.display()
            );
            Document::default()
        }
    }
}

fn write_atomic(dir: &Path, path: &Path, document: &Document) -> Result<(), String> {
    fs::create_dir_all(dir)
        .map_err(|error| format!("无法创建配置目录 {}：{error}", dir.display()))?;
    restrict(dir, 0o700)?;
    let text = serde_json::to_string_pretty(document)
        .map_err(|error| format!("窗口状态序列化失败：{error}"))?;
    let temporary = dir.join(format!(".{FILE_NAME}.tmp"));
    {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|error| format!("无法写入 {}：{error}", temporary.display()))?;
        file.write_all(text.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("写入 {} 失败：{error}", temporary.display()))?;
    }
    restrict(&temporary, 0o600)?;
    fs::rename(&temporary, path).map_err(|error| {
        format!(
            "无法把 {} 重命名为 {}：{error}",
            temporary.display(),
            path.display()
        )
    })?;
    restrict(path, 0o600)?;
    Ok(())
}

#[cfg(unix)]
fn restrict(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|error| format!("无法把 {} 的权限设为 {mode:o}：{error}", path.display()))
}

#[cfg(not(unix))]
fn restrict(_path: &Path, _mode: u32) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(dir: &Path, document: &Document) -> PathBuf {
        let path = dir.join(FILE_NAME);
        write_atomic(dir, &path, document).expect("写入状态文件");
        path
    }

    fn store_global(dir: &Path, width: i32) -> Option<i32> {
        let document = Document {
            version: DOCUMENT_VERSION,
            last_inspector_width: Some(width),
            ..Document::default()
        };
        let path = store(dir, &document);
        load_from(&path).effective_inspector_width(1.0)
    }

    fn monitor(width: i32, height: i32) -> crate::window_size::Rect {
        crate::window_size::Rect {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    #[test]
    fn width_is_window_level_not_per_connection() {
        let dir = tempfile::tempdir().expect("临时目录");
        assert_eq!(store_global(dir.path(), 320), Some(320));
        // 再写一次覆盖同一个值：不存在「另一条连接保留自己的宽度」。
        assert_eq!(store_global(dir.path(), 240), Some(240));
        let document = load_from(&dir.path().join(FILE_NAME));
        assert_eq!(document.last_inspector_width, Some(240));
        assert!(document.inspector_width.is_empty());
    }

    #[test]
    fn legacy_per_connection_width_migrates_to_most_common() {
        let dir = tempfile::tempdir().expect("临时目录");
        let document = Document {
            version: 1,
            last_inspector_width: None,
            inspector_width: BTreeMap::from([
                ("mariadb|a".to_owned(), 360),
                ("sqlite|file|/a.db".to_owned(), 240),
                ("sqlite|file|/b.db".to_owned(), 240),
            ]),
            ..Document::default()
        };
        let path = store(dir.path(), &document);
        assert_eq!(load_from(&path).effective_inspector_width(1.0), Some(240));
    }

    #[test]
    fn remembering_a_width_is_window_level_and_clears_legacy() {
        let dir = tempfile::tempdir().expect("临时目录");
        let legacy = Document {
            version: 1,
            last_inspector_width: None,
            inspector_width: BTreeMap::from([("k".to_owned(), 360)]),
            ..Document::default()
        };
        store(dir.path(), &legacy);
        let path = remember_inspector_in(dir.path(), 300).expect("写入宽度");
        let document = load_from(&path);
        assert_eq!(document.last_inspector_width, Some(300));
        assert!(document.inspector_width.is_empty());
        assert_eq!(document.effective_inspector_width(1.0), Some(300));

        // 写入前也钳制：0 / 9999 不会把工作区挤成 0 宽或占满整窗。
        remember_inspector_in(dir.path(), 0).expect("写入下限");
        assert_eq!(
            load_from(&path).effective_inspector_width(1.0),
            Some(crate::layout::INSPECTOR_MIN)
        );
        remember_inspector_in(dir.path(), 9_000).expect("写入上限");
        assert_eq!(
            load_from(&path).effective_inspector_width(1.0),
            Some(crate::layout::INSPECTOR_MAX)
        );
    }

    #[test]
    fn sidebar_width_is_memorised_in_the_same_file() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = remember_sidebar_in(dir.path(), 300).expect("写入侧栏宽度");
        // 与面板宽度同一个文件、同一份文档：写侧栏不会把面板值冲掉。
        remember_inspector_in(dir.path(), 320).expect("写入面板宽度");
        let document = load_from(&path);
        assert_eq!(document.last_sidebar_width, Some(300));
        assert_eq!(document.effective_sidebar_width(1.0), Some(300));
        assert_eq!(document.effective_inspector_width(1.0), Some(320));
        // 越界钳到 180–420。
        remember_sidebar_in(dir.path(), 20).expect("写入下限");
        assert_eq!(
            load_from(&path).effective_sidebar_width(1.0),
            Some(crate::layout::SIDEBAR_MIN)
        );
    }

    #[test]
    fn geometry_round_trips_and_upgrades_a_v1_file() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join(FILE_NAME);
        // v1 文件：只有面板宽度与旧版按连接记录。
        fs::write(
            &path,
            r#"{"version":1,"last_inspector_width":300,"inspector_width":{"a":260}}"#,
        )
        .expect("写 v1");
        assert_eq!(saved_geometry_from(&path), None);
        remember_geometry_in(
            dir.path(),
            &WindowGeometry {
                width: 1400,
                height: 838,
                x: None,
                y: None,
                maximized: false,
            },
        )
        .expect("写入几何");
        let text = fs::read_to_string(&path).expect("读文件");
        assert!(text.contains("\"version\": 2"), "应升到 v2：{text}");
        assert!(text.contains("\"tier\": \"compact\""), "应带档位：{text}");
        assert!(text.contains("\"window_width\": 1400"), "应存尺寸：{text}");
        // 升级写入不清掉同文件里的面板宽度。
        assert_eq!(load_from(&path).effective_inspector_width(1.0), Some(300));
        assert_eq!(
            saved_geometry_from(&path),
            Some(WindowGeometry {
                width: 1400,
                height: 838,
                x: None,
                y: None,
                maximized: false,
            })
        );
    }

    #[test]
    fn width_fields_are_normalised_to_the_compact_tier() {
        // 除法真的写在代码里：1.25 档存 1120、读回来是 1400。
        assert_eq!(normalized(1400, 1.25), 1120);
        assert_eq!(denormalized(1120, 1.25), 1400);
        assert_eq!(normalized(1400, 1.0), 1400);
        // 非法倍率按 1.0 处理，不做除零。
        assert_eq!(normalized(1400, 0.0), 1400);
        assert_eq!(denormalized(1400, f64::NAN), 1400);
    }

    #[test]
    fn geometry_outside_every_monitor_is_rejected() {
        let monitors = [monitor(1920, 1080)];
        let inside = WindowGeometry {
            width: 1400,
            height: 838,
            x: Some(260),
            y: Some(120),
            maximized: false,
        };
        assert!(inside.fits_in(&monitors));
        // 手改 window_x = 99999：不在任何显示器内。
        let far = WindowGeometry {
            x: Some(99_999),
            ..inside
        };
        assert!(!far.fits_in(&monitors));
        // ±32 容差内的负坐标仍然接受（窗口管理器装饰 / 边缘吸附）。
        assert!(
            WindowGeometry {
                x: Some(-20),
                ..inside
            }
            .fits_in(&monitors)
        );
        assert!(
            !WindowGeometry {
                x: Some(-100),
                ..inside
            }
            .fits_in(&monitors)
        );
        // 没有位置记录时只校验尺寸能否放下。
        assert!(
            WindowGeometry {
                x: None,
                y: None,
                ..inside
            }
            .fits_in(&monitors)
        );
        assert!(
            !WindowGeometry {
                width: 3000,
                x: None,
                y: None,
                ..inside
            }
            .fits_in(&monitors)
        );
        // 第二块显示器上的矩形要能被认出来。
        let dual = [
            monitor(1920, 1080),
            crate::window_size::Rect {
                x: 1920,
                y: 0,
                width: 1920,
                height: 1080,
            },
        ];
        assert!(
            WindowGeometry {
                x: Some(2200),
                y: Some(100),
                ..inside
            }
            .fits_in(&dual)
        );
    }

    #[test]
    fn out_of_range_widths_are_clamped_before_reading_and_storing() {
        let dir = tempfile::tempdir().expect("临时目录");
        assert_eq!(
            store_global(dir.path(), 0),
            Some(crate::layout::INSPECTOR_MIN)
        );
        assert_eq!(
            store_global(dir.path(), 9_000),
            Some(crate::layout::INSPECTOR_MAX)
        );

        let legacy = Document {
            version: 1,
            last_inspector_width: None,
            inspector_width: BTreeMap::from([("k".to_owned(), 9_000)]),
            ..Document::default()
        };
        let path = store(dir.path(), &legacy);
        assert_eq!(
            load_from(&path).effective_inspector_width(1.0),
            Some(crate::layout::INSPECTOR_MAX)
        );
    }

    #[test]
    fn missing_or_broken_file_reads_as_defaults() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join(FILE_NAME);
        assert_eq!(load_from(&path).effective_inspector_width(1.0), None);
        fs::write(&path, "{ not json").expect("写入坏文件");
        assert_eq!(load_from(&path).effective_inspector_width(1.0), None);
        assert_eq!(saved_geometry_from(&path), None);
        // 坏文件不会被读成空后导致下一次写入失败：整份替换仍然成功。
        assert_eq!(store_global(dir.path(), 300), Some(300));
    }

    #[test]
    fn stored_file_has_no_sensitive_keys() {
        let dir = tempfile::tempdir().expect("临时目录");
        store_global(dir.path(), 300);
        let text = fs::read_to_string(dir.path().join(FILE_NAME)).expect("读文件");
        for forbidden in ["password", "secret", "token"] {
            assert!(
                !text.contains(forbidden),
                "状态文件里不应出现 {forbidden}：{text}"
            );
        }
    }

    #[test]
    fn file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("临时目录");
        store_global(dir.path(), 300);
        let mode = fs::metadata(dir.path().join(FILE_NAME))
            .expect("stat")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}
