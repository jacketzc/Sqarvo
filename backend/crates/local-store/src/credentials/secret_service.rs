//! Linux Secret Service（`org.freedesktop.secrets`）凭据存储。
//!
//! # D-Bus 往返
//!
//! 依赖 `org.freedesktop.Secret.Service`：
//!
//! 1. `OpenSession(plain)` 拿到与本连接绑定的会话（秘密值随会话解密/加密）；
//! 2. `ReadAlias("default")` 拿到默认集合（GNOME 上是 `login` 密钥环）；
//! 3. `SearchItems(属性)` 按属性精确定位条目；
//! 4. `CreateItem(属性, 秘密, replace=true)` 写入/覆盖；
//! 5. `Item.GetSecret(会话)` 读取，`Item.Delete()` 删除。
//!
//! 条目属性严格按 ADR 0012：`application`、`profile`、`reference`，label 固定为
//! [`ITEM_LABEL`]。属性里**不含**主机、用户名、库名或密码。
//!
//! # 会话算法
//!
//! 使用 `plain` 会话：秘密值以明文字节在本地 D-Bus 上传给当前用户的密钥环守护进程。
//! D-Bus 会话总线是 `AF_UNIX` socket，内核校验对端 uid，`SO_PEERCRED` 保证只有本用户
//! 连接；`DH` 会话能防「同一用户下的进程窃听」，但同一用户下的进程本来就读得到本用户
//! 的密钥环，收益很小，而 `DH` 实现复杂度高。后续若需要防同一用户下的进程嗅探，
//! 再升级到 `DH`。
//!
//! # 用户确认提示
//!
//! 密钥环锁定或需要用户确认时，Secret Service 返回一个 *prompt* 对象，由服务端在桌面
//! 会话里弹窗。本实现**不**驱动 prompt：那需要订阅 `Completed` 信号并无限期等待桌面
//! 用户点击，而后端服务没有人替他点、也没有 UI，会把调用方（可能是 gRPC 请求线程）
//! 挂死。因此遇到 prompt 一律按「需要用户先在桌面会话处理」如实报错（`FailedPrecondition`）。

use std::collections::HashMap;
use std::fmt;
use std::sync::{Mutex, PoisonError};

use sqarvo_common::{Error, ErrorCategory, ErrorCode, Result};
use zbus::Message;
use zbus::blocking::Connection;
use zbus::zvariant::{DynamicType, OwnedObjectPath, OwnedValue, Value};

use super::{
    APPLICATION_ID, CredentialStore, ITEM_LABEL, Secret, not_found_error, validate_reference,
};

/// Secret Service 的已知名。
const SERVICE: &str = "org.freedesktop.secrets";
/// Secret Service 根对象路径。
const SERVICE_PATH: &str = "/org/freedesktop/secrets";
const SERVICE_IFACE: &str = "org.freedesktop.Secret.Service";
const COLLECTION_IFACE: &str = "org.freedesktop.Secret.Collection";
const ITEM_IFACE: &str = "org.freedesktop.Secret.Item";
const PROPERTIES_IFACE: &str = "org.freedesktop.DBus.Properties";
const DBUS_SERVICE: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";
const DBUS_IFACE: &str = "org.freedesktop.DBus";

/// 默认集合别名。
const DEFAULT_ALIAS: &str = "default";
/// 会话算法：明文（见模块文档）。
const ALGORITHM_PLAIN: &str = "plain";
/// 秘密内容类型。
const CONTENT_TYPE: &str = "text/plain";

/// `CreateItem` 属性字典里条目 label 的键（必须是全限定属性名）。
const PROP_LABEL: &str = "org.freedesktop.Secret.Item.Label";
/// `CreateItem` 属性字典里条目属性的键（值是 `a{ss}`）。
const PROP_ATTRIBUTES: &str = "org.freedesktop.Secret.Item.Attributes";

