//! 驱动 → 连接字段集：一张描述表管所有「哪个驱动需要哪些字段」的差异。
//!
//! # 为什么集中在这里
//!
//! 多驱动对话框最容易烂掉的地方，是把
//! `if driver_id == "sqlite" { … } else { … }` 散落到布局、校验、`params()`、
//! 会话命名、配置持久化里。每加一个驱动就要改五处，漏一处就是「界面允许填主机，
//! 后端报错说 SQLite 不支持端口」这种自相矛盾。
//!
//! 因此这里用**一张表**（[`fields_for`]）描述「某个驱动族显示哪些字段」，
//! 校验与草稿组装（[`build_draft`]）也按同一张表分支。界面层只做三件事：
//! 按表显示/隐藏控件、把控件内容读进 [`FormInput`]、把错误文案写到状态行。
//!
//! # 驱动清单从哪来
//!
//! 驱动 ID 的**清单**来自 `SystemService.ListDrivers`（[`crate::backend::client::DriverSummary`]）。
//! 本模块只知道「哪几个 ID 的字段集是什么」：后端多注册一个前端不认识的驱动时，
//! 下拉里会出现它，但表单如实说明「此版本前端没有该驱动的连接表单」，而不是
//! 猜一套字段、让用户在提交后才看到后端报错。
//!
//! 本模块不依赖 GTK，可单测。

use std::path::Path;

use crate::backend::connection::{ConnectionDraft, DriverFamily, TlsMode};

/// 一个驱动族需要显示的字段集合。
///
/// 字段名对应界面上的控件行；`true` 表示显示且参与校验，`false` 表示不显示、
/// 也不参与 `ConnectionParams`（不是「显示了但不填」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldSet {
    /// 主机。
    pub host: bool,
    /// 端口。
    pub port: bool,
    /// 数据库名。
    pub database: bool,
    /// 用户名。
    pub username: bool,
    /// TLS 模式。
    pub tls: bool,
    /// 密码（需要系统凭据库）。
    pub password: bool,
    /// 数据库文件路径。
    pub file_path: bool,
    /// 只读开关。
    pub read_only: bool,
}

impl FieldSet {
    /// 服务器型驱动的字段集：主机 / 端口 / 库 / 用户 / TLS / 密码。
    pub const SERVER: Self = Self {
        host: true,
        port: true,
        database: true,
        username: true,
        tls: true,
        password: true,
        file_path: false,
        read_only: false,
    };

    /// 文件型驱动的字段集：只有文件路径与只读开关。
    pub const FILE: Self = Self {
        host: false,
        port: false,
        database: false,
        username: false,
        tls: false,
        password: false,
        file_path: true,
        read_only: true,
    };

    /// 没有任何字段（前端不认识该驱动时用）。
    pub const NONE: Self = Self {
        host: false,
        port: false,
        database: false,
        username: false,
        tls: false,
        password: false,
        file_path: false,
        read_only: false,
    };
}

/// 驱动族 → 字段集（描述表本体）。
#[must_use]
pub fn fields_for(family: DriverFamily) -> FieldSet {
    match family {
        DriverFamily::MariaDb => FieldSet::SERVER,
        DriverFamily::Sqlite => FieldSet::FILE,
    }
}

/// 驱动 ID → 字段集；前端不认识的驱动返回 `None`。
#[must_use]
pub fn fields_for_driver(driver_id: &str) -> Option<FieldSet> {
    DriverFamily::for_id(driver_id).map(fields_for)
}

/// SQLite 的「内存数据库」路径：不需要文件存在，也不需要只读。
pub const SQLITE_MEMORY_PATH: &str = ":memory:";

/// 从控件读出的原始输入（纯数据，便于单测）。
///
/// 全部按字符串保存：端口由本模块解析，空串保持「用户没填」的语义，
/// 不在这里做任何默认值填充——默认值属于界面（新建时的初值）。
#[derive(Debug, Clone, Default)]
pub struct FormInput {
    /// 下拉里选中的驱动 ID。
    pub driver_id: String,
    /// 主机。
    pub host: String,
    /// 端口（文本，便于校验「不是数字」）。
    pub port: String,
    /// 数据库名。
    pub database: String,
    /// 用户名。
    pub username: String,
    /// 数据库文件路径。
    pub file_path: String,
    /// 是否以只读方式打开。
    pub read_only: bool,
    /// TLS 模式。
    pub tls_mode: TlsMode,
    /// 已保存的凭据引用（改密码时为新值，不改时复用）。
    pub credential_ref: Option<String>,
    /// 本次输入的密码。
    pub password: String,
    /// QA 档案指定的会话显示名。
    pub name_override: Option<String>,
}

