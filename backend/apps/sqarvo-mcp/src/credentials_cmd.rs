//! 凭据管理子命令（#207 实现；V3.1 §3.2）。
//!
//! 契约：
//! - `set`：缺省自动生成合法 `sqarvo:conn/<uuid v4>` 引用并打印；密码经 rpassword
//!   交互读取（不回显、禁 argv/env 传密、回读确认）；
//! - `delete`：幂等删除；
//! - 两者都走 `sqarvo-local-store` 的 `CredentialStore`（同步 trait，必须
//!   `spawn_blocking`）；密钥环不可用如实报 `credential.unavailable`，永不明文回退
//!   （ADR 0012）；
//! - 管理模式不向 stdout 输出 MCP 协议消息。
//!
//! # 交互安全
//!
//! - 密码只从终端读：**不接受** `--password`、位置参数或环境变量传值（多一个入口，
//!   就多一份进 shell history / 进程表 / 崩溃报告的机会）。
//! - 非交互环境（管道、重定向、CI）在**打开密钥环之前**就报错退出：那里的「读密码」
//!   要么回显到日志，要么在无人值守时永远阻塞。
//! - 输入两遍并要求一致，避免把打错的密码写进密钥环后连接时才暴露。
//! - 提示文案走 stderr，stdout 只留「已写入 + 引用」这类人类可读结果，方便脚本取引用。

use std::io::{IsTerminal, Write};
use std::sync::Arc;

use clap::Subcommand;
use sqarvo_common::{Error, ErrorCategory, ErrorCode, Result};
use sqarvo_local_store::credentials::REFERENCE_PREFIX;
use sqarvo_local_store::{
    CredentialStore, Secret, open_default_credential_store, validate_reference,
};
use uuid::Uuid;

/// `credentials` 子命令的动作。CLI 类型放包里，供 lib 与 bin 共用。
#[derive(Subcommand)]
pub enum CredentialsAction {
    /// 写入密钥环凭据；不传 reference 时自动生成并打印。
    Set {
        /// 凭据引用；必须形如 `sqarvo:conn/<uuid v4>`，缺省自动生成。
        reference: Option<String>,
    },
    /// 删除密钥环凭据（不存在视为成功，幂等）。
    Delete {
        /// 待删除的凭据引用原文。
        reference: String,
    },
}

/// 密码读取抽象。
///
/// 生产实现读终端；测试注入一个返回固定值的闭包，避免在 CI 里真的等待输入。
pub trait PasswordPrompt {
    /// 显示提示并读取一行（应当不回显）。
    fn read(&mut self, label: &str) -> Result<String>;
}

/// 终端密码读取（rpassword，Unix 走 `/dev/tty`，Windows 走控制台 API）。
pub struct TerminalPrompt {
    stdin_is_tty: bool,
}

impl TerminalPrompt {
    /// 构造前先确认 stdin 是终端；不是就直接失败。
    pub fn new() -> Result<Self> {
        ensure_interactive(std::io::stdin().is_terminal())?;
        Ok(Self { stdin_is_tty: true })
    }

    /// 是否处于交互终端（构造成功时恒为 `true`）。
    #[must_use]
    pub const fn is_interactive(&self) -> bool {
        self.stdin_is_tty
    }
}

impl PasswordPrompt for TerminalPrompt {
    fn read(&mut self, label: &str) -> Result<String> {
        eprint!("{label}");
        // 提示必须立刻可见：否则用户会以为命令卡住了。
        let _ = std::io::stderr().flush();
        let value = rpassword::read_password().map_err(|error| {
            Error::new(
                ErrorCode::MCP_INVALID_ARGUMENT,
                ErrorCategory::FailedPrecondition,
                format!("无法从终端读取密码：{error}"),
            )
        })?;
        eprintln!();
        Ok(value)
    }
}

/// 非交互环境下的统一拒绝。
///
/// 单独拆成纯函数是为了能在测试里断言两个分支，而不依赖 CI 的 stdin 到底是什么。
pub fn ensure_interactive(stdin_is_tty: bool) -> Result<()> {
    if stdin_is_tty {
        return Ok(());
    }
    Err(Error::new(
        ErrorCode::MCP_INVALID_ARGUMENT,
        ErrorCategory::FailedPrecondition,
        "凭据输入需要交互式终端（stdin 不是 TTY）：请在终端里运行 `sqarvo-mcp credentials set`；\
         不接受命令行参数或环境变量传密码",
    ))
}

