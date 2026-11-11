//! Windows 访问控制（显式 DACL）的纯逻辑部分。
//!
//! Unix 用 `chmod 0600 / 0700` 表达「只有当前用户可访问」，Windows 没有等价的
//! 「模式位」：文件与目录的访问权由安全描述符里的 DACL 决定，而且新建对象默认
//! **继承**父目录的 ACE（`Users`、`Authenticated Users` 等）。因此 Windows 上要
//! 收紧端点描述文件（含会话 token）和运行时目录，必须显式写入一条「仅当前用户
//! 完全控制、且断开继承」的 DACL。
//!
//! 本模块只做两件能在 Linux 上单测的事：
//!
//! 1. 由当前用户 SID 构造 SDDL 文本（`D:P(A;;FA;;;S-1-5-…)`）；
//! 2. 把 Win32 错误码映射成[可分类错误](sqarvo_common::ErrorCategory)。
//!
//! 真正调用 `SetNamedSecurityInfoW` 的 FFI 在 `win_acl::apply` 里，只在 `cfg(windows)`
//! 编译；这样「安全语义」与「系统调用」可以分别验证：语义在本机 CI 上跑，系统调用在
//! Windows 实机上跑（见 `docs/architecture/local-ipc.md`）。

use std::path::Path;

use sqarvo_common::{Error, ErrorCategory, ErrorCode, Result};

/// Windows 的 P/Invoke 实现，只在 `cfg(windows)` 编译。
#[cfg(windows)]
pub(crate) mod apply;

/// 受保护对象的类型。
///
/// 它决定两件不一样的事：ACE 的继承标志（文件不能带 `OI/CI`，目录必须带，
/// 否则目录里新建的子对象会继承到**空 DACL**，连自己都打不开）和错误文案的名词。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AclTarget {
    /// 端点描述文件（含 token）。
    File,
    /// 目录（config / data / runtime）。
    Directory,
}

impl AclTarget {
    /// 错误文案里的名词。必须是固定串：会进日志，不允许拼接路径以外的内容。
    const fn noun(self) -> &'static str {
        match self {
            Self::File => "文件",
            Self::Directory => "目录",
        }
    }

    /// 唯一一条 ACE 的标记串（除 SID 以外的部分）。
    ///
    /// - 文件：`A;;FA`   —— 允许 / 不继承 / 完全访问；
    /// - 目录：`A;OICI;FA` —— 同上，但让子对象（文件 `OI`、子目录 `CI`）继承同一条
    ///   「仅当前用户完全控制」的 ACE。少了 `OICI`，在受保护目录里新建的
    ///   `backend-endpoint.json` 会继承到空 DACL，后端自己都写不进去。
    const fn ace_flags(self) -> &'static str {
        match self {
            Self::File => "A;;FA",
            Self::Directory => "A;OICI;FA",
        }
    }
}

/// Win32 `ERROR_ACCESS_DENIED`。
const ERROR_ACCESS_DENIED: u32 = 5;
/// Win32 `ERROR_FILE_NOT_FOUND`。
const ERROR_FILE_NOT_FOUND: u32 = 2;
/// Win32 `ERROR_PATH_NOT_FOUND`。
const ERROR_PATH_NOT_FOUND: u32 = 3;
/// Win32 `ERROR_PRIVILEGE_NOT_HELD`。
const ERROR_PRIVILEGE_NOT_HELD: u32 = 1314;

