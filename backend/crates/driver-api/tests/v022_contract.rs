//! v0.2.2 契约（#81）的 SPI 侧断言：插入行、元数据过滤、DDL 来源。
//!
//! 这些断言**不依赖任何驱动实现**，锁的是三件事：
//!
//! 1. **默认实现必须显式报错**：现有 SQLite / MariaDB 驱动一行没改，任何走到插入
//!    或过滤的调用都必须拿到 `capability.unsupported`，绝不能拿到「成功了但什么
//!    都没发生」的空报告（那会让界面显示已保存）；
//! 2. **DDL 必须自证来源**：`exact = false` 却说不清为什么拼装、拼装器是哪个版本的
//!    结果不能被当成可信 DDL；
//! 3. **指纹与钳制是契约的一部分**：过滤条件折进分页 token 的字符串格式、批次
//!    500 行的钳制口径都必须在两个驱动之间一致。

use async_trait::async_trait;
use sqarvo_common::{ErrorCategory, ErrorCode};
use sqarvo_driver_api::{
    ColumnChange, ColumnMeta, Ddl, DdlSource, InsertOutcome, InsertReport, InsertRow,
    InsertRowResult, InsertRowsRequest, MAX_ROW_CHANGES_PER_BATCH, MAX_ROWS_PER_INSERT_BATCH,
    MetadataProvider, NameMatch, ObjectFilter, ObjectKind, ObjectNode, ObjectRef, Page,
    PageRequest, PrimaryKey, TableDataProvider, TablePage, Value, column_not_insertable,
    insert_permission_denied, invalid_change, not_null_violation, not_null_without_default,
    object_kind_unsupported, object_not_found, unique_violation, unsupported_metadata_filtering,
    unsupported_row_insertion,
};

// ---------------------------------------------------------------------------
// 测试替身：只实现**必需**方法，其余全部走默认实现（这正是要断言的东西）
// ---------------------------------------------------------------------------

struct NoInsertDriver;

#[async_trait]
impl TableDataProvider for NoInsertDriver {
    async fn primary_key(&self, _object: &ObjectRef) -> sqarvo_common::Result<PrimaryKey> {
        Ok(PrimaryKey::default())
    }

    async fn browse(
        &self,
        _object: &ObjectRef,
        _page: &PageRequest,
    ) -> sqarvo_common::Result<TablePage> {
        Ok(TablePage {
            columns: Vec::new(),
            rows: Vec::new(),
            next_page_token: None,
            has_more: false,
            read_only: true,
            read_only_reason: None,
            ordering: sqarvo_driver_api::RowOrder::Unstable,
        })
    }
}

struct NoFilterMetadata;

#[async_trait]
impl MetadataProvider for NoFilterMetadata {
    async fn list_children(
        &self,
        _parent: Option<ObjectRef>,
        _page: &PageRequest,
    ) -> sqarvo_common::Result<Page<ObjectNode>> {
        Ok(Page {
            items: Vec::new(),
            next_page_token: None,
            truncated: false,
        })
    }

    async fn columns(&self, _object: &ObjectRef) -> sqarvo_common::Result<Vec<ColumnMeta>> {
        Ok(Vec::new())
    }

    async fn ddl(&self, _object: &ObjectRef) -> sqarvo_common::Result<Ddl> {
        Err(unsupported_row_insertion())
    }
}

fn table_ref() -> ObjectRef {
    ObjectRef {
        session_id: sqarvo_common::SessionId::new(),
        qualified_name: sqarvo_driver_api::QualifiedName::new(
            Some("shop".to_owned()),
            None,
            "orders",
        ),
        kind: ObjectKind::Table,
    }
}

fn one_row() -> InsertRow {
    InsertRow::new(vec![ColumnChange {
        column: "id".to_owned(),
        value: Value::Int(1),
    }])
}

// ---------------------------------------------------------------------------
// 1. 「未实现必须显式报错」
// ---------------------------------------------------------------------------

/// 既有驱动不改一行代码时，插入**必须**返回 `capability.unsupported`。
///
/// 若有人把默认实现改成 `Ok(InsertReport::default())`（看起来更「宽容」），这条
/// 会立刻变红：空报告会被 rpc-server 翻译成「插入了 0 行」的成功响应。
#[tokio::test]
async fn default_insert_rows_reports_unsupported_instead_of_empty_success() {
    let driver = NoInsertDriver;
    assert!(
        !driver.supports_row_insertion(),
        "默认能力位必须是 false：没实现就不能声明支持"
    );

    let error = driver
        .insert_rows(InsertRowsRequest::new(table_ref(), vec![one_row()]))
        .await
        .expect_err("默认实现必须报错，绝不能返回空报告");
    assert_eq!(error.code(), ErrorCode::CAPABILITY_UNSUPPORTED);
    assert_eq!(error.category(), ErrorCategory::Unsupported);
    // 文案要能指导用户：说清「驱动没实现」，而不是「插入失败」。
    assert!(error.message().contains("未实现"));
}