/// 生成一个新的凭据引用。
///
/// 格式固定为 `sqarvo:conn/<小写带连字符的 uuid v4>`（ADR 0012）；生成后立刻自检，
/// 免得格式规则变化时静默产出不可用的引用。
#[must_use]
pub fn generate_reference() -> String {
    let reference = format!("{REFERENCE_PREFIX}{}", Uuid::new_v4());
    debug_assert!(
        validate_reference(&reference).is_ok(),
        "自动生成的引用必须通过 validate_reference"
    );
    reference
}

/// 写入凭据（同步；调用方负责 `spawn_blocking`）。
///
/// 返回实际使用的引用：调用方传了就原样返回，没传则用 [`generate_reference`]。
pub fn set_credential(
    store: &Arc<dyn CredentialStore>,
    reference: Option<String>,
    prompt: &mut dyn PasswordPrompt,
) -> Result<String> {
    let reference = reference.unwrap_or_else(generate_reference);
    // 先校验引用：格式不对时不要先让用户输两遍密码。
    validate_reference(&reference)?;
    let password = prompt.read("数据库密码（输入不回显）：")?;
    let confirmed = prompt.read("再次输入确认：")?;
    if password != confirmed {
        // 文案不回显输入内容。
        return Err(Error::invalid_argument(
            ErrorCode::MCP_INVALID_ARGUMENT,
            "两次输入的密码不一致，未写入任何内容",
        ));
    }
    if password.is_empty() {
        return Err(Error::invalid_argument(
            ErrorCode::MCP_INVALID_ARGUMENT,
            "密码为空：不需要密码的账号请不要配置 credential_ref",
        ));
    }
    store.set(&reference, &Secret::new(password))?;
    Ok(reference)
}

/// 删除凭据（同步；调用方负责 `spawn_blocking`）。
///
/// 引用不存在视为成功（与 `CredentialStore::delete` 的幂等语义一致）。
pub fn delete_credential(store: &Arc<dyn CredentialStore>, reference: &str) -> Result<()> {
    validate_reference(reference)?;
    store.delete(reference)
}

/// 执行管理子命令。
///
/// 不启动 MCP server，也不输出任何协议消息；结果是人类可读文本（stderr 提示 +
/// stdout 结果）。
pub async fn run(action: CredentialsAction) -> Result<()> {
    match action {
        CredentialsAction::Set { reference } => {
            // TTY 检查放在最前面：非交互环境不碰密钥环、不读 stdin。
            let mut prompt = TerminalPrompt::new()?;
            let store = open_store().await;
            let implementation = store.implementation();
            let store_for_task = Arc::clone(&store);
            let reference = tokio::task::spawn_blocking(move || {
                set_credential(&store_for_task, reference, &mut prompt)
            })
            .await
            .map_err(|error| join_error(&error))??;
            println!("已写入凭据：{reference}");
            println!("把该引用填进 mcp.json 的 credential_ref 字段。凭据实现：{implementation}");
            Ok(())
        }
        CredentialsAction::Delete { reference } => {
            let store = open_store().await;
            let reference_clone = reference.clone();
            let store_clone = Arc::clone(&store);
            tokio::task::spawn_blocking(move || delete_credential(&store_clone, &reference_clone))
                .await
                .map_err(|error| join_error(&error))??;
            println!("已删除凭据（若原本不存在则视为成功）：{reference}");
            Ok(())
        }
    }
}

async fn open_store() -> Arc<dyn CredentialStore> {
    match tokio::task::spawn_blocking(open_default_credential_store).await {
        Ok(store) => store,
        Err(error) => {
            tracing::warn!(%error, "凭据存储装配任务异常，退回不可用实现");
            Arc::new(sqarvo_local_store::UnavailableCredentialStore)
        }
    }
}