/// 构造「仅当前用户完全控制、且不继承」的 SDDL。
///
/// 结果形如 `D:P(A;;FA;;;S-1-5-21-…-1001)`（文件）或
/// `D:P(A;OICI;FA;;;S-1-5-21-…-1001)`（目录）：
///
/// - `D:` DACL；
/// - `P` DACL protected，断开从父目录继承的 ACE（等价于
///   `PROTECTED_DACL_SECURITY_INFORMATION`）；
/// - 一条 `(A;…;FA;;;<SID>)` ACE：该 SID 允许完全访问（`FA`）。
///
/// `sid` 必须来自当前进程 token（`win_acl::apply::current_user_sid`），
/// 这里仍然做格式校验：SDDL 是文本格式，一旦 SID 文本里混入 `;` 或 `(` 就会多出
/// 一条 ACE（例如把 `Everyone` 放进来）。校验失败按内部错误上报，绝不退化成
/// 「不收紧」。
pub(crate) fn owner_only_sddl(sid: &str, target: AclTarget) -> Result<String> {
    if !is_sid_string(sid) {
        return Err(Error::internal(
            "无法构造仅当前用户可访问的 DACL：系统返回的 SID 文本不合法",
        ));
    }
    Ok(format!("D:P({};;;{sid})", target.ace_flags()))
}

/// SID 文本的最小校验：`S-1-<数字>[-<数字>…]`，只允许数字与连字符。
///
/// 不追求完整的 SID 语法（那由系统保证），只拒绝能改变 SDDL 结构的字符和明显不成
/// 形的输入（`S-1-` 后面没有子权威、连续连字符）。
fn is_sid_string(sid: &str) -> bool {
    // 最长 SID 文本约 184 字节（前缀 + 15 个 32 位子权威），这里给足余量后拒绝超长输入。
    const MAX_LEN: usize = 256;
    let Some(rest) = sid.strip_prefix("S-1-") else {
        return false;
    };
    sid.len() <= MAX_LEN
        && rest.chars().next().is_some_and(|c| c.is_ascii_digit())
        && !rest.ends_with('-')
        && !rest.contains("--")
        && rest.chars().all(|c| c.is_ascii_digit() || c == '-')
}

/// 把收紧 DACL 时拿到的 Win32 错误码映射成可分类错误。
///
/// 与 Unix 分支语义一致：失败**必须**返回错误（不能「尽力而为」后继续启动），
/// 因为端点文件里是会话 token，权限没收紧就等于把 token 交给同机其他用户。
/// 错误文案只含对象名词、路径与错误码——路径里不会有 token，token 也从不进入
/// 错误构造的任何参数。
pub(crate) fn classify_acl_error(target: AclTarget, path: &Path, code: u32) -> Error {
    let (category, hint) = match code {
        ERROR_ACCESS_DENIED | ERROR_PRIVILEGE_NOT_HELD => (
            ErrorCategory::PermissionDenied,
            "当前账户没有修改该对象访问控制的权限",
        ),
        ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => (
            ErrorCategory::NotFound,
            "目标对象已不存在（可能被并发删除）",
        ),
        _ => (
            ErrorCategory::Internal,
            "这是系统 API 返回的未预期失败，重试不会改变结果",
        ),
    };
    Error::new(
        ErrorCode::IO,
        category,
        format!(
            "无法把{}的访问控制收紧到当前用户：{}（Win32 错误码 {code}；{hint}）",
            target.noun(),
            path.display()
        ),
    )
}

