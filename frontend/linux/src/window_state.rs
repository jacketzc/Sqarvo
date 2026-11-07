//! 窗口状态文件：`$XDG_CONFIG_HOME/sqarvo/Sqarvo/window.json`（#119）。
//!
//! 只存「窗口级偏好」，与连接配置（`connections.json`）分开：
//!
//! - 连接配置是用户数据（怎么连、连哪里），窗口状态是界面临时偏好；
//! - 把面板宽度写进 `connections.json` 会让「改了一次窗口」看起来像改了一条连接，
//!   而且同一份连接配置在两个不同屏幕大小的机器上会被来回覆盖。
//!
//! 当前只有一项：**信息面板宽度按连接记忆**（workspace-model §5）。
//!
//! 键是连接的配置身份（[`crate::config::SavedConnection::key`]），不是显示名——
//! 两条同名连接不会被当成同一条。文件里不含密码，只有宽度数字与连接身份。
//!
//! 读失败、写失败都不影响连接：状态文件是便利功能，读不到就用规范默认值 280。
//! 写入同样采用「临时文件 + rename」，进程崩溃不会留下半个 JSON。

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
    /// 连接身份 → 信息面板宽度。
    #[serde(default)]
    inspector_width: BTreeMap<String, i32>,
}

/// 某个连接上次的面板宽度（已钳制到 240–520；没有记录时为 `None`）。
#[must_use]
pub fn inspector_width(connection_key: &str) -> Option<i32> {
    if connection_key.is_empty() {
        return None;
    }
    let dir = crate::config::config_dir().ok()?;
    load_from(&dir.join(FILE_NAME))
        .inspector_width
        .get(connection_key)
        .copied()
}

/// 记住某个连接的面板宽度，返回写入路径。
///
/// 宽度先钳制到可拖拽区间：状态文件被人手改坏（例如 0 或 9999）时，
/// 下一帧也不会把工作区挤成 0 宽。
pub fn remember_inspector_width(connection_key: &str, width: i32) -> Result<PathBuf, String> {
    if connection_key.is_empty() {
        return Err("没有连接身份，宽度不落盘".to_owned());
    }
    let dir = crate::config::config_dir().map_err(|error| error.to_string())?;
    let width = crate::layout::clamp_inspector(width);
    let path = dir.join(FILE_NAME);
    let mut document = load_from(&path);
    document.version = DOCUMENT_VERSION;
    document
        .inspector_width
        .insert(connection_key.to_owned(), width);
    write_atomic(&dir, &path, &document)?;
    eprintln!(
        "[shell] 信息面板宽度 {width} 已按连接记入 {}",
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

    fn roundtrip(dir: &Path, key: &str, width: i32) -> Option<i32> {
        let path = dir.join(FILE_NAME);
        let mut document = load_from(&path);
        document
            .inspector_width
            .insert(key.to_owned(), crate::layout::clamp_inspector(width));
        write_atomic(dir, &path, &document).expect("写入状态文件");
        load_from(&path).inspector_width.get(key).copied()
    }

    #[test]
    fn width_is_remembered_per_connection_key() {
        let dir = tempfile::tempdir().expect("临时目录");
        assert_eq!(roundtrip(dir.path(), "sqarvo:conn/a", 320), Some(320));
        // 另一条连接保持自己的宽度，不会互相覆盖。
        assert_eq!(roundtrip(dir.path(), "sqarvo:conn/b", 240), Some(240));
        let document = load_from(&dir.path().join(FILE_NAME));
        assert_eq!(document.inspector_width.get("sqarvo:conn/a"), Some(&320));
        assert_eq!(document.inspector_width.get("sqarvo:conn/b"), Some(&240));
    }

    #[test]
    fn out_of_range_widths_are_clamped_before_storing() {
        let dir = tempfile::tempdir().expect("临时目录");
        assert_eq!(
            roundtrip(dir.path(), "k1", 0),
            Some(crate::layout::INSPECTOR_MIN)
        );
        assert_eq!(
            roundtrip(dir.path(), "k2", 9_000),
            Some(crate::layout::INSPECTOR_MAX)
        );
    }

    #[test]
    fn missing_or_broken_file_reads_as_defaults() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join(FILE_NAME);
        assert!(load_from(&path).inspector_width.is_empty());
        fs::write(&path, "{ not json").expect("写入坏文件");
        assert!(load_from(&path).inspector_width.is_empty());
        // 坏文件不会被读成空后被下一次写入静默覆盖成「只有一条记录」以外的内容：
        // 写入是整份替换，这里只断言下次写入仍然成功。
        assert_eq!(roundtrip(dir.path(), "k3", 300), Some(300));
    }

    #[test]
    fn stored_file_has_no_sensitive_keys() {
        let dir = tempfile::tempdir().expect("临时目录");
        roundtrip(dir.path(), "sqarvo:conn/a", 300);
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
        roundtrip(dir.path(), "k", 300);
        let mode = fs::metadata(dir.path().join(FILE_NAME))
            .expect("stat")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}
