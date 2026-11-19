//! 共享档案持久化、拒绝非法文档和真实跨进程写竞争。

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sqarvo_common::{ErrorCategory, ErrorCode, Result};
use sqarvo_driver_api::{ConnectionParams, DriverId, TlsMode};
use sqarvo_local_store::file_lock::{LockMode, try_lock};
use sqarvo_local_store::{AppPaths, ConnectionProfile, McpAccessPolicy, ProfileStore};
use tempfile::TempDir;
use uuid::Uuid;

fn store_at(dir: &Path) -> ProfileStore {
    ProfileStore::new(&AppPaths::from_roots(
        dir.join("config"),
        dir.join("data"),
        dir.join("run"),
    ))
}

fn profile(name: &str, port: u16) -> ConnectionProfile {
    ConnectionProfile {
        profile_id: Uuid::nil(),
        revision: 0,
        name: name.to_owned(),
        params: ConnectionParams {
            driver_id: DriverId::new("mariadb").unwrap(),
            host: "localhost".to_owned(),
            port,
            database: Some("example".to_owned()),
            username: Some("test".to_owned()),
            credential_ref: Some(format!("sqarvo:conn/{}", Uuid::new_v4())),
            tls: TlsMode::VerifyFull,
            tls_server_name: Some("db.example".to_owned()),
            properties: BTreeMap::new(),
        },
        file_path: String::new(),
        display_name: None,
        last_used: None,
    }
}

fn document_path(dir: &Path) -> std::path::PathBuf {
    dir.join("config/connections.json")
}

#[test]
fn stores_flattened_driver_params_and_owner_only_files() {
    let root = TempDir::new().unwrap();
    let store = store_at(root.path());
    let saved = store.save(profile("Example", 3306), None).unwrap();
    assert_eq!(saved.revision, 1);
    assert_eq!(saved.profile_id.get_version_num(), 4);
    assert_eq!(store.get(saved.profile_id).unwrap(), saved);
    assert_eq!(store.snapshot().unwrap().revision, 1);

    let file = document_path(root.path());
    let data: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    assert_eq!(data["profiles"][0]["tls"], "verify-full");
    assert_eq!(data["profiles"][0]["tls_server_name"], "db.example");
    assert_eq!(data["profiles"][0]["driver_id"], "mariadb");
    assert!(data["profiles"][0].get("params").is_none());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        for (path, expected) in [
            (file.clone(), 0o600),
            (file.with_extension("json.lock"), 0o600),
            (file.parent().unwrap().to_owned(), 0o700),
        ] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                expected
            );
        }
    }
}

#[test]
fn defaults_missing_policy_fields_and_rejects_malformed_documents_without_recovery() {
    let root = TempDir::new().unwrap();
    let store = store_at(root.path());
    let file = document_path(root.path());
    assert_eq!(store.get_mcp_access().unwrap(), McpAccessPolicy::default());
    let saved = store.save(profile("database", 3306), None).unwrap();
    let valid = fs::read(&file).unwrap();
    let mut doc: Value = serde_json::from_slice(&valid).unwrap();
    doc.as_object_mut().unwrap().remove("mcp");
    write_json(&file, &doc);
    assert_eq!(store.get_mcp_access().unwrap(), McpAccessPolicy::default());

    doc["mcp"] = json!({"enabled":true});
    write_json(&file, &doc);
    assert!(store.get_mcp_access().unwrap().read_only);
    doc["mcp"] = json!({"read_only":true});
    write_json(&file, &doc);
    assert!(!store.get_mcp_access().unwrap().enabled);

    for (path, value) in [
        ("/schema_version", json!(8)),
        ("/mcp", json!({"enabled":"true"})),
        ("/mcp", json!({"enabled":false, "read_only":false})),
        ("/mcp", json!({"enabled":true, "unknown":1})),
        ("/profiles/0/name", json!("")),
        ("/profiles/0/driver_id", json!("INVALID")),
        ("/profiles/0/tls", json!("invalid")),
        ("/profiles/0/credential_ref", json!("private-input")),
        (
            "/profiles/0/properties",
            json!({"password":"private-input"}),
        ),
        ("/profiles/0/profile_id", json!(Uuid::nil())),
    ] {
        let mut invalid: Value = serde_json::from_slice(&valid).unwrap();
        *invalid.pointer_mut(path).unwrap() = value;
        write_json(&file, &invalid);
        assert_failed_closed(&store, &file);
    }

    let mut invalid: Value = serde_json::from_slice(&valid).unwrap();
    invalid["profiles"][0]["password"] = json!("private-input");
    write_json(&file, &invalid);
    assert_failed_closed(&store, &file);
    fs::write(&file, b"{partial JSON").unwrap();
    assert_failed_closed(&store, &file);
    fs::write(&file, valid).unwrap();
    assert_eq!(store.get(saved.profile_id).unwrap(), saved);
}

