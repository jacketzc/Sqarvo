//! 内存凭据存储：测试与无桌面会话的环境使用。
//!
//! 语义与真实实现严格对齐，避免「测试里通过、桌面上失败」：
//!
//! - 引用必须通过 [`validate_reference`]；
//! - 同一引用重复写入覆盖旧值；
//! - 读取不存在的引用返回 `credential.not_found`；
//! - 删除不存在的引用视为成功。
//!
//! 数据只在进程内存里，进程退出即消失。它**不是**明文回退方案：不会读写任何配置文件、
//! 环境变量或日志，也不持久化任何内容。生产装配必须走
//! [`open_default_credential_store`](crate::credentials::open_default_credential_store)。

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use sqarvo_common::Result;

use super::{CredentialStore, Secret, not_found_error, validate_reference};

/// 进程内凭据存储。
///
/// `Debug` 只打印条目数量：内层 `Secret` 的 `Debug` 已脱敏，但仍不把映射内容暴露给
/// 偶然的 `dbg!`/日志。
#[derive(Default)]
pub struct MemoryCredentialStore {
    entries: Mutex<HashMap<String, Secret>>,
}

impl MemoryCredentialStore {
    /// 创建空存储。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前保存的条目数量（测试与诊断用）。
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// 是否没有任何条目。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 某个引用是否存在。
    #[must_use]
    pub fn contains(&self, reference: &str) -> bool {
        self.lock().contains_key(reference)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Secret>> {
        // 锁中毒只可能来自持有者 panic；内存表本身没有不变量会被破坏，
        // 取回内部数据继续用比在这里再 panic 一次更合适。
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl std::fmt::Debug for MemoryCredentialStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryCredentialStore")
            .field("entries", &self.len())
            .finish()
    }
}

impl CredentialStore for MemoryCredentialStore {
    fn get(&self, reference: &str) -> Result<Secret> {
        validate_reference(reference)?;
        self.lock()
            .get(reference)
            .cloned()
            .ok_or_else(not_found_error)
    }

    fn set(&self, reference: &str, secret: &Secret) -> Result<()> {
        validate_reference(reference)?;
        self.lock().insert(reference.to_owned(), secret.clone());
        Ok(())
    }

    fn delete(&self, reference: &str) -> Result<()> {
        validate_reference(reference)?;
        self.lock().remove(reference);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqarvo_common::{ErrorCategory, ErrorCode};

    const REF: &str = "sqarvo:conn/3f9a1c4e-7b2d-4a6f-9c11-0d2b8e5a71f4";

    #[test]
    fn roundtrip_and_overwrite() {
        let store = MemoryCredentialStore::new();
        store.set(REF, &Secret::new("first")).unwrap();
        assert_eq!(store.get(REF).unwrap().expose(), "first");

        store.set(REF, &Secret::new("second")).unwrap();
        assert_eq!(store.get(REF).unwrap().expose(), "second");
        assert_eq!(store.len(), 1, "覆盖写不应产生第二个条目");
    }

    #[test]
    fn missing_reference_reports_not_found() {
        let store = MemoryCredentialStore::new();
        let error = store.get(REF).unwrap_err();
        assert_eq!(error.code(), ErrorCode::CREDENTIAL_NOT_FOUND);
        assert_eq!(error.category(), ErrorCategory::NotFound);
    }

    #[test]
    fn delete_is_idempotent() {
        let store = MemoryCredentialStore::new();
        store.set(REF, &Secret::new("value")).unwrap();
        store.delete(REF).unwrap();
        assert!(store.is_empty());
        store.delete(REF).unwrap();
        store.delete(REF).unwrap();
    }

    #[test]
    fn references_are_namespaced() {
        let other = "sqarvo:conn/0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d";
        let store = MemoryCredentialStore::new();
        store.set(REF, &Secret::new("a")).unwrap();
        store.set(other, &Secret::new("b")).unwrap();
        assert_eq!(store.get(REF).unwrap().expose(), "a");
        assert_eq!(store.get(other).unwrap().expose(), "b");
        store.delete(REF).unwrap();
        assert_eq!(store.get(other).unwrap().expose(), "b");
    }

    #[test]
    fn invalid_reference_is_rejected_before_touching_data() {
        let store = MemoryCredentialStore::new();
        let error = store.set("conn-1", &Secret::new("value")).unwrap_err();
        assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);
        assert!(store.is_empty());
    }

    #[test]
    fn long_secret_is_preserved() {
        let store = MemoryCredentialStore::new();
        let long = "p".repeat(64 * 1024);
        store.set(REF, &Secret::new(long.clone())).unwrap();
        assert_eq!(store.get(REF).unwrap().expose(), long);
    }

    #[test]
    fn debug_does_not_leak_secret() {
        let store = MemoryCredentialStore::new();
        store.set(REF, &Secret::new("hunter2")).unwrap();
        let rendered = format!("{store:?}");
        assert!(!rendered.contains("hunter2"));
        assert!(!rendered.contains(REF));
    }
}