/// 非空过滤同理：默认实现必须报错，不能静默退化成「不过滤」。
#[tokio::test]
async fn default_metadata_filter_reports_unsupported_instead_of_unfiltered_list() {
    let provider = NoFilterMetadata;
    let filter = ObjectFilter {
        name: Some(NameMatch::Contains("ord".to_owned())),
        kinds: Vec::new(),
    };

    let error = provider
        .list_children_filtered(None, &filter, &PageRequest::default())
        .await
        .expect_err("默认实现必须报错，绝不能返回未过滤的列表");
    assert_eq!(error.code(), ErrorCode::CAPABILITY_UNSUPPORTED);
    assert_eq!(error.category(), ErrorCategory::Unsupported);

    // 错误构造函数本身也是契约：rpc-server 用同一文案。
    assert_eq!(
        unsupported_metadata_filtering().code(),
        ErrorCode::CAPABILITY_UNSUPPORTED
    );
}

// ---------------------------------------------------------------------------
// 2. DDL 来源必须自证
// ---------------------------------------------------------------------------

#[test]
fn ddl_source_is_self_describing() {
    let exact = Ddl::from_database("CREATE TABLE orders (id INTEGER)", "sqlite_master.sql");
    assert!(exact.exact);
    assert_eq!(exact.source, DdlSource::Database);
    assert!(exact.is_consistent(), "原文必须说明取自哪里");
    assert_eq!(exact.generator_version, None);

    let built = Ddl::synthesized(
        "CREATE TABLE orders (id INTEGER)",
        "由 information_schema 拼装；不含 CHECK 约束与索引",
        "ddl-gen/1",
    );
    assert!(!built.exact, "拼装结果绝不能标成原文");
    assert_eq!(built.source, DdlSource::Synthesized);
    assert!(built.is_consistent());
    assert_eq!(built.generator_version.as_deref(), Some("ddl-gen/1"));

    // 反面：exact 与 source 打架、拼装却不说来源、拼装却没有生成器版本。
    let contradictory = Ddl {
        text: "CREATE TABLE t (id INTEGER)".to_owned(),
        exact: true,
        source: DdlSource::Synthesized,
        source_detail: Some("拼装".to_owned()),
        generator_version: Some("ddl-gen/1".to_owned()),
    };
    assert!(
        !contradictory.is_consistent(),
        "exact=true 不能配 source=synthesized"
    );

    let unexplained = Ddl::synthesized("CREATE TABLE t (id INTEGER)", "   ", "ddl-gen/1");
    assert!(!unexplained.is_consistent(), "空白说明等于没说明");

    let unversioned = Ddl::synthesized("CREATE TABLE t (id INTEGER)", "拼装", "");
    assert!(
        !unversioned.is_consistent(),
        "拼装必须能定位到具体生成器版本"
    );

    let unnamed_source = Ddl::from_database("CREATE TABLE t (id INTEGER)", "");
    assert!(!unnamed_source.is_consistent(), "原文也要说明取自哪里");

    assert_eq!(DdlSource::Database.as_str(), "database");
    assert_eq!(DdlSource::Synthesized.as_str(), "synthesized");
}

// ---------------------------------------------------------------------------
// 3. 过滤条件：指纹格式与结构校验
// ---------------------------------------------------------------------------

#[test]
fn filter_fingerprint_is_stable_and_kind_order_independent() {
    // 空过滤的指纹为空串：不带过滤的请求不会因为「有没有 filter 对象」而失效。
    assert_eq!(ObjectFilter::none().fingerprint_input(), "");
    assert!(ObjectFilter::none().is_empty());

    let contains = ObjectFilter {
        name: Some(NameMatch::Contains("ord".to_owned())),
        kinds: Vec::new(),
    };
    assert_eq!(contains.fingerprint_input(), "name=contains:ord");

    let exact = ObjectFilter {
        name: Some(NameMatch::Exact("orders".to_owned())),
        kinds: Vec::new(),
    };
    assert_eq!(exact.fingerprint_input(), "name=exact:orders");
    // contains 与 exact 是不同条件：指纹必须不同，否则换条件还能接着翻页。
    assert_ne!(contains.fingerprint_input(), exact.fingerprint_input());

    let view_first = ObjectFilter {
        name: None,
        kinds: vec![ObjectKind::View, ObjectKind::Table],
    };
    let table_first = ObjectFilter {
        name: None,
        kinds: vec![ObjectKind::Table, ObjectKind::View],
    };
    // 种类是集合语义：顺序不同必须产出同一个指纹，否则「换个顺序搜同样的东西」
    // 会让在途 token 无故失效。
    assert_eq!(
        view_first.fingerprint_input(),
        table_first.fingerprint_input()
    );
    assert_eq!(view_first.fingerprint_input(), "kinds=table,view");

    let both = ObjectFilter {
        name: Some(NameMatch::Contains("%_".to_owned())),
        kinds: vec![ObjectKind::Table],
    };
    assert_eq!(both.fingerprint_input(), "name=contains:%_;kinds=table");
}

