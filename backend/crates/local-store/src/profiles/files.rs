//! 共享档案文件的锁定与原子持久化。

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use sqarvo_common::{Error, ErrorCategory, ErrorCode, Result};
use uuid::Uuid;

use crate::endpoint::restrict_file_to_owner;
use crate::file_lock::{LockMode, try_lock};
use crate::restrict_to_owner;

const MAX_DOCUMENT_BYTES: usize = 16 * 1024 * 1024;
const LOCK_TIMEOUT: Duration = Duration::from_secs(2);
const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(10);
const TEMPORARY_PREFIX: &str = "connections.json.tmp-";

pub(super) fn lock(dir: &Path, mode: LockMode) -> Result<File> {
    fs::create_dir_all(dir).map_err(safe_io_error)?;
    if !fs::symlink_metadata(dir).map_err(safe_io_error)?.is_dir() {
        return Err(super::invalid("共享连接档案目录非法"));
    }
    restrict_to_owner(dir)?;
    let path = dir.join("connections.json.lock");
    let file = private_options()
        .create(true)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(safe_io_error)?;
    require_regular_file(&file).map_err(safe_io_error)?;
    restrict_file_to_owner(&path)?;
    let start = Instant::now();
    loop {
        if try_lock(&file, mode).map_err(safe_io_error)? {
            return Ok(file);
        }
        let remaining = LOCK_TIMEOUT.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            return Err(Error::new(
                ErrorCode::PROFILE_LOCKED,
                ErrorCategory::Timeout,
                "等待共享连接档案锁超时，请重试",
            ));
        }
        thread::sleep(remaining.min(LOCK_POLL_INTERVAL));
    }
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options
}

fn require_regular_file(file: &File) -> io::Result<()> {
    let metadata = file.metadata()?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::ErrorKind::InvalidData.into());
        }
    }
    if metadata.is_file() {
        Ok(())
    } else {
        Err(io::ErrorKind::InvalidData.into())
    }
}

pub(super) fn read(path: &Path) -> io::Result<Vec<u8>> {
    let file = private_options().read(true).open(path)?;
    require_regular_file(&file)?;
    let mut payload = Vec::new();
    file.take(MAX_DOCUMENT_BYTES as u64 + 1)
        .read_to_end(&mut payload)?;
    if payload.len() > MAX_DOCUMENT_BYTES {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(payload)
}

pub(super) fn clear_temporary_files(dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir).map_err(safe_io_error)? {
        let entry = entry.map_err(safe_io_error)?;
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with(TEMPORARY_PREFIX)
        {
            continue;
        }
        if entry.file_type().map_err(safe_io_error)?.is_file() {
            fs::remove_file(entry.path()).map_err(safe_io_error)?;
        }
    }
    Ok(())
}

pub(super) fn atomic_write(path: &Path, payload: &[u8]) -> Result<()> {
    if payload.len() > MAX_DOCUMENT_BYTES {
        return Err(super::invalid("共享连接档案超过文件大小上限"));
    }
    let name = path
        .file_name()
        .ok_or_else(|| super::invalid("共享连接档案路径非法"))?;
    let mut temporary_name = name.to_os_string();
    temporary_name.push(format!(".tmp-{}", Uuid::new_v4()));
    let temporary = path.with_file_name(temporary_name);
    let file = private_options()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(safe_io_error)?;
    let result = write_and_replace(file, &temporary, path, payload);
    if result.is_err() {
        // 临时文件只属于本次操作。在原子替换已成功、仅父目录同步失败的情况下，
        // 临时名称已经消失，故清理不会碰到目标文件。
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn write_and_replace(
    mut file: File,
    temporary: &Path,
    target: &Path,
    payload: &[u8],
) -> Result<()> {
    restrict_file_to_owner(temporary)?;
    file.write_all(payload).map_err(safe_io_error)?;
    file.sync_all().map_err(safe_io_error)?;
    drop(file);
    replace(temporary, target).map_err(safe_io_error)?;
    #[cfg(unix)]
    if let Some(parent) = target.parent() {
        File::open(parent)
            .and_then(|dir| dir.sync_all())
            .map_err(safe_io_error)?;
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace(temporary: &Path, target: &Path) -> io::Result<()> {
    fs::rename(temporary, target)
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn replace(temporary: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let from: Vec<_> = temporary.as_os_str().encode_wide().chain([0]).collect();
    let to: Vec<_> = target.as_os_str().encode_wide().chain([0]).collect();
    if from[..from.len() - 1].contains(&0) || to[..to.len() - 1].contains(&0) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    // SAFETY: 两个缓冲都是有效且以 NUL 结尾的 UTF-16 路径，调用期间存活。
    let moved = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(super) fn safe_io_error(error: io::Error) -> Error {
    match error.kind() {
        io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput => {
            super::invalid("共享连接档案文件类型、格式或大小非法")
        }
        io::ErrorKind::PermissionDenied => Error::new(
            ErrorCode::IO,
            ErrorCategory::PermissionDenied,
            "无权访问共享连接档案文件",
        ),
        _ => Error::new(
            ErrorCode::IO,
            ErrorCategory::Internal,
            "共享连接档案文件操作失败",
        ),
    }
    .with_source(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_replacement_preserves_target_and_removes_its_temporary_file() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("connections.json");
        fs::create_dir(&target).unwrap();
        let original = target.join("original");
        fs::write(&original, b"original").unwrap();

        assert!(atomic_write(&target, b"new").is_err());

        assert_eq!(fs::read(original).unwrap(), b"original");
        let entries = fs::read_dir(root.path())
            .unwrap()
            .collect::<io::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path(), target);
    }
}