fn write_json(path: &Path, doc: &Value) {
    fs::write(path, serde_json::to_vec(doc).unwrap()).unwrap();
}

#[test]
fn sqlite_legacy_path_is_completed_and_conflicting_documents_fail_closed() {
    let root = TempDir::new().unwrap();
    let store = store_at(root.path());
    let mut sqlite = profile("sqlite fixture", 0);
    sqlite.params.driver_id = DriverId::new("sqlite").unwrap();
    sqlite.params.host = "/tmp/case-sensitive.sqlite".to_owned();
    let saved = store.save(sqlite, None).unwrap();
    let file = document_path(root.path());

    let valid: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    for field in ["file_path", "host"] {
        let mut legacy = valid.clone();
        legacy["profiles"][0][field] = json!("");
        write_json(&file, &legacy);
        let before = fs::read(&file).unwrap();
        assert_eq!(store.get(saved.profile_id).unwrap(), saved);
        assert_eq!(fs::read(&file).unwrap(), before);
    }

    let mut invalid = valid;
    invalid["profiles"][0]["file_path"] = json!("/tmp/private-input");
    write_json(&file, &invalid);
    assert_failed_closed(&store, &file);
}

fn assert_failed_closed(store: &ProfileStore, path: &Path) {
    let before = fs::read(path).unwrap();
    let error = store.snapshot().unwrap_err();
    assert_eq!(error.code(), ErrorCode::PROFILE_INVALID);
    assert!(!format!("{error:?}").contains("private-input"));
    assert_eq!(
        store
            .set_mcp_access(McpAccessPolicy::default())
            .unwrap_err()
            .code(),
        ErrorCode::PROFILE_INVALID
    );
    assert_eq!(
        store.run_once_migration().unwrap_err().code(),
        ErrorCode::PROFILE_INVALID
    );
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn mcp_policy_has_no_effect_on_profile_revision_or_fields() {
    let root = TempDir::new().unwrap();
    let store = store_at(root.path());
    let saved = store.save(profile("Example", 3306), None).unwrap();
    let before = store.snapshot().unwrap();
    let (policy, revision) = store
        .update_mcp_access(|old| {
            assert_eq!(old, McpAccessPolicy::default());
            Ok(McpAccessPolicy {
                enabled: true,
                read_only: false,
            })
        })
        .unwrap();
    let after = store.snapshot().unwrap();
    assert_eq!(after.revision, before.revision + 1);
    assert_eq!(after.revision, revision);
    assert_eq!(after.mcp, policy);
    assert_eq!(after.profiles, vec![saved]);
    assert_eq!(before.migration_state, after.migration_state);
}

#[test]
fn identity_and_name_resolution_use_expected_revision_and_distinct_keys() {
    let root = TempDir::new().unwrap();
    let store = store_at(root.path());
    let saved = store.save(profile("shared name", 3306), None).unwrap();
    let mut duplicate = saved.clone();
    duplicate.profile_id = Uuid::nil();
    duplicate.params.host = "LOCALHOST".to_owned();
    assert_eq!(
        store.save(duplicate.clone(), None).unwrap_err().code(),
        ErrorCode::PROFILE_REVISION_CONFLICT
    );
    let updated = store.save(duplicate, Some(1)).unwrap();
    assert_eq!(updated.profile_id, saved.profile_id);
    assert_eq!(updated.revision, 2);
    assert_eq!(
        store.delete(updated.profile_id, 1).unwrap_err().code(),
        ErrorCode::PROFILE_REVISION_CONFLICT
    );
    let mut other = profile("shared name", 3307);
    other
        .params
        .credential_ref
        .clone_from(&saved.params.credential_ref);
    store.save(other, None).unwrap();
    let doc = store.snapshot().unwrap();
    assert_eq!(
        doc.resolve("shared name").unwrap_err().code(),
        ErrorCode::PROFILE_AMBIGUOUS_NAME
    );
    assert_eq!(
        doc.resolve(&saved.profile_id.to_string()).unwrap(),
        &updated
    );
    store.delete(saved.profile_id, 2).unwrap();
    assert_eq!(
        store.get(saved.profile_id).unwrap_err().category(),
        ErrorCategory::NotFound
    );
    assert_eq!(store.list().unwrap().len(), 1);
}

#[test]
fn startup_cleans_old_temporaries_without_replacing_lock_file() {
    let root = TempDir::new().unwrap();
    let store = store_at(root.path());
    store.snapshot().unwrap();
    let file = document_path(root.path());
    let temporary = file.with_extension("json.tmp-stale");
    let lock_path = file.with_extension("json.lock");
    fs::write(&temporary, "partial").unwrap();
    fs::write(&lock_path, "lock marker").unwrap();
    store.run_once_migration().unwrap();
    assert!(!temporary.exists());
    assert_eq!(fs::read_to_string(lock_path).unwrap(), "lock marker");
}

#[test]
fn shared_locks_coexist_and_exclusive_contention_has_bounded_timeout() {
    let root = TempDir::new().unwrap();
    let store = store_at(root.path());
    store.snapshot().unwrap();
    let lock_path = document_path(root.path()).with_extension("json.lock");
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock_path)
        .unwrap();
    assert!(try_lock(&lock_file, LockMode::Shared).unwrap());
    assert_eq!(store.list().unwrap(), []);
    let started = Instant::now();
    let error = store.save(profile("timed out", 3306), None).unwrap_err();
    assert_eq!(error.code(), ErrorCode::PROFILE_LOCKED);
    assert_eq!(error.category(), ErrorCategory::Timeout);
    assert!(started.elapsed() < Duration::from_secs(5));
    drop(lock_file);
    assert!(store.save(profile("after release", 3306), None).is_ok());
}