#[test]
fn filter_validation_rejects_ambiguous_conditions() {
    // 空串不是「匹配全部」的简写：不过滤必须留空字段。
    let empty_name = ObjectFilter {
        name: Some(NameMatch::Contains(String::new())),
        kinds: Vec::new(),
    };
    assert_eq!(
        empty_name
            .validate()
            .expect_err("空过滤值必须被拒绝")
            .code(),
        ErrorCode::METADATA_FILTER_INVALID
    );

    let duplicated = ObjectFilter {
        name: None,
        kinds: vec![ObjectKind::Table, ObjectKind::Table],
    };
    assert_eq!(
        duplicated
            .validate()
            .expect_err("重复种类必须被拒绝")
            .code(),
        ErrorCode::METADATA_FILTER_INVALID
    );

    let valid = ObjectFilter {
        name: Some(NameMatch::Contains("ord".to_owned())),
        kinds: vec![ObjectKind::Table, ObjectKind::View],
    };
    valid.validate().expect("合法过滤条件不应报错");
}

// ---------------------------------------------------------------------------
// 4. 批次钳制与风险阈值
// ---------------------------------------------------------------------------

#[test]
fn insert_batch_is_clamped_at_five_hundred_rows() {
    assert_eq!(MAX_ROWS_PER_INSERT_BATCH, MAX_ROW_CHANGES_PER_BATCH);
    assert_eq!(MAX_ROWS_PER_INSERT_BATCH, 500);

    let many = InsertRowsRequest::new(
        table_ref(),
        (0..MAX_ROWS_PER_INSERT_BATCH + 5)
            .map(|_| one_row())
            .collect(),
    );
    assert!(many.is_clamped());
    assert_eq!(many.effective_rows().len(), MAX_ROWS_PER_INSERT_BATCH);
    assert_eq!(many.deferred_rows(), 5);
    // 预计影响行数按**实际执行**的行算：钳制后的 500，不是请求的 505。
    assert_eq!(many.estimated_rows_affected(), 500);

    let single = InsertRowsRequest::new(table_ref(), vec![one_row()]);
    assert!(!single.is_clamped());
    assert_eq!(
        single.estimated_rows_affected(),
        1,
        "单行插入就是批量的一条"
    );
    assert_eq!(single.deferred_rows(), 0);
}

// ---------------------------------------------------------------------------
// 5. 省略 ≠ NULL、结构校验与逐行结果语义
// ---------------------------------------------------------------------------

#[test]
fn omitted_columns_are_not_null() {
    // 省略 id（交给自增）：values 里只有一列。
    let insert = InsertRow::new(vec![ColumnChange {
        column: "name".to_owned(),
        value: Value::Text("tea".to_owned()),
    }]);
    assert_eq!(insert.columns(), vec!["name"]);
    insert.validate().expect("省略列是合法插入");

    // 显式 NULL：同一列以 Value::Null 出现，与「省略」在结构上可区分。
    let explicit_null = InsertRow::new(vec![ColumnChange {
        column: "name".to_owned(),
        value: Value::Null,
    }]);
    assert_eq!(explicit_null.values.len(), 1);
    assert_eq!(explicit_null.values[0].value, Value::Null);
    explicit_null.validate().expect("显式 NULL 是合法插入");
    assert_ne!(
        insert.values, explicit_null.values,
        "省略与显式 NULL 必须可区分，否则自增列会被写成 NULL"
    );
}

#[test]
fn insert_structure_violations_use_config_invalid() {
    let duplicated = InsertRowsRequest::new(
        table_ref(),
        vec![InsertRow::new(vec![
            ColumnChange {
                column: "name".to_owned(),
                value: Value::Text("a".to_owned()),
            },
            ColumnChange {
                column: "name".to_owned(),
                value: Value::Text("b".to_owned()),
            },
        ])],
    );
    assert_eq!(
        duplicated.validate().expect_err("重复列必须被拒绝").code(),
        ErrorCode::CONFIG_INVALID
    );

    let empty_batch = InsertRowsRequest::new(table_ref(), Vec::new());
    assert_eq!(
        empty_batch.validate().expect_err("空批次必须被拒绝").code(),
        ErrorCode::CONFIG_INVALID
    );

    let blank_column = InsertRow::new(vec![ColumnChange {
        column: "  ".to_owned(),
        value: Value::Int(1),
    }]);
    assert_eq!(
        blank_column
            .validate()
            .expect_err("空列名必须被拒绝")
            .code(),
        ErrorCode::CONFIG_INVALID
    );

    let unsupported = InsertRow::new(vec![ColumnChange {
        column: "geom".to_owned(),
        value: Value::Unsupported {
            type_name: "geometry".to_owned(),
            text: "POINT(0 0)".to_owned(),
        },
    }]);
    assert_eq!(
        unsupported
            .validate()
            .expect_err("Unsupported 值不能写入")
            .code(),
        ErrorCode::CONFIG_INVALID
    );
}

