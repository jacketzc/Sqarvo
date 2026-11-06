//! MariaDB 真实 TLS 握手逐档验证（#106）。
//!
//! 与 `tests/integration.rs` 分开的原因：这里的用例需要**按 TLS 档位分别建连**，
//! 而 `integration.rs` 的所有用例共用一份 `Preferred` 连接参数。分档验证还要额外
//! 环境（根证书、无 TLS 监听端、带 SAN 证书的监听端），单独一个测试二进制可以把
//! 「缺哪个变量、跳过哪一档」写清楚，也不会让共用的 CRUD 用例跟着受影响。
//!
//! 判据是服务端会话变量 `Ssl_cipher`：非空表示该会话**真的**完成了 TLS 握手。
//! 只看「连接成功」无法区分「走了 TLS」和「回退到明文」，所以每一档都查这个值。
//!
//! 环境变量（缺变量时打印跳过原因并返回，绝不静默通过）：
//!
//! | 变量 | 用途 |
//! |---|---|
//! | `SQARVO_MARIADB_URL` | 启用 TLS 的测试库（形如 `mysql://user:***@127.0.0.1:53306/db`） |
//! | `SQARVO_MARIADB_CA` | 上面那个测试库的根证书 PEM（`verify-full` 主机名不匹配用例） |
//! | `SQARVO_MARIADB_NO_TLS_URL` | 未启用 TLS 的测试库（`preferred` 回退路径） |
//! | `SQARVO_MARIADB_SAN_URL` | 证书带 SAN 的监听端（`verify-ca` / `verify-full` 成功路径） |
//! | `SQARVO_MARIADB_SAN_CA` | 上面那个监听端的根证书 PEM |
//! | `SQARVO_MARIADB_SAN_SERVER_NAME` | 可选：`tls_server_name` 覆盖值；不设则用 URL 里的主机名 |
//!
//! 复现命令见 `docs/database/mariadb.md`「TLS 逐档验证」。

use std::collections::BTreeMap;

use sqarvo_common::{ErrorCategory, QueryId};
use sqarvo_driver_api::{
    ConnectionParams, Driver, DriverSession, ExecuteRequest, Secret, TlsMode, Value,
};
use sqarvo_driver_mariadb::{MariaDbDriver, error};

/// 「必须 TLS」档位的判据：`SHOW SESSION STATUS` 里这个变量的值。
const CIPHER_STATUS: &str = "SHOW SESSION STATUS LIKE 'Ssl_cipher'";

#[tokio::test]
async fn disabled_tls_connects_without_cipher() {
    let Some(target) = Target::from_env("SQARVO_MARIADB_URL") else {
        return;
    };
    let session = connect(&target, TlsMode::Disabled, None, None).await;
    let cipher = ssl_cipher(&*session).await;
    assert_eq!(
        cipher, "",
        "disabled 档不能协商出密码套件，实际：{cipher:?}"
    );
    session.close().await.expect("关闭会话");
}

#[tokio::test]
async fn preferred_tls_negotiates_cipher() {
    let Some(target) = Target::from_env("SQARVO_MARIADB_URL") else {
        return;
    };
    let session = connect(&target, TlsMode::Preferred, None, None).await;
    let cipher = ssl_cipher(&*session).await;
    assert!(!cipher.is_empty(), "preferred 档应完成 TLS 握手");
    println!("preferred：Ssl_cipher = {cipher}");
    session.close().await.expect("关闭会话");
}

#[tokio::test]
async fn required_tls_negotiates_cipher() {
    let Some(target) = Target::from_env("SQARVO_MARIADB_URL") else {
        return;
    };
    let session = connect(&target, TlsMode::Required, None, None).await;
    let cipher = ssl_cipher(&*session).await;
    assert!(!cipher.is_empty(), "required 档应完成 TLS 握手");
    println!("required：Ssl_cipher = {cipher}");
    session.close().await.expect("关闭会话");
}

/// 证书链校验（`verify-ca`）：把监听端的根证书固定为唯一信任根。
///
/// 注意：本档位在当前依赖组合下**并不跳过主机名校验**——`mysql_async` 0.37.1 的
/// `skip_domain_validation` 靠匹配错误字符串 `NotValidForName` 实现，而 rustls 0.23
/// 的 `Display` 输出是「invalid peer certificate: certificate not valid for name …」，
/// 匹配不到，于是 `verify-ca` 实际等同 `verify-full`。因此这里用**证书带 SAN** 的
/// 监听端取证；对无 SAN 的证书它必然被拒绝（实测见 `docs/database/mariadb.md`）。
#[tokio::test]
async fn verify_ca_trusts_pinned_root_certificate() {
    let Some(target) = Target::from_env("SQARVO_MARIADB_SAN_URL") else {
        eprintln!(
            "跳过：未设置 SQARVO_MARIADB_SAN_URL（证书带 SAN 的测试库，用于 verify-ca 成功路径）"
        );
        return;
    };
    let Some(ca) = env_value("SQARVO_MARIADB_SAN_CA") else {
        eprintln!("跳过：未设置 SQARVO_MARIADB_SAN_CA（SQARVO_MARIADB_SAN_URL 的根证书 PEM）");
        return;
    };
    let session = connect(&target, TlsMode::VerifyCa, Some(&ca), None).await;
    let cipher = ssl_cipher(&*session).await;
    assert!(!cipher.is_empty(), "verify-ca 档应完成 TLS 握手");
    println!("verify-ca：Ssl_cipher = {cipher}");
    session.close().await.expect("关闭会话");
}