/// 读取当前进程 token 里的用户 SID 失败时的错误。
///
/// 单独一条文案：这一层发生在拿到路径之前（连「要收紧哪个对象」都还没到），
/// 前端需要能区分「读不到自己的身份」与「目标对象权限不足」。
pub(crate) fn classify_sid_error(code: u32) -> Error {
    let category = match code {
        ERROR_ACCESS_DENIED | ERROR_PRIVILEGE_NOT_HELD => ErrorCategory::PermissionDenied,
        _ => ErrorCategory::Internal,
    };
    Error::new(
        ErrorCode::IO,
        category,
        format!("无法读取当前进程的用户 SID（收紧 Windows 访问控制需要它；Win32 错误码 {code}）"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER_SID: &str = "S-1-5-21-1004336348-1177238915-682003330-1001";

    #[test]
    fn file_sddl_is_owner_only_and_protected() {
        let sddl = owner_only_sddl(USER_SID, AclTarget::File).unwrap();
        // `D:P` = 显式 DACL 且断开继承；唯一 ACE 是当前用户完全控制。
        assert_eq!(sddl, format!("D:P(A;;FA;;;{USER_SID})"));
        assert!(sddl.starts_with("D:P("));
        assert_eq!(sddl.matches('(').count(), 1, "只能有一条 ACE");
    }

    #[test]
    fn directory_sddl_lets_children_inherit_owner_only() {
        let sddl = owner_only_sddl(USER_SID, AclTarget::Directory).unwrap();
        assert_eq!(sddl, format!("D:P(A;OICI;FA;;;{USER_SID})"));
        // 目录必须带 OI/CI：否则目录里新建的文件继承到空 DACL，后端自己都写不进去。
        assert!(sddl.contains("OICI"));
        assert_eq!(sddl.matches('(').count(), 1, "只能有一条 ACE");
    }

    #[test]
    fn sddl_rejects_sid_text_that_could_add_aces() {
        for sid in [
            "",
            "S-1-",
            "S-1-5-",
            "S-1-5--21-1",
            "S-1-5-21-1)(A;;GA;;;WD",
            "S-1-5-21-1;FA;;;WD",
            "S-1-5-21-1 (A;;GA;;;WD)",
            "Everyone",
            "S-2-5-21-1",
            "s-1-5-21-1",
            &"S-1-5-".to_owned().repeat(100),
        ] {
            let error = owner_only_sddl(sid, AclTarget::File).unwrap_err();
            assert_eq!(error.code(), ErrorCode::INTERNAL, "{sid}");
            assert_eq!(error.category(), ErrorCategory::Internal);
            // 错误文案不回显输入：SID 文本本身不敏感，但保持与其它模块一致的
            // 「不抄写输入」原则，避免将来有人把路径/用户名拼进来。
            assert!(sid.is_empty() || !error.to_string().contains(sid));
        }
    }

    #[test]
    fn access_denied_is_permission_denied() {
        for code in [5, 1314] {
            let error = classify_acl_error(AclTarget::Directory, Path::new("C:\\run"), code);
            assert_eq!(error.code(), ErrorCode::IO);
            assert_eq!(error.category(), ErrorCategory::PermissionDenied);
            assert!(!error.category().is_retryable(), "用户改权限前重试没有意义");
            assert!(error.to_string().contains(&code.to_string()));
        }
    }

    #[test]
    fn vanished_object_is_not_found() {
        for code in [2, 3] {
            let error = classify_acl_error(AclTarget::File, Path::new("C:\\run\\e.json"), code);
            assert_eq!(error.category(), ErrorCategory::NotFound);
        }
    }

    #[test]
    fn unexpected_codes_are_internal_and_classified() {
        for code in [0, 8, 87, 1338, 0xffff_ffff] {
            let error = classify_acl_error(AclTarget::File, Path::new("C:\\x"), code);
            assert_eq!(error.category(), ErrorCategory::Internal);
            assert_eq!(error.code(), ErrorCode::IO);
        }
    }

    #[test]
    fn sid_read_errors_are_classified() {
        assert_eq!(
            classify_sid_error(5).category(),
            ErrorCategory::PermissionDenied
        );
        assert_eq!(classify_sid_error(87).category(), ErrorCategory::Internal);
        assert_eq!(classify_sid_error(5).code(), ErrorCode::IO);
    }

    /// 端点文件是 token 的载体：错误文案必须能被安全写进日志。
    #[test]
    fn acl_error_text_never_contains_a_token() {
        let token = "0123456789abcdef".repeat(4);
        let path = Path::new("C:\\Users\\u\\AppData\\Local\\sqarvo\\run\\backend-endpoint.json");
        let error = classify_acl_error(AclTarget::File, path, 5);
        let rendered = format!("{error} {error:?}");
        assert!(!rendered.contains(&token));
        assert!(!rendered.contains("token"));
        assert!(rendered.contains("Win32 错误码 5"));
    }
}
