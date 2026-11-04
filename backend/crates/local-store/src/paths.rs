//! 应用本地目录布局。

use std::fs;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use sqarvo_common::{Error, Result};

/// 应用本地目录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    config: PathBuf,
    data: PathBuf,
    runtime: PathBuf,
}

impl AppPaths {
    /// 依据操作系统约定解析目录。
    ///
    /// - 配置：`~/.config/sqarvo`（Linux）、`~/Library/Application Support/io.sqarvo.Sqarvo`（macOS）、
    ///   `%APPDATA%\sqarvo\Sqarvo`（Windows）；
    /// - 运行时目录优先使用 `XDG_RUNTIME_DIR`，不可用时退化为数据目录下的 `run`。
    pub fn discover() -> Result<Self> {
        let dirs = ProjectDirs::from("io", "sqarvo", "Sqarvo")
            .ok_or_else(|| Error::internal("无法确定用户目录：缺少 HOME / APPDATA 等环境变量"))?;
        let data = dirs.data_dir().to_path_buf();
        let runtime = dirs
            .runtime_dir()
            .map_or_else(|| data.join("run"), Path::to_path_buf);
        Ok(Self {
            config: dirs.config_dir().to_path_buf(),
            data,
            runtime,
        })
    }

    /// 由显式根目录构造，便于测试与打包场景覆盖默认位置。
    #[must_use]
    pub fn from_roots(
        config: impl Into<PathBuf>,
        data: impl Into<PathBuf>,
        runtime: impl Into<PathBuf>,
    ) -> Self {
        Self {
            config: config.into(),
            data: data.into(),
            runtime: runtime.into(),
        }
    }

    /// 创建目录，并在 Unix 上把权限收紧到 0700。
    pub fn ensure_dirs(&self) -> Result<()> {
        for dir in [&self.config, &self.data, &self.runtime] {
            fs::create_dir_all(dir).map_err(Error::from)?;
            restrict_to_owner(dir)?;
        }
        Ok(())
    }

    /// 配置目录。
    #[must_use]
    pub fn config_dir(&self) -> &Path {
        &self.config
    }

    /// 数据目录。
    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.data
    }

    /// 运行时目录（socket、端点描述文件）。
    #[must_use]
    pub fn runtime_dir(&self) -> &Path {
        &self.runtime
    }

    /// Unix 域套接字路径。
    #[must_use]
    pub fn socket_path(&self) -> PathBuf {
        self.runtime.join("backend.sock")
    }

    /// 端点描述文件路径。
    #[must_use]
    pub fn endpoint_file(&self) -> PathBuf {
        self.runtime.join("backend-endpoint.json")
    }
}

/// 把目录权限收紧到当前用户（Unix 0700）。
///
/// Windows 下由用户配置目录的 ACL 继承提供隔离；显式 DACL 收紧尚未实现。
pub fn restrict_to_owner(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = fs::Permissions::from_mode(0o700);
        fs::set_permissions(path, permissions).map_err(Error::from)?;
    }
    #[cfg(not(unix))]
    {
        // Windows 下由用户目录 ACL 继承提供隔离；显式 DACL 收紧尚未实现。
        let _ = path;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_derived_from_runtime_root() {
        let paths = AppPaths::from_roots("/tmp/cfg", "/tmp/data", "/tmp/run");
        assert_eq!(paths.socket_path(), PathBuf::from("/tmp/run/backend.sock"));
        assert_eq!(
            paths.endpoint_file(),
            PathBuf::from("/tmp/run/backend-endpoint.json")
        );
    }

    #[test]
    fn ensure_dirs_creates_private_directories() {
        let root = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_roots(
            root.path().join("config"),
            root.path().join("data"),
            root.path().join("run"),
        );
        paths.ensure_dirs().unwrap();
        assert!(paths.config_dir().is_dir());
        assert!(paths.runtime_dir().is_dir());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(paths.runtime_dir())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700);
        }
    }
}