const CHILD_ROOT: &str = "SQARVO_PROFILE_STORE_TEST_ROOT";
const CHILD_ROLE: &str = "SQARVO_PROFILE_STORE_TEST_ROLE";
const CHILD_ID: &str = "SQARVO_PROFILE_STORE_TEST_ID";

#[test]
fn subprocess_writer() {
    let Some(root) = std::env::var_os(CHILD_ROOT) else {
        return;
    };
    let root = Path::new(&root);
    let role = std::env::var(CHILD_ROLE).unwrap();
    let store = store_at(root);
    if role == "lock-holder" {
        store.snapshot().unwrap();
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(document_path(root).with_extension("json.lock"))
            .unwrap();
        assert!(try_lock(&lock, LockMode::Exclusive).unwrap());
        fs::write(root.join(&role), b"locked").unwrap();
        wait_for_files(root, &["release-lock"]);
        drop(lock);
        return;
    }
    if role == "policy" {
        fs::write(root.join(&role), b"ready").unwrap();
        wait_for_files(root, &["go"]);
        store
            .set_mcp_access(McpAccessPolicy {
                enabled: true,
                read_only: false,
            })
            .unwrap();
        return;
    }
    let mut connection = profile(&role, if role == "first" { 3306 } else { 3307 });
    let mut expected = None;
    if let Ok(id) = std::env::var(CHILD_ID) {
        connection = store.get(id.parse().unwrap()).unwrap();
        connection.name.clone_from(&role);
        expected = Some(connection.revision);
    }
    fs::write(root.join(&role), b"ready").unwrap();
    wait_for_files(root, &["go"]);
    let result = store.save(connection, expected);
    fs::write(root.join(format!("{role}-result")), result_name(result)).unwrap();
}