/// 按驱动字段集校验输入并组装草稿；失败时返回可直接展示的中文原因。
///
/// 校验只覆盖本驱动真正用到的字段：给 SQLite 传了主机/端口也不会被接受，
/// 反过来 MariaDB 的空主机、空端口、空用户名必须拦住。
///
/// 库名**不是**必填（ADR 0024 §3）：留空表示「连上服务器后浏览所有库」。
/// 空库名会原样进入草稿（`database` 为空串），不会被解释成
/// `information_schema` 或任何默认库。
pub fn build_draft(input: &FormInput) -> Result<ConnectionDraft, String> {
    let Some(family) = DriverFamily::for_id(&input.driver_id) else {
        return Err(format!(
            "此版本前端没有驱动「{}」的连接表单（后端已注册该驱动）",
            input.driver_id
        ));
    };
    let fields = fields_for(family);
    let mut draft = ConnectionDraft {
        name: String::new(),
        driver_id: input.driver_id.clone(),
        host: String::new(),
        port: 0,
        database: String::new(),
        username: String::new(),
        // 默认「禁用」而不是 `TlsMode::default()`（优先）：文件型驱动没有 TLS，
        // 保存到 connections.json 的 `tls_mode` 必须是 disabled，不能写一个假的安全策略。
        tls_mode: TlsMode::Disabled,
        file_path: String::new(),
        read_only: false,
        credential_ref: input.credential_ref.clone(),
        password: String::new(),
    };

    if fields.host {
        let host = input.host.trim();
        if host.is_empty() {
            return Err("请填写主机".to_owned());
        }
        draft.host = host.to_owned();
    }
    if fields.port {
        let port_text = input.port.trim();
        let port: u16 = port_text
            .parse()
            .map_err(|_| format!("端口必须是 1-65535 之间的数字（当前：{port_text}）"))?;
        if port == 0 {
            return Err("端口必须是 1-65535 之间的数字".to_owned());
        }
        draft.port = port;
    }
    if fields.database {
        // 空库名是合法值：语义是「没有默认库」，不是「缺少必填项」。
        draft.database = input.database.trim().to_owned();
    }
    if fields.username {
        let username = input.username.trim();
        if username.is_empty() {
            return Err("请填写用户名".to_owned());
        }
        draft.username = username.to_owned();
    }
    if fields.tls {
        draft.tls_mode = input.tls_mode;
    }
    if fields.password {
        let password = input.password.clone();
        if password.is_empty() && input.credential_ref.is_none() {
            return Err("请输入密码".to_owned());
        }
        draft.password = password;
    }
    if fields.file_path {
        let path = input.file_path.trim();
        if path.is_empty() {
            return Err("请填写数据库文件路径（例如 /home/me/app.db）".to_owned());
        }
        // 文件不存在时前端先拦住：让后端抛「无法打开数据库」只会把真正的原因
        // （路径打错 / 目录不存在 / 权限）藏在驱动错误里。
        if path != SQLITE_MEMORY_PATH && !Path::new(path).exists() {
            return Err(format!(
                "数据库文件不存在：{path}（请选择已有文件，或先用 sqlite3 创建）"
            ));
        }
        draft.file_path = path.to_owned();
    }
    if fields.read_only {
        draft.read_only = input.read_only;
        if draft.read_only && draft.file_path == SQLITE_MEMORY_PATH {
            // 与后端 `SqliteConfig::from_params` 同一条规则，提前在界面上说清楚。
            return Err("内存数据库（:memory:）不支持只读模式".to_owned());
        }
    }

    draft.name = input
        .name_override
        .clone()
        .unwrap_or_else(|| default_name(&draft, family));
    Ok(draft)
}

