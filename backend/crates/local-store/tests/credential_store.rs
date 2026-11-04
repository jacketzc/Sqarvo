//! 凭据存储语义测试。
//!
//! 内存替身与真实 Secret Service 必须表现一致：引用不存在、覆盖写、删除幂等。
//! 真实密钥环用例由 `SQARVO_SECRET_SERVICE_TEST=1` 门控——不设置时打印跳过原因，
//! 因为「零测试」和「跳过测试」必须能区分开。

use sqarvo_common::{ErrorCategory, ErrorCode};
use sqarvo_local_store::{CredentialStore, MemoryCredentialStore, Secret};
use uuid::Uuid;

/// 引用不存在时的契约。
fn assert_not_found(store: &dyn CredentialStore, reference: &str) {
    let error = store.get(reference).unwrap_err();
    assert_eq!(error.code(), ErrorCode::CREDENTIAL_NOT_FOUND);
    assert_eq!(error.category(), ErrorCategory::NotFound);
}

/// 两个实现共用的语义：写入 → 读取 → 覆盖 → 删除 → 幂等删除。
fn assert_store_semantics(store: &dyn CredentialStore, reference: &str) {
    assert_not_found(store, reference);

    store.set(reference, &Secret::new("first")).unwrap();
    assert_eq!(store.get(reference).unwrap().expose(), "first");

    // 覆盖写：同一引用只保留最新值（ADR 0012 不做版本化）。
    store.set(reference, &Secret::new("second")).unwrap();
    assert_eq!(store.get(reference).unwrap().expose(), "second");

    // 超长 secret（64 KiB）：不能截断、不能因为长度报错。
    let long = "p".repeat(64 * 1024);
    store.set(reference, &Secret::new(long.clone())).unwrap();
    assert_eq!(store.get(reference).unwrap().expose(), long);

    store.delete(reference).unwrap();
    assert_not_found(store, reference);
    // 幂等：重复删除不报错。
    store.delete(reference).unwrap();
}

#[test]
fn memory_store_semantics() {
    let store = MemoryCredentialStore::new();
    let reference = format!("sqarvo:conn/{}", Uuid::new_v4());
    assert_store_semantics(&store, &reference);
}

#[test]
fn memory_store_rejects_malformed_reference() {
    let store = MemoryCredentialStore::new();
    let error = store
        .set("sqarvo:conn/db.example.com:3306", &Secret::new("x"))
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);
    assert_eq!(error.category(), ErrorCategory::InvalidArgument);
}

/// 工厂必须永不 panic：环境可用时得到 Secret Service 实现（读到 `not_found`），
/// 不可用时得到 `UnavailableCredentialStore`（`credential.unavailable`）。
///
/// 用 `DBUS_SESSION_BUS_ADDRESS=unix:path=/nonexistent` 运行可以把不可用分支走一遍：
/// `DBUS_SESSION_BUS_ADDRESS=unix:path=/nonexistent cargo test -p sqarvo-local-store`
#[test]
fn factory_never_panics_and_reports_a_typed_error() {
    let store = sqarvo_local_store::open_default_credential_store();
    let reference = format!("sqarvo:conn/{}", Uuid::new_v4());
    let error = store.get(&reference).unwrap_err();
    assert!(
        matches!(
            error.code(),
            ErrorCode::CREDENTIAL_NOT_FOUND | ErrorCode::CREDENTIAL_UNAVAILABLE
        ),
        "未预期的错误码：{}",
        error.code()
    );
    eprintln!(
        "[factory] 装配结果：{}（{} / {}）",
        error.code(),
        error.category(),
        error.message()
    );
}