/// Secret Service 用来表示「没有对象/没有提示」的对象路径。
const NONE_PATH: &str = "/";

/// 档案名最大长度：只是防止把路径或连接串误当档案名写进密钥环属性。
const MAX_PROFILE_LEN: usize = 64;

/// Linux Secret Service 凭据存储。
///
/// 一个实例持有一条 D-Bus 连接与一个 `plain` 会话，可跨线程共享（内部状态为 `Mutex`）。
/// 构造见 [`SecretServiceCredentialStore::connect`]；一般通过
/// [`open_default_credential_store`](super::open_default_credential_store) 装配。
pub struct SecretServiceCredentialStore {
    conn: Connection,
    profile: String,
    /// `plain` 会话路径，懒加载：`OpenSession` 每条连接只需一次。
    session: Mutex<Option<OwnedObjectPath>>,
}

impl fmt::Debug for SecretServiceCredentialStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretServiceCredentialStore")
            .field("profile", &self.profile)
            .field("bus", &self.conn.unique_name().map(ToString::to_string))
            .finish_non_exhaustive()
    }
}

impl SecretServiceCredentialStore {
    /// 连接会话总线并确认 Secret Service 可用。
    ///
    /// 失败原因按[模块文档](super)的错误表分类，不会 panic；调用方（通常是
    /// [`open_credential_store`](super::open_credential_store)）据此决定是否退回
    /// [`UnavailableCredentialStore`](super::UnavailableCredentialStore)。
    ///
    /// 构造时会做两次探测：`NameHasOwner`（服务是否在）与 `OpenSession`（服务是否响应）。
    /// 这样「装了客户端但没起密钥环」在启动阶段就能被发现，而不是等第一次连接数据库时
    /// 才暴露。
    pub fn connect(profile: &str) -> Result<Self> {
        validate_profile(profile)?;
        let conn = Connection::session().map_err(|error| classify_zbus_error(&error))?;
        let store = Self {
            conn,
            profile: profile.to_owned(),
            session: Mutex::new(None),
        };
        store.ensure_service()?;
        store.session()?;
        Ok(store)
    }

