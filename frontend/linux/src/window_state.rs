//! 窗口状态文件：`$XDG_CONFIG_HOME/sqarvo/Sqarvo/window.json`（#119）。
//!
//! 只存「窗口级偏好」，与连接配置（`connections.json`）分开：
//!
//! - 连接配置是用户数据（怎么连、连哪里），窗口状态是界面临时偏好；
//! - 把面板宽度写进 `connections.json` 会让「改了一次窗口」看起来像改了一条连接，
//!   而且同一份连接配置在两个不同屏幕大小的机器上会被来回覆盖。
//!
//! 当前只有一项：**信息面板宽度**。v0.2.3.2 之前按连接记忆，结果启动态（还没有
//! 连接）用 280、连上后读回某条连接的 240，外壳形状在点击连接的一瞬间跳 40px。
//! #155 起改为**窗口级记忆**：启动态、连接态、切换连接都读同一个值，拖一次就全局
//! 生效；旧文件里的按连接宽度只用于一次性迁移（取最常用的值）。
//!
//! 文件里不含密码，只有宽度数字。读失败、写失败都不影响连接：状态文件是便利功能，
//! 读不到就用规范默认值 280。写入采用「临时文件 + rename」，进程崩溃不会留下半个 JSON。

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 状态文件名（与 `connections.json` 同目录）。
pub const FILE_NAME: &str = "window.json";

/// 当前文档版本。
const DOCUMENT_VERSION: u32 = 1;

/// 状态文件结构。
#[derive(Debug, Default, Serialize, Deserialize)]
struct Document {
    #[serde(default)]
    version: u32,
    /// 窗口级信息面板宽度（v0.2.3.2 / #155 起）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_inspector_width: Option<i32>,
    /// 旧版（按连接记忆）的宽度：只读，用于迁移；下次写入时清空。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    inspector_width: BTreeMap<String, i32>,
}

impl Document {
    /// 生效的面板宽度：优先窗口级值；没有则从旧版按连接记录迁移。
    fn effective_inspector_width(&self) -> Option<i32> {
        if let Some(width) = self.last_inspector_width {
            return Some(crate::layout::clamp_inspector(width));
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
}

/// 窗口上次记住的信息面板宽度（已钳制到 240–520；没有记录时为 `None`）。
#[must_use]
pub fn last_inspector_width() -> Option<i32> {
    let dir = crate::config::config_dir().ok()?;
    load_from(&dir.join(FILE_NAME)).effective_inspector_width()
}

/// 记住窗口的信息面板宽度，返回写入路径。
///
/// 宽度先钳制到可拖拽区间：状态文件被人手改坏（例如 0 或 9999）时，
/// 下一帧也不会把工作区挤成 0 宽。旧版按连接的记录在第一次写入时清空。
pub fn remember_inspector_width(width: i32) -> Result<PathBuf, String> {
    let dir = crate::config::config_dir().map_err(|error| error.to_string())?;
    remember_in_dir(&dir, width)
}

/// [`remember_inspector_width`] 的目录参数版本（测试用，不影响生产路径）。
fn remember_in_dir(dir: &Path, width: i32) -> Result<PathBuf, String> {
    let width = crate::layout::clamp_inspector(width);
    let path = dir.join(FILE_NAME);
    let mut document = load_from(&path);
    document.version = DOCUMENT_VERSION;
    document.last_inspector_width = Some(width);
    document.inspector_width.clear();
    write_atomic(dir, &path, &document)?;
    eprintln!(
        "[shell] 信息面板宽度 {width} 已记入 {}（窗口级）",
        path.display()
    );
    Ok(path)
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
            inspector_width: BTreeMap::new(),
        };
        let path = store(dir, &document);
        load_from(&path).effective_inspector_width()
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
            version: DOCUMENT_VERSION,
            last_inspector_width: None,
            inspector_width: BTreeMap::from([
                ("mariadb|a".to_owned(), 360),
                ("sqlite|file|/a.db".to_owned(), 240),
                ("sqlite|file|/b.db".to_owned(), 240),
            ]),
        };
        let path = store(dir.path(), &document);
        assert_eq!(load_from(&path).effective_inspector_width(), Some(240));
    }

    #[test]
    fn remembering_a_width_is_window_level_and_clears_legacy() {
        let dir = tempfile::tempdir().expect("临时目录");
        let legacy = Document {
            version: DOCUMENT_VERSION,
            last_inspector_width: None,
            inspector_width: BTreeMap::from([("k".to_owned(), 360)]),
        };
        store(dir.path(), &legacy);
        let path = remember_in_dir(dir.path(), 300).expect("写入宽度");
        let document = load_from(&path);
        assert_eq!(document.last_inspector_width, Some(300));
        assert!(document.inspector_width.is_empty());
        assert_eq!(document.effective_inspector_width(), Some(300));

        // 写入前也钳制：0 / 9999 不会把工作区挤成 0 宽或占满整窗。
        remember_in_dir(dir.path(), 0).expect("写入下限");
        assert_eq!(
            load_from(&path).effective_inspector_width(),
            Some(crate::layout::INSPECTOR_MIN)
        );
        remember_in_dir(dir.path(), 9_000).expect("写入上限");
        assert_eq!(
            load_from(&path).effective_inspector_width(),
            Some(crate::layout::INSPECTOR_MAX)
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
            version: DOCUMENT_VERSION,
            last_inspector_width: None,
            inspector_width: BTreeMap::from([("k".to_owned(), 9_000)]),
        };
        let path = store(dir.path(), &legacy);
        assert_eq!(
            load_from(&path).effective_inspector_width(),
            Some(crate::layout::INSPECTOR_MAX)
        );
    }

    #[test]
    fn missing_or_broken_file_reads_as_defaults() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join(FILE_NAME);
        assert_eq!(load_from(&path).effective_inspector_width(), None);
        fs::write(&path, "{ not json").expect("写入坏文件");
        assert_eq!(load_from(&path).effective_inspector_width(), None);
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