/// `verify-full` 在证书**没有**匹配的主机名时必须失败，且要映射成 TLS 类错误码
/// （`error::CONNECTION_FAILED` / `Unavailable`，文案「TLS 握手失败」）。
#[tokio::test]
async fn verify_full_rejects_hostname_mismatch() {
    let Some(target) = Target::from_env("SQARVO_MARIADB_URL") else {
        return;
    };
    let Some(ca) = env_value("SQARVO_MARIADB_CA") else {
        eprintln!(
            "跳过：未设置 SQARVO_MARIADB_CA（根证书 PEM 路径，用于 verify-ca / verify-full）"
        );
        return;
    };
    let (params, secret) = target.params(TlsMode::VerifyFull, Some(&ca), None);
    let driver = MariaDbDriver::new();
    let failure = driver
        .test_connection(&params, Some(&secret))
        .await
        .expect_err("证书里没有 127.0.0.1，verify-full 必须拒绝");
    assert_eq!(failure.code(), error::CONNECTION_FAILED);
    assert_eq!(failure.category(), ErrorCategory::Unavailable);
    println!(
        "verify-full（主机名不匹配）：code={} category={:?} message={}",
        failure.code(),
        failure.category(),
        failure.message()
    );
}

/// `verify-full` 的成功路径：监听端证书带 `IP:127.0.0.1` / `DNS:localhost` SAN。
#[tokio::test]
async fn verify_full_accepts_matching_certificate() {
    let Some(target) = Target::from_env("SQARVO_MARIADB_SAN_URL") else {
        eprintln!(
            "跳过：未设置 SQARVO_MARIADB_SAN_URL（证书带 SAN 的测试库，用于 verify-full 成功路径）"
        );
        return;
    };
    let Some(ca) = env_value("SQARVO_MARIADB_SAN_CA") else {
        eprintln!("跳过：未设置 SQARVO_MARIADB_SAN_CA（SQARVO_MARIADB_SAN_URL 的根证书 PEM）");
        return;
    };
    let server_name = env_value("SQARVO_MARIADB_SAN_SERVER_NAME");
    let session = connect(
        &target,
        TlsMode::VerifyFull,
        Some(&ca),
        server_name.as_deref(),
    )
    .await;
    let cipher = ssl_cipher(&*session).await;
    assert!(!cipher.is_empty(), "verify-full 档应完成 TLS 握手");
    println!("verify-full：Ssl_cipher = {cipher}");
    session.close().await.expect("关闭会话");
}

/// `preferred` 的降级路径：服务端没有 TLS 监听时连接必须成功，并带
/// `mariadb.tls_fallback` 告警（告警只在 `test_connection` 的报告里）。
#[tokio::test]
async fn preferred_falls_back_to_plaintext_and_warns() {
    let Some(target) = Target::from_env("SQARVO_MARIADB_NO_TLS_URL") else {
        eprintln!(
            "跳过：未设置 SQARVO_MARIADB_NO_TLS_URL（未启用 TLS 的测试库，用于 preferred 回退路径）"
        );
        return;
    };
    let (params, secret) = target.params(TlsMode::Preferred, None, None);
    let driver = MariaDbDriver::new();
    let report = driver
        .test_connection(&params, Some(&secret))
        .await
        .expect("preferred 在无 TLS 监听端应回退明文并连接成功");
    assert!(report.reachable);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.code == "mariadb.tls_fallback"),
        "回退必须带 mariadb.tls_fallback 告警，实际：{:?}",
        report.warnings
    );
    println!(
        "preferred 回退：version={:?} warnings={:?}",
        report.server_version, report.warnings
    );

    // 回退后的会话必须是明文（Ssl_cipher 为空），否则「回退」只是文案。
    let session = connect(&target, TlsMode::Preferred, None, None).await;
    let cipher = ssl_cipher(&*session).await;
    assert_eq!(cipher, "", "回退后的会话不应有密码套件，实际：{cipher:?}");
    session.close().await.expect("关闭会话");
}