    /// 属性字典：`application` + `profile` + `reference`（ADR 0012）。
    fn attributes<'a>(&'a self, reference: &'a str) -> HashMap<&'a str, &'a str> {
        HashMap::from([
            ("application", APPLICATION_ID),
            ("profile", self.profile.as_str()),
            ("reference", reference),
        ])
    }

    /// 会话路径（懒加载并缓存）。
    fn session(&self) -> Result<OwnedObjectPath> {
        let mut guard = self.session.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(path) = guard.as_ref() {
            return Ok(path.clone());
        }
        let reply = self.call(
            SERVICE_PATH,
            SERVICE_IFACE,
            "OpenSession",
            &(ALGORITHM_PLAIN, Value::from("")),
        )?;
        let (_output, path): (OwnedValue, OwnedObjectPath) =
            reply.body().deserialize().map_err(|_| protocol_error())?;
        if is_none_path(&path) {
            return Err(precondition_error(
                "系统凭据服务未能建立会话（返回了提示对象），请先在桌面会话中解锁密钥环",
            ));
        }
        *guard = Some(path.clone());
        Ok(path)
    }

    /// 默认集合路径。
    fn collection(&self) -> Result<OwnedObjectPath> {
        let reply = self.call(SERVICE_PATH, SERVICE_IFACE, "ReadAlias", &(DEFAULT_ALIAS,))?;
        let (path,): (OwnedObjectPath,) =
            reply.body().deserialize().map_err(|_| protocol_error())?;
        if is_none_path(&path) {
            return Err(unavailable_error(
                "系统凭据服务没有默认集合（default alias），请先创建并解锁密钥环",
            ));
        }
        Ok(path)
    }

    /// 集合是否锁定。
    ///
    /// 不能只依赖 `SearchItems` 的返回值判断「不存在」：集合锁定时搜索可能看不到条目，
    /// 那会把「已锁定」误报成「不存在」。
    fn collection_locked(&self, collection: &OwnedObjectPath) -> Result<bool> {
        let reply = self.call(
            collection.as_str(),
            PROPERTIES_IFACE,
            "Get",
            &(COLLECTION_IFACE, "Locked"),
        )?;
        let (value,): (OwnedValue,) = reply.body().deserialize().map_err(|_| protocol_error())?;
        bool::try_from(value).map_err(|_| protocol_error())
    }

    /// 按属性检索条目，分别返回「已解锁」与「已锁定」的条目。
    fn search(
        &self,
        attributes: &HashMap<&str, &str>,
    ) -> Result<(Vec<OwnedObjectPath>, Vec<OwnedObjectPath>)> {
        let reply = self.call(SERVICE_PATH, SERVICE_IFACE, "SearchItems", attributes)?;
        reply.body().deserialize().map_err(|_| protocol_error())
    }

    /// 发送一次方法调用。
    ///
    /// 所有 D-Bus 错误在这里统一分类；任何情况下都不会把 zbus 的 `Display`/`Debug`
    /// 或 D-Bus 错误正文拼进错误对象（它们可能带实现细节甚至条目内容）。
    fn call<B>(&self, path: &str, iface: &str, method: &str, body: &B) -> Result<Message>
    where
        B: serde::ser::Serialize + DynamicType,
    {
        self.conn
            .call_method(Some(SERVICE), path, Some(iface), method, body)
            .map_err(|error| classify_zbus_error(&error))
    }

    /// 确认 `org.freedesktop.secrets` 有拥有者。
    fn ensure_service(&self) -> Result<()> {
        let reply = self
            .conn
            .call_method(
                Some(DBUS_SERVICE),
                DBUS_PATH,
                Some(DBUS_IFACE),
                "NameHasOwner",
                &(SERVICE,),
            )
            .map_err(|error| classify_zbus_error(&error))?;
        let (owned,): (bool,) = reply.body().deserialize().map_err(|_| protocol_error())?;
        if owned {
            Ok(())
        } else {
            Err(unavailable_error(
                "系统中没有运行系统凭据服务（org.freedesktop.secrets），请安装并启动密钥环",
            ))
        }
    }
}

impl CredentialStore for SecretServiceCredentialStore {
    fn get(&self, reference: &str) -> Result<Secret> {
        validate_reference(reference)?;
        let attributes = self.attributes(reference);
        let (unlocked, locked) = self.search(&attributes)?;
        // 属性唯一且写入用 replace=true，正常只有一个条目；若历史数据里有重复，
        // 取第一个已解锁条目即可，下次写入会把它们合并。
        if let Some(item) = unlocked.first() {
            let session = self.session()?;
            return self.read_secret(item, &session);
        }
        if locked.is_empty() {
            Err(not_found_error())
        } else {
            Err(precondition_error(
                "系统凭据库中的条目已锁定：请先在桌面会话中解锁密钥环后重试",
            ))
        }
    }

    fn set(&self, reference: &str, secret: &Secret) -> Result<()> {
        validate_reference(reference)?;
        let session = self.session()?;
        let collection = self.collection()?;
        if self.collection_locked(&collection)? {
            return Err(precondition_error(
                "系统凭据库已锁定：请先在桌面会话中解锁密钥环后重试，本次密码未保存",
            ));
        }

        let attributes = self.attributes(reference);
        let properties = HashMap::from([
            (PROP_LABEL, Value::from(ITEM_LABEL)),
            (PROP_ATTRIBUTES, Value::from(attributes)),
        ]);
        // 秘密结构 `(oayays)`：会话、参数、值、内容类型。
        let payload = (
            session,
            Vec::<u8>::new(),
            secret.expose().as_bytes(),
            CONTENT_TYPE,
        );
        // replace=true：同一属性的条目覆盖写，符合 ADR 0012 的「不做版本化」。
        let reply = self.call(
            collection.as_str(),
            COLLECTION_IFACE,
            "CreateItem",
            &(properties, payload, true),
        )?;
        let (item, prompt): (OwnedObjectPath, OwnedObjectPath) =
            reply.body().deserialize().map_err(|_| protocol_error())?;
        if is_none_path(&item) || !is_none_path(&prompt) {
            return Err(precondition_error(
                "系统凭据服务要求用户在桌面会话中确认本次写入，密码未保存；请解锁密钥环后重试",
            ));
        }
        Ok(())
    }

