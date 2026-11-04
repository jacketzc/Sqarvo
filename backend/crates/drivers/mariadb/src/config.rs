//! 连接参数校验与 `mysql_async` 选项构造（TLS 模式映射见 ADR 0011）。

use std::path::PathBuf;
use std::time::Duration;

use mysql_async::{ClientIdentity, Opts, OptsBuilder, SslOpts};
use sqarvo_common::{Error, ErrorCode, Result};
use sqarvo_driver_api::{ConnectionParams, Secret, TlsMode};

use crate::driver::DRIVER_ID;

/// 默认连接超时。
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 允许的连接超时上限。
const MAX_CONNECT_TIMEOUT: Duration = Duration::from_secs(300);
/// 允许的连接超时下限。
const MIN_CONNECT_TIMEOUT: Duration = Duration::from_millis(100);

/// `properties` 允许的键。
const PROP_SSL_CA: &str = "ssl_ca";
const PROP_SSL_CERT: &str = "ssl_cert";
const PROP_SSL_KEY: &str = "ssl_key";
const PROP_CONNECT_TIMEOUT_MS: &str = "connect_timeout_ms";

/// 归一化后的连接配置。
#[derive(Clone)]
pub(crate) struct MariaDbConfig {
    /// 已构造好的驱动选项（含密码）。
    pub(crate) opts: Opts,
    /// 建连超时。
    pub(crate) connect_timeout: Duration,
    /// TLS 模式；`Preferred` 需要「先 TLS，失败再回退明文」。
    pub(crate) tls: TlsMode,
    /// 回退到明文时产生的告警（仅 `Preferred`）。
    pub(crate) tls_fallback_allowed: bool,
}

impl std::fmt::Debug for MariaDbConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Opts 含密码，绝不打印内容。
        f.debug_struct("MariaDbConfig")
            .field("tls", &self.tls)
            .field("connect_timeout", &self.connect_timeout)
            .finish_non_exhaustive()
    }
}

impl MariaDbConfig {
    /// 校验并构造配置。
    pub(crate) fn from_params(params: &ConnectionParams, secret: Option<&Secret>) -> Result<Self> {
        params.validate()?;
        if params.driver_id.as_str() != DRIVER_ID {
            return Err(config_error("该驱动只能处理 mariadb 连接参数"));
        }
        if params.port == 0 {
            return Err(config_error("MariaDB 连接必须提供端口"));
        }
        let Some(username) = params.username.as_deref().map(str::trim) else {
            return Err(config_error("MariaDB 连接必须提供用户名"));
        };
        if username.is_empty() {
            return Err(config_error("MariaDB 用户名不能为空"));
        }
        let database = params
            .database
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty());

        let mut ssl_ca = None;
        let mut ssl_cert = None;
        let mut ssl_key = None;
        let mut connect_timeout = DEFAULT_CONNECT_TIMEOUT;
        for (key, value) in &params.properties {
            match key.as_str() {
                PROP_SSL_CA => ssl_ca = Some(require_existing_path(value, PROP_SSL_CA)?),
                PROP_SSL_CERT => ssl_cert = Some(require_existing_path(value, PROP_SSL_CERT)?),
                PROP_SSL_KEY => ssl_key = Some(require_existing_path(value, PROP_SSL_KEY)?),
                PROP_CONNECT_TIMEOUT_MS => {
                    connect_timeout = parse_timeout(value)?;
                }
                other => {
                    return Err(config_error(format!("MariaDB 不支持的连接参数：{other}")));
                }
            }
        }
        if ssl_cert.is_some() != ssl_key.is_some() {
            return Err(config_error("客户端证书与私钥必须同时提供"));
        }
        if params.tls == TlsMode::Disabled && (ssl_ca.is_some() || ssl_cert.is_some()) {
            return Err(config_error("TLS 已禁用时不能提供证书参数"));
        }
        if params.tls_server_name.is_some() && params.tls == TlsMode::Disabled {
            return Err(config_error("TLS 已禁用时不能指定 TLS 主机名"));
        }

        let ssl_opts = build_ssl_opts(params, ssl_ca.as_ref(), ssl_cert, ssl_key);
        let tls_fallback_allowed = matches!(params.tls, TlsMode::Unspecified | TlsMode::Preferred);

        let mut builder = OptsBuilder::default()
            .ip_or_hostname(params.host.trim())
            .tcp_port(params.port)
            .user(Some(username))
            // 密码只在这里出现一次；Opts 的 Debug 不打印内容。
            .pass(Some(secret.map(Secret::expose).unwrap_or_default()))
            .db_name(database)
            // 连接参数里的 host 始终按主机名处理：不尝试本地 unix socket。
            .prefer_socket(false);
        if let Some(ssl_opts) = ssl_opts {
            builder = builder.ssl_opts(ssl_opts);
        }

        Ok(Self {
            opts: Opts::from(builder),
            connect_timeout,
            tls: params.tls,
            tls_fallback_allowed,
        })
    }
}

fn build_ssl_opts(
    params: &ConnectionParams,
    ssl_ca: Option<&PathBuf>,
    ssl_cert: Option<PathBuf>,
    ssl_key: Option<PathBuf>,
) -> Option<SslOpts> {
    let mut ssl_opts = match params.tls {
        TlsMode::Disabled => return None,
        // `prefer` / 未指定：先尝试 TLS，不校验证书（与 MySQL 客户端 PREFERRED 语义一致），
        // 握手失败时由连接逻辑回退明文并给出告警。
        TlsMode::Unspecified | TlsMode::Preferred | TlsMode::Required => {
            SslOpts::default().with_danger_accept_invalid_certs(true)
        }
        TlsMode::VerifyCa => SslOpts::default().with_danger_skip_domain_validation(true),
        TlsMode::VerifyFull => SslOpts::default(),
    };

    if let Some(ca) = ssl_ca {
        ssl_opts = ssl_opts.with_root_certs(vec![ca.clone().into()]);
    }
    if let (Some(cert), Some(key)) = (ssl_cert, ssl_key) {
        ssl_opts =
            ssl_opts.with_client_identity(Some(ClientIdentity::new(cert.into(), key.into())));
    }
    if let Some(server_name) = params
        .tls_server_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        ssl_opts = ssl_opts.with_danger_tls_hostname_override(Some(server_name.to_owned()));
    }
    Some(ssl_opts)
}

fn require_existing_path(value: &str, key: &str) -> Result<PathBuf> {
    let path = PathBuf::from(value.trim());
    if !path.is_file() {
        return Err(config_error(format!("{key} 指向的文件不存在")));
    }
    Ok(path)
}

fn parse_timeout(value: &str) -> Result<Duration> {
    let millis: u64 = value
        .trim()
        .parse()
        .map_err(|_| config_error("connect_timeout_ms 必须是整数毫秒"))?;
    let timeout = Duration::from_millis(millis);
    if !(MIN_CONNECT_TIMEOUT..=MAX_CONNECT_TIMEOUT).contains(&timeout) {
        return Err(config_error(
            "connect_timeout_ms 超出允许范围（100–300000）",
        ));
    }
    Ok(timeout)
}

fn config_error(message: impl Into<String>) -> Error {
    Error::invalid_argument(ErrorCode::CONFIG_INVALID, message)
}