/// 反向对照：`required` 在同一个无 TLS 监听端必须**报错**而不是回退——
/// 证明上面的 `mariadb.tls_fallback` 只属于 `preferred` 语义。
#[tokio::test]
async fn required_fails_on_server_without_tls() {
    let Some(target) = Target::from_env("SQARVO_MARIADB_NO_TLS_URL") else {
        eprintln!(
            "跳过：未设置 SQARVO_MARIADB_NO_TLS_URL（未启用 TLS 的测试库，用于 required 反向对照）"
        );
        return;
    };
    let (params, secret) = target.params(TlsMode::Required, None, None);
    let driver = MariaDbDriver::new();
    let failure = driver
        .test_connection(&params, Some(&secret))
        .await
        .expect_err("required 在没有 TLS 的服务端必须失败");
    println!(
        "required（服务端无 TLS）：code={} category={:?} message={}",
        failure.code(),
        failure.category(),
        failure.message()
    );
    // 只断言错误码：分类目前是 `Internal`（`mysql_async` 把「服务端不支持 SSL」
    // 归到 `Error::Other`），见 `docs/database/mariadb.md`「TLS 逐档验证」的已知缺口。
    assert_eq!(failure.code(), error::CONNECTION_FAILED);
}

// ---------------------------------------------------------------------------
// 辅助
// ---------------------------------------------------------------------------

/// 从 URL 解出的连接目标。
struct Target {
    host: String,
    port: u16,
    database: Option<String>,
    username: String,
    password: String,
}

impl Target {
    /// 读取环境变量并解析；未设置时打印跳过原因。
    fn from_env(name: &str) -> Option<Self> {
        let Some(url) = env_value(name) else {
            eprintln!("跳过：未设置 {name}（示例 mysql://user:***@127.0.0.1:53306/db）");
            return None;
        };
        let rest = url
            .strip_prefix("mysql://")
            .unwrap_or_else(|| panic!("{name} 必须以 mysql:// 开头"));
        let (auth, authority) = rest
            .split_once('@')
            .unwrap_or_else(|| panic!("{name} 缺少 @"));
        let (user, password) = auth
            .split_once(':')
            .unwrap_or_else(|| panic!("{name} 缺少密码"));
        let (endpoint, database) = match authority.split_once('/') {
            Some((endpoint, database)) => (endpoint, Some(database.to_owned())),
            None => (authority, None),
        };
        let (host, port) = match endpoint.split_once(':') {
            Some((host, port)) => (host.to_owned(), port.parse().expect("端口必须是数字")),
            None => (endpoint.to_owned(), 3306),
        };
        Some(Self {
            host,
            port,
            database,
            username: user.to_owned(),
            password: password.to_owned(),
        })
    }

    fn params(
        &self,
        tls: TlsMode,
        ca: Option<&str>,
        server_name: Option<&str>,
    ) -> (ConnectionParams, Secret) {
        let mut properties = BTreeMap::new();
        if let Some(ca) = ca {
            properties.insert("ssl_ca".to_owned(), ca.to_owned());
        }
        let params = ConnectionParams {
            driver_id: "mariadb".parse().unwrap(),
            host: self.host.clone(),
            port: self.port,
            database: self.database.clone(),
            username: Some(self.username.clone()),
            credential_ref: Some("sqarvo:conn/tls-handshake".to_owned()),
            tls,
            tls_server_name: server_name.map(str::to_owned),
            properties,
        };
        (params, Secret::new(self.password.clone()))
    }
}

fn env_value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

async fn connect(
    target: &Target,
    tls: TlsMode,
    ca: Option<&str>,
    server_name: Option<&str>,
) -> Box<dyn DriverSession> {
    let (params, secret) = target.params(tls, ca, server_name);
    MariaDbDriver::new()
        .connect(&params, Some(&secret))
        .await
        .unwrap_or_else(|error| panic!("{tls:?} 档建连失败：{error:?}"))
}

/// 读会话的 `Ssl_cipher`；未启用 TLS 时服务端返回空字符串。
async fn ssl_cipher(session: &dyn DriverSession) -> String {
    let start = session
        .execute(ExecuteRequest {
            query_id: QueryId::new(),
            sql: CIPHER_STATUS.to_owned(),
            max_rows: None,
            timeout: None,
        })
        .await
        .expect("读取 Ssl_cipher 失败");
    let batch = start.first_batch.expect("SHOW SESSION STATUS 应返回行");
    assert_eq!(batch.rows.len(), 1, "Ssl_cipher 应恰好一行");
    match &batch.rows[0][1].value {
        Value::Text(text) => text.clone(),
        other => panic!("期望文本，实际 {other:?}"),
    }
}