    fn delete(&self, reference: &str) -> Result<()> {
        validate_reference(reference)?;
        let attributes = self.attributes(reference);
        let (unlocked, locked) = self.search(&attributes)?;
        if unlocked.is_empty() && locked.is_empty() {
            // 幂等：引用不存在就是成功。但要先排除「集合锁定导致搜索看不到条目」，
            // 否则会把「删不掉」谎报成「已删除」。
            let collection = self.collection()?;
            if self.collection_locked(&collection)? {
                return Err(precondition_error(
                    "系统凭据库已锁定：无法确认该引用是否已删除，请先解锁后重试",
                ));
            }
            return Ok(());
        }
        if !locked.is_empty() {
            return Err(precondition_error(
                "系统凭据库中的条目已锁定：请先在桌面会话中解锁密钥环后重试",
            ));
        }
        for item in &unlocked {
            let reply = self.call(item.as_str(), ITEM_IFACE, "Delete", &())?;
            let (prompt,): (OwnedObjectPath,) =
                reply.body().deserialize().map_err(|_| protocol_error())?;
            if !is_none_path(&prompt) {
                return Err(precondition_error(
                    "删除凭据需要用户在桌面会话中确认，凭据未删除",
                ));
            }
        }
        Ok(())
    }

    fn implementation(&self) -> &'static str {
        "linux-secret-service"
    }
}

impl SecretServiceCredentialStore {
    /// 读取条目秘密并转换为 UTF-8 文本。
    fn read_secret(&self, item: &OwnedObjectPath, session: &OwnedObjectPath) -> Result<Secret> {
        let reply = self.call(item.as_str(), ITEM_IFACE, "GetSecret", &(session,))?;
        let (_session, _parameters, value, _content_type): (
            OwnedObjectPath,
            Vec<u8>,
            Vec<u8>,
            String,
        ) = reply.body().deserialize().map_err(|_| protocol_error())?;
        decode_secret(&value)
    }
}

/// `Secret` 是 UTF-8 文本；密钥环里可能有别的应用写入的二进制条目（或用户手工改坏的数据）。
/// 这种情况不能 `from_utf8_lossy` 糊过去——那会把错误密码交给驱动并得到一个莫名其妙的
/// 认证失败，必须如实报「内容损坏、请重新保存」。
fn decode_secret(bytes: &[u8]) -> Result<Secret> {
    match std::str::from_utf8(bytes) {
        Ok(value) => Ok(Secret::new(value)),
        Err(_) => Err(precondition_error(
            "系统凭据库中的凭据内容不是合法的 UTF-8 文本，请在应用内重新保存该连接的密码",
        )),
    }
}

fn validate_profile(profile: &str) -> Result<()> {
    let valid = !profile.is_empty()
        && profile.len() <= MAX_PROFILE_LEN
        && !profile.chars().any(char::is_control);
    if valid {
        Ok(())
    } else {
        Err(Error::invalid_argument(
            ErrorCode::CONFIG_INVALID,
            "用户档案名不合法：不能为空、不能超过 64 字节、不能包含控制字符",
        ))
    }
}

fn is_none_path(path: &OwnedObjectPath) -> bool {
    path.as_str() == NONE_PATH
}