fn result_name(result: Result<ConnectionProfile>) -> &'static str {
    match result {
        Ok(_) => "ok",
        Err(error) if error.code() == ErrorCode::PROFILE_REVISION_CONFLICT => "conflict",
        Err(error) => panic!("意外错误：{error:?}"),
    }
}

fn wait_for_files(root: &Path, files: &[&str]) {
    let started = Instant::now();
    while !files.iter().all(|file| root.join(file).exists()) {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "等待子进程超时"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn spawn_writer(root: &Path, role: &str, id: Option<Uuid>) -> Child {
    let mut command = Command::new(std::env::current_exe().unwrap());
    configure_writer(&mut command, root, role, id);
    command.spawn().unwrap()
}

fn configure_writer(command: &mut Command, root: &Path, role: &str, id: Option<Uuid>) {
    command
        .args(["--exact", "subprocess_writer"])
        .env(CHILD_ROOT, root)
        .env(CHILD_ROLE, role)
        .env_remove(CHILD_ID)
        .stdout(Stdio::null());
    if let Some(id) = id {
        command.env(CHILD_ID, id.to_string());
    }
}

fn run_writers(root: &Path, id: Option<Uuid>) -> [String; 2] {
    let mut first = spawn_writer(root, "first", id);
    let mut second = spawn_writer(root, "second", id);
    wait_for_files(root, &["first", "second"]);
    fs::write(root.join("go"), b"write").unwrap();
    assert!(first.wait().unwrap().success());
    assert!(second.wait().unwrap().success());
    [
        fs::read_to_string(root.join("first-result")).unwrap(),
        fs::read_to_string(root.join("second-result")).unwrap(),
    ]
}

#[test]
fn two_processes_create_distinct_profiles_without_losing_updates() {
    let root = TempDir::new().unwrap();
    let store = store_at(root.path());
    assert_eq!(run_writers(root.path(), None), ["ok", "ok"]);
    let doc = store.snapshot().unwrap();
    assert_eq!(doc.revision, 2);
    assert_eq!(doc.profiles.len(), 2);
    assert!(doc.profiles.iter().any(|profile| profile.name == "first"));
    assert!(doc.profiles.iter().any(|profile| profile.name == "second"));
}

#[test]
fn two_processes_edit_different_profiles_without_losing_updates() {
    let root = TempDir::new().unwrap();
    let store = store_at(root.path());
    let first = store.save(profile("before A", 3306), None).unwrap();
    let second = store.save(profile("before B", 3307), None).unwrap();
    let mut first_writer = spawn_writer(root.path(), "first", Some(first.profile_id));
    let mut second_writer = spawn_writer(root.path(), "second", Some(second.profile_id));
    wait_for_files(root.path(), &["first", "second"]);
    fs::write(root.path().join("go"), b"write").unwrap();
    assert!(first_writer.wait().unwrap().success());
    assert!(second_writer.wait().unwrap().success());
    for (role, saved) in [("first", first), ("second", second)] {
        let updated = store.get(saved.profile_id).unwrap();
        assert_eq!(updated.name, role);
        assert_eq!(updated.revision, saved.revision + 1);
    }
    assert_eq!(store.snapshot().unwrap().revision, 4);
}

#[test]
fn one_of_two_processes_editing_the_same_revision_gets_conflict() {
    let root = TempDir::new().unwrap();
    let store = store_at(root.path());
    let saved = store.save(profile("original", 3306), None).unwrap();
    let mut outcomes = run_writers(root.path(), Some(saved.profile_id));
    outcomes.sort();
    assert_eq!(outcomes, ["conflict", "ok"]);
    let doc = store.snapshot().unwrap();
    assert_eq!(doc.revision, 2);
    assert_eq!(doc.profiles[0].revision, 2);
    assert_eq!(doc.profiles.len(), 1);
}

#[test]
fn two_threads_preserve_distinct_profile_updates() {
    let root = TempDir::new().unwrap();
    let store = store_at(root.path());
    let ready = std::sync::Barrier::new(2);
    thread::scope(|scope| {
        for (name, port) in [("first", 3306), ("second", 3307)] {
            let store = &store;
            let ready = &ready;
            scope.spawn(move || {
                ready.wait();
                store.save(profile(name, port), None).unwrap();
            });
        }
    });
    let doc = store.snapshot().unwrap();
    assert_eq!(doc.revision, 2);
    assert_eq!(doc.profiles.len(), 2);
}

#[test]
fn profile_and_policy_updates_in_separate_processes_do_not_overwrite_each_other() {
    let root = TempDir::new().unwrap();
    let mut profile = spawn_writer(root.path(), "first", None);
    let mut policy = spawn_writer(root.path(), "policy", None);
    wait_for_files(root.path(), &["first", "policy"]);
    fs::write(root.path().join("go"), b"write").unwrap();
    assert!(profile.wait().unwrap().success());
    assert!(policy.wait().unwrap().success());
    let doc = store_at(root.path()).snapshot().unwrap();
    assert_eq!(doc.revision, 2);
    assert_eq!(doc.profiles.len(), 1);
    assert_eq!(doc.profiles[0].name, "first");
    assert_eq!(
        doc.mcp,
        McpAccessPolicy {
            enabled: true,
            read_only: false
        }
    );
}

#[test]
fn killed_lock_holder_releases_the_lock_without_replacing_the_document() {
    let root = TempDir::new().unwrap();
    let store = store_at(root.path());
    let saved = store.save(profile("original", 3307), None).unwrap();
    let mut holder = spawn_writer(root.path(), "lock-holder", None);
    wait_for_files(root.path(), &["lock-holder"]);
    let mut writer = spawn_writer(root.path(), "first", None);
    wait_for_files(root.path(), &["first"]);
    fs::write(root.path().join("go"), b"write").unwrap();
    let original = fs::read(document_path(root.path())).unwrap();
    thread::sleep(Duration::from_millis(200));
    let waited = writer.try_wait().unwrap().is_none();
    let unchanged = fs::read(document_path(root.path())).unwrap() == original;
    holder.kill().unwrap();
    assert!(!holder.wait().unwrap().success());
    assert!(writer.wait().unwrap().success());
    assert!(waited);
    assert!(unchanged);
    assert_eq!(store.get(saved.profile_id).unwrap(), saved);
    assert_eq!(
        fs::read_to_string(root.path().join("first-result")).unwrap(),
        "ok"
    );
    assert_eq!(store.snapshot().unwrap().revision, 2);
}

#[test]
#[cfg(target_os = "linux")]
fn write_interruption_preserves_previous_document_and_leaves_recoverable_temp() {
    use std::os::unix::process::ExitStatusExt;

    let root = TempDir::new().unwrap();
    let store = store_at(root.path());
    let saved = store.save(profile(&"x".repeat(4096), 3307), None).unwrap();
    let original = fs::read(document_path(root.path())).unwrap();
    let mut command = Command::new("sh");
    // POSIX ulimit units vary by shell. A single block is smaller than the existing
    // document, so an unrelated profile save hits the limit before rename.
    command.args(["-c", "ulimit -f 1 && exec \"$@\"", "profile-write"]);
    command.arg(std::env::current_exe().unwrap());
    configure_writer(&mut command, root.path(), "first", None);
    let mut writer = command.spawn().unwrap();
    wait_for_files(root.path(), &["first"]);
    fs::write(root.path().join("go"), b"write").unwrap();
    assert_eq!(writer.wait().unwrap().signal(), Some(libc::SIGXFSZ));
    assert_eq!(fs::read(document_path(root.path())).unwrap(), original);
    let abandoned = || {
        fs::read_dir(root.path().join("config"))
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("connections.json.tmp-")
            })
            .count()
    };
    assert_eq!(abandoned(), 1);
    store.run_once_migration().unwrap();
    assert_eq!(abandoned(), 0);
    assert_eq!(store.get(saved.profile_id).unwrap(), saved);
}