/// 真实 Secret Service 用例。
///
/// 需要：桌面会话（`DBUS_SESSION_BUS_ADDRESS` 可用）且 `org.freedesktop.secrets`
/// 提供服务（GNOME 上是 gnome-keyring）。运行方式：
///
/// ```text
/// SQARVO_SECRET_SERVICE_TEST=1 cargo test -p sqarvo-local-store -- --nocapture
/// ```
#[cfg(target_os = "linux")]
#[test]
fn secret_service_semantics() {
    if std::env::var("SQARVO_SECRET_SERVICE_TEST").as_deref() != Ok("1") {
        eprintln!(
            "跳过 secret_service_semantics：未设置 SQARVO_SECRET_SERVICE_TEST=1；\
             该用例会真实读写当前桌面会话的密钥环（org.freedesktop.secrets），\
             需要 D-Bus 会话总线与已解锁的默认集合"
        );
        return;
    }

    let store = sqarvo_local_store::SecretServiceCredentialStore::connect("default")
        .unwrap_or_else(|error| {
            panic!(
                "连接 Secret Service 失败（{} / {}）：{error}\n\
                 请确认 DBUS_SESSION_BUS_ADDRESS 指向桌面会话总线，且密钥环已启动并解锁",
                error.code(),
                error.category()
            )
        });

    let reference = format!("sqarvo:conn/{}", Uuid::new_v4());
    eprintln!("[live] 凭据引用 = {reference}");
    assert_store_semantics(&store, &reference);
    eprintln!("[live] 写入 / 覆盖写 / 读取 / 64 KiB 长密码 / 删除 / 幂等删除 均符合契约");

    // 条目属性必须严格按 ADR 0012：只有 application / profile / reference，
    // label 固定为 "Sqarvo"，不含主机、用户名或引用。
    let inspected = format!("sqarvo:conn/{}", Uuid::new_v4());
    store.set(&inspected, &Secret::new("inspect")).unwrap();
    assert_adr_0012_item_properties(&inspected);
    eprintln!("[live] 条目属性 = application/profile/reference，label = \"Sqarvo\"（已验证）");
    store.delete(&inspected).unwrap();
    assert_not_found(&store, &inspected);
    eprintln!("[live] 清理完成：密钥环中没有残留的 Sqarvo 测试条目");
}

/// 直接走 D-Bus 校验写入的条目属性与 label。
#[cfg(target_os = "linux")]
fn assert_adr_0012_item_properties(reference: &str) {
    use std::collections::HashMap;
    use zbus::blocking::Connection;
    use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

    let conn = Connection::session().expect("会话总线");
    let attributes: HashMap<&str, &str> = HashMap::from([
        (
            "application",
            sqarvo_local_store::credentials::APPLICATION_ID,
        ),
        ("profile", sqarvo_local_store::credentials::DEFAULT_PROFILE),
        ("reference", reference),
    ]);
    let reply = conn
        .call_method(
            Some("org.freedesktop.secrets"),
            "/org/freedesktop/secrets",
            Some("org.freedesktop.Secret.Service"),
            "SearchItems",
            &attributes,
        )
        .expect("SearchItems");
    let (unlocked, locked): (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) =
        reply.body().deserialize().expect("SearchItems 返回值");
    assert!(locked.is_empty(), "锁定的条目不应参与本用例：{locked:?}");
    assert_eq!(unlocked.len(), 1, "同一引用应只对应一个条目");

    let reply = conn
        .call_method(
            Some("org.freedesktop.secrets"),
            unlocked[0].as_str(),
            Some("org.freedesktop.DBus.Properties"),
            "GetAll",
            &("org.freedesktop.Secret.Item",),
        )
        .expect("Properties.GetAll");
    let properties: HashMap<String, OwnedValue> =
        reply.body().deserialize().expect("GetAll 返回值");

    let label = properties
        .get("Label")
        .and_then(|value| <&str>::try_from(value).ok())
        .expect("条目应有 Label");
    assert_eq!(label, "Sqarvo");
    assert!(!label.contains('@'));
    assert!(!label.contains(reference));

    let attributes = properties.get("Attributes").expect("条目应有 Attributes");
    let Value::Dict(dict) = &**attributes else {
        panic!("Attributes 应是 a{{ss}}：{attributes:?}");
    };
    let mut keys: Vec<&str> = dict
        .iter()
        .map(|(key, _value)| <&str>::try_from(key).expect("属性键是字符串"))
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["application", "profile", "reference"]);
}