fn unavailable_error(message: &str) -> Error {
    Error::new(
        ErrorCode::CREDENTIAL_UNAVAILABLE,
        ErrorCategory::Unavailable,
        message,
    )
}

fn precondition_error(message: &str) -> Error {
    Error::new(
        ErrorCode::CREDENTIAL_UNAVAILABLE,
        ErrorCategory::FailedPrecondition,
        message,
    )
}

/// 协议层故障：服务回了东西但看不懂。
///
/// 用 `Internal` 分类：这是实现/版本不匹配的 bug，重试不会好，但要按凭据不可用上报，
/// 让前端知道「连不上数据库是因为拿不到密码」。
fn protocol_error() -> Error {
    Error::new(
        ErrorCode::CREDENTIAL_UNAVAILABLE,
        ErrorCategory::Internal,
        "系统凭据服务返回了无法解析的响应",
    )
    .with_source(SecretServiceFault {
        kind: "protocol",
        name: None,
    })
}

fn classify_zbus_error(error: &zbus::Error) -> Error {
    if let zbus::Error::MethodError(name, detail, _reply) = error {
        return classify_method_error(name.as_str(), detail.as_deref());
    }
    unavailable_error("无法与系统凭据服务通信（会话总线不可用或服务未运行）")
        .with_source(SecretServiceFault::from_transport(error))
}

/// 按 D-Bus 错误名分类。
///
/// `detail` 只用于「存在与否」，**绝不**进入错误链：D-Bus 错误正文由服务实现决定，
/// 可能包含条目属性甚至被错误实现塞进去的秘密值；而 `Error::source` 会跟着 `Debug`
/// 和日志流出去。
fn classify_method_error(name: &str, _detail: Option<&str>) -> Error {
    let fault = |kind: &'static str| SecretServiceFault {
        kind,
        name: Some(name.to_owned()),
    };
    match name {
        "org.freedesktop.DBus.Error.AccessDenied"
        | "org.freedesktop.DBus.Error.AuthFailed"
        | "org.freedesktop.DBus.Error.InteractiveAuthorizationRequired" => Error::new(
            ErrorCode::CREDENTIAL_UNAVAILABLE,
            ErrorCategory::PermissionDenied,
            "没有访问系统凭据库的权限：请检查当前会话对密钥环的访问策略",
        )
        .with_source(fault("permission_denied")),
        "org.freedesktop.DBus.Error.ServiceUnknown"
        | "org.freedesktop.DBus.Error.NameHasNoOwner"
        | "org.freedesktop.DBus.Error.Disconnected"
        | "org.freedesktop.DBus.Error.NoReply"
        | "org.freedesktop.DBus.Error.Timeout"
        | "org.freedesktop.DBus.Error.NoServer"
        | "org.freedesktop.DBus.Error.Spawn.Failed"
        | "org.freedesktop.DBus.Error.Spawn.ChildExited" => {
            unavailable_error("系统凭据服务不可用（连接已断开或服务未运行）")
                .with_source(fault("transport"))
        }
        _ if name.ends_with(".IsLocked") => {
            unavailable_error("系统凭据库已锁定：请先在桌面会话中解锁密钥环后重试")
                .with_source(fault("locked"))
        }
        _ if name.ends_with(".NoSuchObject") || name.ends_with(".NoSuchItem") => {
            not_found_error().with_source(fault("no_such_object"))
        }
        _ if name.ends_with(".NoSession") => {
            unavailable_error("系统凭据服务会话已失效：请重试（后端会重新建立会话）")
                .with_source(fault("no_session"))
        }
        _ => Error::new(
            ErrorCode::CREDENTIAL_UNAVAILABLE,
            ErrorCategory::Internal,
            "系统凭据服务返回了未预期的错误",
        )
        .with_source(fault("method_error")),
    }
}