#[test]
fn rolled_back_batch_reports_zero_net_rows() {
    let report = InsertReport {
        results: vec![
            InsertRowResult {
                index: 0,
                outcome: InsertOutcome::Inserted { rows_affected: 1 },
                key: None,
                message: None,
            },
            InsertRowResult {
                index: 1,
                outcome: InsertOutcome::Failed {
                    code: ErrorCode::TABLE_DATA_UNIQUE_VIOLATION,
                },
                key: None,
                message: Some("列 email 的值已存在".to_owned()),
            },
        ],
        deferred: 0,
        rolled_back: true,
    };
    // 逐行：一行语句层成功；整批：净影响 0（已回滚）。
    assert_eq!(report.results[0].rows_affected(), 1);
    assert_eq!(report.rows_affected(), 0, "整批回滚后净影响必须为 0");
    assert_eq!(report.inserted_rows(), 1, "语句层成功数不受回滚影响");
    assert!(report.has_failures());

    let committed = InsertReport {
        rolled_back: false,
        ..report
    };
    assert_eq!(committed.rows_affected(), 1, "未回滚时净影响 = 成功行数");
    assert_eq!(
        InsertOutcome::Inserted { rows_affected: 1 }.as_str(),
        "inserted"
    );
    assert_eq!(
        InsertOutcome::Failed {
            code: ErrorCode::TABLE_DATA_UNIQUE_VIOLATION
        }
        .as_str(),
        "failed"
    );
    assert_eq!(
        InsertOutcome::Inserted { rows_affected: 1 }.error_code(),
        None
    );
    assert_eq!(
        InsertOutcome::Failed {
            code: ErrorCode::TABLE_DATA_UNIQUE_VIOLATION
        }
        .error_code(),
        Some(ErrorCode::TABLE_DATA_UNIQUE_VIOLATION)
    );
}

// ---------------------------------------------------------------------------
// 6. 新错误码：文案与分类
// ---------------------------------------------------------------------------

#[test]
fn insert_and_metadata_error_codes_match_documentation() {
    let cases = [
        (
            unique_violation("email"),
            ErrorCode::TABLE_DATA_UNIQUE_VIOLATION,
            ErrorCategory::AlreadyExists,
        ),
        (
            not_null_violation("email"),
            ErrorCode::TABLE_DATA_NOT_NULL_VIOLATION,
            ErrorCategory::InvalidArgument,
        ),
        (
            not_null_without_default("email"),
            ErrorCode::TABLE_DATA_NOT_NULL_WITHOUT_DEFAULT,
            ErrorCategory::InvalidArgument,
        ),
        (
            column_not_insertable("created_at"),
            ErrorCode::TABLE_DATA_COLUMN_NOT_INSERTABLE,
            ErrorCategory::InvalidArgument,
        ),
        (
            insert_permission_denied("shop.orders"),
            ErrorCode::TABLE_DATA_PERMISSION_DENIED,
            ErrorCategory::PermissionDenied,
        ),
        (
            object_not_found("shop.orders"),
            ErrorCode::METADATA_OBJECT_NOT_FOUND,
            ErrorCategory::NotFound,
        ),
        (
            object_kind_unsupported(&ObjectKind::Column, "GetDdl"),
            ErrorCode::METADATA_OBJECT_KIND_UNSUPPORTED,
            ErrorCategory::InvalidArgument,
        ),
    ];

    for (error, code, category) in cases {
        assert_eq!(error.code(), code, "错误码文本漂移");
        assert_eq!(error.category(), category, "{code} 的分类漂移");
        // 文案面向用户：不泄露 SQL、参数值与凭据。
        let message = error.message();
        for forbidden in ["INSERT ", "SELECT ", "password", "secret"] {
            assert!(
                !message.contains(forbidden),
                "错误文案不得包含 {forbidden}：{message}"
            );
        }
    }

    // 两类 NOT NULL 错误必须可区分（处置不同，前端不能合并成一句话）。
    assert_ne!(
        not_null_violation("email").message(),
        not_null_without_default("email").message()
    );
    // 结构校验错误沿用既有码，不新造。
    assert_eq!(invalid_change("x").code(), ErrorCode::CONFIG_INVALID);
}