fn join_error(error: &tokio::task::JoinError) -> Error {
    Error::new(
        ErrorCode::MCP_INTERNAL,
        ErrorCategory::Internal,
        format!("凭据操作任务异常终止：{error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqarvo_local_store::MemoryCredentialStore;

    /// 测试用密码输入：按顺序返回预设值。
    struct ScriptedPrompt {
        answers: Vec<String>,
        prompts: Vec<String>,
    }

    impl ScriptedPrompt {
        fn new(answers: &[&str]) -> Self {
            Self {
                answers: answers.iter().map(|value| (*value).to_owned()).collect(),
                prompts: Vec::new(),
            }
        }
    }

    impl PasswordPrompt for ScriptedPrompt {
        fn read(&mut self, label: &str) -> Result<String> {
            self.prompts.push(label.to_owned());
            if self.answers.is_empty() {
                return Err(Error::internal("测试脚本没有更多输入"));
            }
            Ok(self.answers.remove(0))
        }
    }

    fn store() -> Arc<dyn CredentialStore> {
        Arc::new(MemoryCredentialStore::new())
    }

    #[test]
    fn generated_reference_is_a_valid_conn_reference() {
        let reference = generate_reference();
        assert!(reference.starts_with(REFERENCE_PREFIX));
        validate_reference(&reference).expect("自动生成的引用必须合法");
        assert_ne!(reference, generate_reference(), "每次生成都应是新引用");
    }

    #[test]
    fn set_generates_a_reference_and_stores_the_secret() {
        let store = store();
        let mut prompt = ScriptedPrompt::new(&["pw-123", "pw-123"]);
        let reference = set_credential(&store, None, &mut prompt).expect("写入成功");
        validate_reference(&reference).expect("生成的引用合法");
        assert_eq!(prompt.prompts.len(), 2, "必须输入两遍确认");
        let stored = store.get(&reference).expect("能读回凭据");
        assert_eq!(stored.expose(), "pw-123");
    }

    #[test]
    fn set_with_explicit_reference_keeps_it() {
        let store = store();
        let reference = format!("{REFERENCE_PREFIX}{}", Uuid::new_v4());
        let mut prompt = ScriptedPrompt::new(&["pw", "pw"]);
        let used = set_credential(&store, Some(reference.clone()), &mut prompt).expect("写入成功");
        assert_eq!(used, reference);
        assert!(store.get(&reference).is_ok());
    }

    #[test]
    fn mismatched_confirmation_writes_nothing() {
        let store = store();
        let mut prompt = ScriptedPrompt::new(&["pw-a", "pw-b"]);
        let error = set_credential(&store, None, &mut prompt).expect_err("不一致必须失败");
        assert_eq!(error.code(), ErrorCode::MCP_INVALID_ARGUMENT);
        assert!(!error.message().contains("pw-a"), "错误文案不得回显密码");
        assert!(!error.message().contains("pw-b"));
    }

    #[test]
    fn malformed_reference_is_rejected_before_prompting() {
        let store = store();
        let mut prompt = ScriptedPrompt::new(&[]);
        let error = set_credential(&store, Some("sqarvo:CONN/BAD".to_owned()), &mut prompt)
            .expect_err("格式非法");
        assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);
        assert!(prompt.prompts.is_empty(), "格式非法时不该让用户输密码");
    }

    #[test]
    fn empty_password_is_rejected() {
        let store = store();
        let mut prompt = ScriptedPrompt::new(&["", ""]);
        let error = set_credential(&store, None, &mut prompt).expect_err("空密码必须拒绝");
        assert!(error.message().contains("密码为空"));
    }

    #[test]
    fn delete_is_idempotent() {
        let store = store();
        let reference = format!("{REFERENCE_PREFIX}{}", Uuid::new_v4());
        store
            .set(&reference, &Secret::new("pw"))
            .expect("写入内存凭据");
        delete_credential(&store, &reference).expect("第一次删除成功");
        delete_credential(&store, &reference).expect("第二次删除仍成功（幂等）");
        assert!(store.get(&reference).is_err());
    }

    #[test]
    fn non_interactive_stdin_is_rejected() {
        let error = ensure_interactive(false).expect_err("非 TTY 必须拒绝");
        assert_eq!(error.category(), ErrorCategory::FailedPrecondition);
        assert!(error.message().contains("交互式终端"));
        ensure_interactive(true).expect("TTY 允许继续");
    }
}