/// 供日志定位的 D-Bus 故障摘要。
///
/// 只保留短标签与错误名，显式丢弃正文——见 [`classify_method_error`] 的说明。
#[derive(Debug, Clone, PartialEq, Eq)]
struct SecretServiceFault {
    kind: &'static str,
    name: Option<String>,
}

impl SecretServiceFault {
    fn from_transport(error: &zbus::Error) -> Self {
        match error {
            zbus::Error::Address(_) => Self {
                kind: "address",
                name: None,
            },
            zbus::Error::InputOutput(_) => Self {
                kind: "io",
                name: None,
            },
            zbus::Error::Handshake(_) => Self {
                kind: "handshake",
                name: None,
            },
            zbus::Error::Unsupported => Self {
                kind: "unsupported",
                name: None,
            },
            other => Self {
                kind: "transport",
                // 只取错误枚举的「类别名」，不用 Debug/Display：那会带上内部细节。
                name: Some(short_variant_name(other)),
            },
        }
    }
}

/// 取 `zbus::Error` 变体的短标识，用于日志定位。
fn short_variant_name(error: &zbus::Error) -> String {
    let debug = format!("{error:?}");
    let head = debug
        .split_once('(')
        .map_or(debug.as_str(), |(head, _)| head);
    head.to_owned()
}

impl fmt::Display for SecretServiceFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.name {
            Some(name) => write!(f, "{} ({name})", self.kind),
            None => f.write_str(self.kind),
        }
    }
}

impl std::error::Error for SecretServiceFault {}

#[cfg(test)]
mod tests {
    use super::*;

    const REF: &str = "sqarvo:conn/3f9a1c4e-7b2d-4a6f-9c11-0d2b8e5a71f4";

    fn assert_code_and_category(error: &Error, code: ErrorCode, category: ErrorCategory) {
        assert_eq!(error.code(), code);
        assert_eq!(error.category(), category);
    }

    #[test]
    fn secret_debug_is_redacted_through_store_errors() {
        // 错误链与日志里都不能出现密码。
        let secret = Secret::new("hunter2");
        let error = precondition_error("写入失败").with_source(SecretServiceFault {
            kind: "x",
            name: None,
        });
        let rendered = format!("{error} {error:?} {secret:?}");
        assert!(!rendered.contains("hunter2"));
    }

    #[test]
    fn locked_keyring_is_unavailable() {
        let error = classify_method_error("org.freedesktop.Secret.Error.IsLocked", None);
        assert_code_and_category(
            &error,
            ErrorCode::CREDENTIAL_UNAVAILABLE,
            ErrorCategory::Unavailable,
        );
        assert!(error.category().is_retryable());
    }

    #[test]
    fn missing_item_is_not_found() {
        let error = classify_method_error("org.freedesktop.Secret.Error.NoSuchObject", None);
        assert_code_and_category(
            &error,
            ErrorCode::CREDENTIAL_NOT_FOUND,
            ErrorCategory::NotFound,
        );
    }

    #[test]
    fn permission_denied_keeps_credential_code() {
        let error = classify_method_error("org.freedesktop.DBus.Error.AccessDenied", None);
        assert_code_and_category(
            &error,
            ErrorCode::CREDENTIAL_UNAVAILABLE,
            ErrorCategory::PermissionDenied,
        );
    }

    #[test]
    fn service_unknown_is_unavailable() {
        let error = classify_method_error("org.freedesktop.DBus.Error.ServiceUnknown", None);
        assert_code_and_category(
            &error,
            ErrorCode::CREDENTIAL_UNAVAILABLE,
            ErrorCategory::Unavailable,
        );
    }

    #[test]
    fn unknown_method_error_is_internal_without_detail() {
        let error = classify_method_error(
            "org.freedesktop.Secret.Error.SomeFutureError",
            Some("password=hunter2"),
        );
        assert_code_and_category(
            &error,
            ErrorCode::CREDENTIAL_UNAVAILABLE,
            ErrorCategory::Internal,
        );
        // D-Bus 正文可能带秘密值，绝不能顺着 Display / Debug 流出去。
        assert!(!format!("{error} {error:?}").contains("hunter2"));
        assert_eq!(
            error.to_string(),
            "系统凭据服务返回了未预期的错误 [credential.unavailable]"
        );
    }