/// 会话显示名：服务器型 `用户@主机/库`（无默认库时 `用户@主机`），
/// 文件型 `文件名（SQLite）`。
///
/// 名字里带驱动，是为了让会话列表/标题栏能看出连的是哪种数据库——
/// 之前所有连接都按 `user@host/db` 派生，SQLite 的显示名会变成
/// `/home/me/app.db@/home/me/app.db` 这种没有意义的东西。
///
/// 没有默认库时**不拼斜杠**：`user@host/` 这种残缺名字看不出「库为空」，
/// 只会让用户以为库名丢了（ADR 0024 §3）。
#[must_use]
pub fn default_name(draft: &ConnectionDraft, family: DriverFamily) -> String {
    match family {
        DriverFamily::MariaDb => {
            let database = draft.database.trim();
            if database.is_empty() {
                format!("{}@{}", draft.username, draft.host)
            } else {
                format!("{}@{}/{}", draft.username, draft.host, database)
            }
        }
        DriverFamily::Sqlite => {
            let file = Path::new(&draft.file_path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| draft.file_path.clone());
            if draft.read_only {
                format!("{file}（SQLite 只读）")
            } else {
                format!("{file}（SQLite）")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mariadb_input() -> FormInput {
        FormInput {
            driver_id: "mariadb".to_owned(),
            host: "127.0.0.1".to_owned(),
            port: "53306".to_owned(),
            database: "sqarvo_test".to_owned(),
            username: "sqarvo_test".to_owned(),
            tls_mode: TlsMode::Preferred,
            password: "hunter2".to_owned(),
            ..FormInput::default()
        }
    }

    fn sqlite_input(path: &str) -> FormInput {
        FormInput {
            driver_id: "sqlite".to_owned(),
            file_path: path.to_owned(),
            ..FormInput::default()
        }
    }

    #[test]
    fn field_sets_differ_between_families() {
        // MariaDB：网络与凭据字段可见，文件字段不可见。
        let server = fields_for_driver("mariadb").expect("mariadb 是已知驱动");
        assert!(server.host && server.port && server.database && server.username);
        assert!(server.tls && server.password);
        assert!(!server.file_path && !server.read_only);

        // SQLite：只有文件路径与只读开关，主机/端口/用户/TLS/密码一律不显示。
        let file = fields_for_driver("sqlite").expect("sqlite 是已知驱动");
        assert!(file.file_path && file.read_only);
        assert!(!file.host && !file.port && !file.database);
        assert!(!file.username && !file.tls && !file.password);
    }

    #[test]
    fn unknown_driver_has_no_field_set() {
        // 后端可能注册前端还不认识的驱动：不猜字段，交给对话框如实说明。
        assert_eq!(fields_for_driver("postgresql"), None);
        let mut input = mariadb_input();
        input.driver_id = "postgresql".to_owned();
        let error = build_draft(&input).unwrap_err();
        assert!(error.contains("postgresql"), "{error}");
        assert!(error.contains("没有"), "{error}");
    }

    #[test]
    fn mariadb_draft_keeps_network_fields_and_credential() {
        let draft = build_draft(&mariadb_input()).expect("MariaDB 草稿应有效");
        assert_eq!(draft.driver_id, "mariadb");
        assert_eq!(draft.host, "127.0.0.1");
        assert_eq!(draft.port, 53306);
        assert_eq!(draft.database, "sqarvo_test");
        assert_eq!(draft.username, "sqarvo_test");
        assert_eq!(draft.tls_mode, TlsMode::Preferred);
        assert_eq!(draft.password, "hunter2");
        // 文件字段对 MariaDB 没有意义：即使输入里有也不能带进草稿。
        assert!(draft.file_path.is_empty());
        assert!(!draft.read_only);
        assert_eq!(draft.name, "sqarvo_test@127.0.0.1/sqarvo_test");
        assert!(draft.needs_credential());
    }

    #[test]
    fn mariadb_validation_blocks_empty_host_port_and_username() {
        let mut input = mariadb_input();
        input.host = "   ".to_owned();
        assert_eq!(build_draft(&input).unwrap_err(), "请填写主机");

        let mut input = mariadb_input();
        input.port = "3306a".to_owned();
        assert!(build_draft(&input).unwrap_err().contains("端口"));

        let mut input = mariadb_input();
        input.port = "0".to_owned();
        assert!(build_draft(&input).unwrap_err().contains("端口"));

        let mut input = mariadb_input();
        input.username = String::new();
        assert_eq!(build_draft(&input).unwrap_err(), "请填写用户名");

        let mut input = mariadb_input();
        input.password = String::new();
        assert_eq!(build_draft(&input).unwrap_err(), "请输入密码");

        // 已保存凭据时密码可以为空（复用旧凭据）。
        let mut input = mariadb_input();
        input.password = String::new();
        input.credential_ref = Some("sqarvo:conn/00000000-0000-4000-8000-000000000000".to_owned());
        assert!(build_draft(&input).is_ok());
    }

    #[test]
    fn mariadb_accepts_an_empty_database_as_no_default_catalog() {
        // 「先连服务器、再从树里挑库」是标准姿势：库名留空要能提交，
        // 草稿里保持空串——不能在这里补成 information_schema 或别的默认库。
        let mut input = mariadb_input();
        input.database = String::new();
        let draft = build_draft(&input).expect("空库名应可提交");
        assert!(draft.database.is_empty());
        assert_eq!(draft.host, "127.0.0.1");
        assert_eq!(draft.port, 53306);
        assert_eq!(draft.username, "sqarvo_test");

        // 只留空白的库名与空串等价。
        let mut input = mariadb_input();
        input.database = "   ".to_owned();
        let draft = build_draft(&input).expect("空白库名应可提交");
        assert!(draft.database.is_empty());
    }

    #[test]
    fn name_degrades_to_user_at_host_without_a_default_database() {
        let mut input = mariadb_input();
        input.database = String::new();
        let draft = build_draft(&input).expect("空库名应可提交");
        assert_eq!(draft.name, "sqarvo_test@127.0.0.1");
        assert!(
            !draft.name.ends_with('/'),
            "不得生成残缺名字：{}",
            draft.name
        );
        // 有默认库时保持既有格式（回归）。
        assert_eq!(
            build_draft(&mariadb_input()).unwrap().name,
            "sqarvo_test@127.0.0.1/sqarvo_test"
        );
    }

    #[test]
    fn sqlite_requires_an_existing_file() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("demo.db");
        std::fs::write(&path, b"").expect("建空文件");
        let text = path.to_str().expect("UTF-8 路径");

        let draft = build_draft(&sqlite_input(text)).expect("存在的文件应通过");
        assert_eq!(draft.driver_id, "sqlite");
        assert_eq!(draft.file_path, text);
        assert!(!draft.read_only);
        // 主机/端口/库/用户名绝不能从输入里漏进 SQLite 草稿。
        assert!(draft.host.is_empty() && draft.database.is_empty() && draft.username.is_empty());
        assert_eq!(draft.port, 0);
        assert_eq!(draft.name, "demo.db（SQLite）");
        assert!(!draft.needs_credential());
        // 文件型驱动没有 TLS：保存到配置里的 tls_mode 必须是 disabled，而不是「优先」。
        assert_eq!(draft.tls_mode, TlsMode::Disabled);
    }

    #[test]
    fn sqlite_validation_blocks_empty_and_missing_paths() {
        assert!(
            build_draft(&sqlite_input(""))
                .unwrap_err()
                .contains("文件路径")
        );
        assert!(
            build_draft(&sqlite_input("   "))
                .unwrap_err()
                .contains("文件路径")
        );

        let missing = "/tmp/sqarvo-does-not-exist-67/app.db";
        let error = build_draft(&sqlite_input(missing)).unwrap_err();
        // 必须先说「文件不存在」并带上路径，而不是让后端报晦。
        assert!(error.contains("数据库文件不存在"), "{error}");
        assert!(error.contains(missing), "{error}");
    }

    #[test]
    fn sqlite_memory_database_is_not_a_read_only_candidate() {
        let draft = build_draft(&sqlite_input(SQLITE_MEMORY_PATH)).expect("内存库可用");
        assert_eq!(draft.file_path, SQLITE_MEMORY_PATH);

        let mut input = sqlite_input(SQLITE_MEMORY_PATH);
        input.read_only = true;
        let error = build_draft(&input).unwrap_err();
        assert!(error.contains("不支持只读"), "{error}");
    }

    #[test]
    fn sqlite_read_only_is_carried_and_named() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("demo.db");
        std::fs::write(&path, b"").expect("建空文件");
        let mut input = sqlite_input(path.to_str().expect("UTF-8 路径"));
        input.read_only = true;
        let draft = build_draft(&input).expect("只读草稿应有效");
        assert!(draft.read_only);
        assert_eq!(draft.name, "demo.db（SQLite 只读）");
    }

    #[test]
    fn name_override_wins_for_every_family() {
        let mut input = mariadb_input();
        input.name_override = Some("演示库".to_owned());
        assert_eq!(build_draft(&input).unwrap().name, "演示库");

        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("demo.db");
        std::fs::write(&path, b"").expect("建空文件");
        let mut input = sqlite_input(path.to_str().expect("UTF-8 路径"));
        input.name_override = Some("演示库".to_owned());
        assert_eq!(build_draft(&input).unwrap().name, "演示库");
    }

    #[test]
    fn sqlite_draft_ignores_network_and_credential_inputs() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("demo.db");
        std::fs::write(&path, b"").expect("建空文件");
        let mut input = sqlite_input(path.to_str().expect("UTF-8 路径"));
        // 切驱动后残留的旧值必须被忽略，而不是「顺带提交」。
        input.host = "127.0.0.1".to_owned();
        input.port = "3306".to_owned();
        input.database = "ignored".to_owned();
        input.username = "ignored".to_owned();
        input.password = "hunter2".to_owned();
        input.credential_ref = Some("sqarvo:conn/unused".to_owned());

        let draft = build_draft(&input).expect("SQLite 草稿应有效");
        assert!(draft.password.is_empty(), "SQLite 不应携带密码");
        let params = draft.params("");
        assert!(params.credential_ref.is_empty());
        assert!(params.username.is_empty());
        assert_eq!(params.port, 0);
    }
}