    #[test]
    fn dbus_detail_is_never_forwarded() {
        for (name, detail) in [
            ("org.freedesktop.Secret.Error.IsLocked", "secret=hunter2"),
            (
                "org.freedesktop.DBus.Error.AccessDenied",
                "owned-by=hunter2@db",
            ),
            ("org.freedesktop.Secret.Error.NoSuchObject", "/hunter2"),
        ] {
            let error = classify_method_error(name, Some(detail));
            let rendered = format!("{error} {error:?}");
            assert!(!rendered.contains("hunter2"), "{rendered}");
        }

        // 证据：D-Bus 错误正文里塞了假密码，错误链输出里只剩分类文案与错误名。
        let error = classify_method_error(
            "org.freedesktop.Secret.Error.IsLocked",
            Some("password=hunter2"),
        );
        eprintln!("[redaction] Display = {error}");
        eprintln!("[redaction] Debug    = {error:?}");
        eprintln!(
            "[redaction] source   = {}",
            std::error::Error::source(&error)
                .map_or_else(|| "<none>".to_owned(), ToString::to_string)
        );
        assert!(!format!("{error} {error:?}").contains("hunter2"));
    }

    #[test]
    fn non_utf8_secret_is_rejected() {
        let error = decode_secret(&[0xff, 0xfe, 0x00]).unwrap_err();
        assert_code_and_category(
            &error,
            ErrorCode::CREDENTIAL_UNAVAILABLE,
            ErrorCategory::FailedPrecondition,
        );
        // 错误里不能回显原始字节。
        assert!(!error.to_string().contains('\u{fffd}'));
    }

    #[test]
    fn utf8_secret_roundtrips() {
        assert_eq!(
            decode_secret("pässwörd".as_bytes()).unwrap().expose(),
            "pässwörd"
        );
        assert_eq!(decode_secret(b"").unwrap().expose(), "");
    }

    #[test]
    fn item_label_and_attributes_follow_adr_0012() {
        let attributes = HashMap::from([
            ("application", APPLICATION_ID),
            ("profile", "default"),
            ("reference", REF),
        ]);
        let properties = HashMap::from([
            (PROP_LABEL, Value::from(ITEM_LABEL)),
            (PROP_ATTRIBUTES, Value::from(attributes)),
        ]);

        assert_eq!(
            properties
                .get(PROP_LABEL)
                .and_then(|value| <&str>::try_from(value).ok()),
            Some(ITEM_LABEL)
        );
        // label 是固定常量：不含主机、用户名、引用。
        assert_eq!(ITEM_LABEL, "Sqarvo");
        assert!(!ITEM_LABEL.contains('@'));
        assert!(!ITEM_LABEL.contains(REF));

        let mut keys: Vec<&str> = match properties.get(PROP_ATTRIBUTES) {
            Some(Value::Dict(dict)) => dict
                .iter()
                .map(|(key, _value)| <&str>::try_from(key).expect("属性键是字符串"))
                .collect(),
            other => panic!("属性应是 a{{ss}} 字典：{other:?}"),
        };
        keys.sort_unstable();
        assert_eq!(keys, ["application", "profile", "reference"]);
    }

    #[test]
    fn profile_is_validated() {
        assert!(validate_profile("default").is_ok());
        for profile in ["", "with\nnewline", &"x".repeat(MAX_PROFILE_LEN + 1)] {
            let error = validate_profile(profile).unwrap_err();
            assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);
        }
    }

    #[test]
    fn connect_rejects_invalid_profile_without_touching_the_bus() {
        let error = SecretServiceCredentialStore::connect("").unwrap_err();
        assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);
    }
}
