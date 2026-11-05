//! 表数据浏览与行编辑的契约测试（ADR 0017）。
//!
//! 三类断言，都不依赖服务实现：
//!
//! 1. **Protobuf 字段号兼容**：直接读 `build.rs` 生成的 `FileDescriptorSet`，
//!    逐字段锁定编号、名字、类型与枚举取值。字段号一旦发布不得复用，改动这里
//!    必须同时提升协议主版本；
//! 2. **错误语义**：新增错误码 → gRPC 状态码 → trailing metadata 的映射固定，
//!    并且文案不含 SQL、参数值与凭据；
//! 3. **游标不透明性**：`next_page_token` 只能是字符串，客户端原样回传，
//!    被篡改的游标必须报 `table_data.cursor_invalid`，不得回退到第一页。

use std::collections::BTreeMap;

use prost::Message;
use prost_types::{DescriptorProto, FieldDescriptorProto, FileDescriptorSet};
use sqarvo_common::{Error, ErrorCode};
use sqarvo_driver_api::{
    ColumnMeta, ColumnType, DriverSession, ExecutionStart, IsolationLevel,
    ParameterizedExecuteRequest, PrimaryKey, PrimaryKeyColumn, ReadOnlyReason, ResultBatch,
    RowChange, RowChangeRequest, RowKey, RowOrder, TableDataProvider, TablePage, Value,
};
use sqarvo_rpc_server::error::{ERROR_CODE_METADATA_KEY, to_status};
use tonic::Code;

/// `build.rs` 生成的原样描述符（不进入 git）。
const DESCRIPTOR_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/sqarvo-descriptor.bin"));

fn descriptor_set() -> FileDescriptorSet {
    FileDescriptorSet::decode(DESCRIPTOR_BYTES).expect("描述符可解析")
}

/// 取出某文件中的全部 message，键为 `message.name`。
fn messages(file_name: &str) -> BTreeMap<String, DescriptorProto> {
    let set = descriptor_set();
    let file = set
        .file
        .iter()
        .find(|file| file.name() == file_name)
        .unwrap_or_else(|| panic!("描述符中缺少 {file_name}"));
    file.message_type
        .iter()
        .map(|message| (message.name().to_owned(), message.clone()))
        .collect()
}

/// 断言某 message 的字段编号与名字完全一致（顺序即声明顺序）。
fn assert_fields(message: &DescriptorProto, expected: &[(i32, &str)]) {
    let actual: Vec<(i32, &str)> = message
        .field
        .iter()
        .map(|field| (field.number(), field.name()))
        .collect();
    assert_eq!(
        actual,
        expected.to_vec(),
        "{} 的字段编号/名字发生变化：字段号一旦发布不得复用",
        message.name()
    );
}

fn field<'a>(message: &'a DescriptorProto, name: &str) -> &'a FieldDescriptorProto {
    message
        .field
        .iter()
        .find(|field| field.name() == name)
        .unwrap_or_else(|| panic!("{} 缺少字段 {name}", message.name()))
}

fn type_name_of<'a>(message: &'a DescriptorProto, name: &str) -> &'a str {
    field(message, name).type_name()
}

#[test]
fn table_data_message_field_numbers_are_frozen() {
    let table_data = messages("services/v1/table_data.proto");

    assert_fields(
        &table_data["BrowseTableRequest"],
        &[(1, "object"), (2, "page_size"), (3, "page_token")],
    );
    assert_fields(
        &table_data["BrowseTableResponse"],
        &[
            (1, "columns"),
            (2, "rows"),
            (3, "next_page_token"),
            (4, "has_more"),
            (5, "read_only"),
            (6, "read_only_reason"),
            (7, "ordering"),
        ],
    );
    assert_fields(
        &table_data["PrimaryKeyValue"],
        &[(1, "column"), (2, "value")],
    );
    assert_fields(&table_data["RowKey"], &[(1, "values")]);
    assert_fields(&table_data["ColumnChange"], &[(1, "column"), (2, "value")]);
    assert_fields(
        &table_data["RowUpdate"],
        &[(1, "key"), (2, "changes"), (3, "expected_rows_affected")],
    );
    assert_fields(
        &table_data["RowDelete"],
        &[(1, "key"), (2, "expected_rows_affected")],
    );
    assert_fields(
        &table_data["ApplyRowChangesRequest"],
        &[
            (1, "session_id"),
            (2, "object"),
            (3, "transaction_id"),
            (4, "updates"),
            (5, "deletes"),
            (6, "acknowledge_risk"),
        ],
    );
    assert_fields(
        &table_data["RowChangeResult"],
        &[
            (1, "change_index"),
            (2, "outcome"),
            (3, "rows_affected"),
            (4, "message"),
        ],
    );
    assert_fields(
        &table_data["ApplyRowChangesResponse"],
        &[
            (1, "results"),
            (2, "total_rows_affected"),
            (3, "clamped"),
            (4, "deferred_changes"),
        ],
    );
}

#[test]
fn table_data_field_types_preserve_value_model() {
    let table_data = messages("services/v1/table_data.proto");

    // 行与列复用查询结果的强类型值模型，不得退化成字符串。
    assert_eq!(
        type_name_of(&table_data["BrowseTableResponse"], "columns"),
        ".sqarvo.common.v1.ColumnMeta"
    );
    assert_eq!(
        type_name_of(&table_data["BrowseTableResponse"], "rows"),
        ".sqarvo.common.v1.Row"
    );
    assert_eq!(
        type_name_of(&table_data["PrimaryKeyValue"], "value"),
        ".sqarvo.common.v1.ColumnValue"
    );
    assert_eq!(
        type_name_of(&table_data["ColumnChange"], "value"),
        ".sqarvo.common.v1.ColumnValue"
    );

    // 游标必须是不透明字符串：不允许换成客户端可解析的结构化 message。
    assert_eq!(
        field(&table_data["BrowseTableResponse"], "next_page_token").r#type(),
        prost_types::field_descriptor_proto::Type::String
    );
    assert_eq!(
        field(&table_data["BrowseTableRequest"], "page_token").r#type(),
        prost_types::field_descriptor_proto::Type::String
    );

    // 确认字段必须是显式布尔，不能靠「是否有变更」推断。
    assert_eq!(
        field(&table_data["ApplyRowChangesRequest"], "acknowledge_risk").r#type(),
        prost_types::field_descriptor_proto::Type::Bool
    );

    // 行变更不得携带 SQL 文本字段。
    for name in ["RowUpdate", "RowDelete", "RowKey"] {
        assert!(
            table_data[name]
                .field
                .iter()
                .all(|field| !field.name().contains("sql")),
            "{name} 不得携带 SQL"
        );
    }
}

#[test]
fn primary_key_rpc_is_additive_to_metadata_service() {
    let set = descriptor_set();
    let metadata_file = set
        .file
        .iter()
        .find(|file| file.name() == "services/v1/metadata.proto")
        .expect("描述符含 metadata.proto");
    let service = metadata_file
        .service
        .iter()
        .find(|service| service.name() == "MetadataService")
        .expect("MetadataService 存在");

    let methods: Vec<&str> = service
        .method
        .iter()
        .map(prost_types::MethodDescriptorProto::name)
        .collect();
    assert_eq!(
        methods,
        vec!["ListNodes", "GetColumns", "GetDdl", "GetPrimaryKey"],
        "既有 RPC 顺序不得变化，新 RPC 只能追加"
    );

    let primary_key = service
        .method
        .iter()
        .find(|method| method.name() == "GetPrimaryKey")
        .expect("GetPrimaryKey 存在");
    assert_eq!(
        primary_key.input_type(),
        ".sqarvo.services.v1.GetPrimaryKeyRequest"
    );
    assert_eq!(
        primary_key.output_type(),
        ".sqarvo.services.v1.GetPrimaryKeyResponse"
    );

    let messages = messages("services/v1/metadata.proto");
    assert_fields(&messages["GetPrimaryKeyRequest"], &[(1, "object")]);
    assert_fields(&messages["GetPrimaryKeyResponse"], &[(1, "columns")]);
    // 复合主键的顺序必须可表达：position 是主键内顺序，不是表内列序号。
    assert_fields(
        &messages["PrimaryKeyColumn"],
        &[(1, "name"), (2, "position")],
    );
}

#[test]
fn table_data_service_methods_are_frozen() {
    let set = descriptor_set();
    let file = set
        .file
        .iter()
        .find(|file| file.name() == "services/v1/table_data.proto")
        .expect("描述符含 table_data.proto");
    let service = file
        .service
        .iter()
        .find(|service| service.name() == "TableDataService")
        .expect("TableDataService 存在");
    let methods: Vec<(&str, &str, &str)> = service
        .method
        .iter()
        .map(|method| (method.name(), method.input_type(), method.output_type()))
        .collect();
    assert_eq!(
        methods,
        vec![
            (
                "BrowseTable",
                ".sqarvo.services.v1.BrowseTableRequest",
                ".sqarvo.services.v1.BrowseTableResponse"
            ),
            (
                "ApplyRowChanges",
                ".sqarvo.services.v1.ApplyRowChangesRequest",
                ".sqarvo.services.v1.ApplyRowChangesResponse"
            ),
        ]
    );
}

#[test]
fn table_data_enum_values_are_frozen() {
    let set = descriptor_set();
    let file = set
        .file
        .iter()
        .find(|file| file.name() == "services/v1/table_data.proto")
        .expect("描述符含 table_data.proto");

    let values = |name: &str| -> Vec<(i32, String)> {
        file.enum_type
            .iter()
            .find(|enumeration| enumeration.name() == name)
            .unwrap_or_else(|| panic!("缺少枚举 {name}"))
            .value
            .iter()
            .map(|value| (value.number(), value.name().to_owned()))
            .collect()
    };

    assert_eq!(
        values("RowOrder"),
        vec![
            (0, "ROW_ORDER_UNSPECIFIED".to_owned()),
            (1, "ROW_ORDER_PRIMARY_KEY_ASCENDING".to_owned()),
            (2, "ROW_ORDER_UNSTABLE".to_owned()),
        ]
    );
    assert_eq!(
        values("ReadOnlyReason"),
        vec![
            (0, "READ_ONLY_REASON_UNSPECIFIED".to_owned()),
            (1, "READ_ONLY_REASON_NO_PRIMARY_KEY".to_owned()),
            (2, "READ_ONLY_REASON_DRIVER_UNSUPPORTED".to_owned()),
        ]
    );
    assert_eq!(
        values("ChangeOutcome"),
        vec![
            (0, "CHANGE_OUTCOME_UNSPECIFIED".to_owned()),
            (1, "CHANGE_OUTCOME_APPLIED".to_owned()),
            (2, "CHANGE_OUTCOME_ROW_NOT_FOUND".to_owned()),
            (3, "CHANGE_OUTCOME_CONFLICT".to_owned()),
        ]
    );
}

#[test]
fn new_error_codes_map_to_expected_grpc_status() {
    let cases: [(Error, Code, ErrorCode); 6] = [
        (
            Error::failed_precondition(
                ErrorCode::TABLE_DATA_NO_PRIMARY_KEY,
                "该表没有主键，无法按行编辑",
            ),
            Code::FailedPrecondition,
            ErrorCode::TABLE_DATA_NO_PRIMARY_KEY,
        ),
        (
            Error::not_found(ErrorCode::TRANSACTION_NOT_FOUND, "事务不存在或已结束"),
            Code::NotFound,
            ErrorCode::TRANSACTION_NOT_FOUND,
        ),
        (
            Error::already_exists(ErrorCode::TRANSACTION_ALREADY_ACTIVE, "会话已有打开的事务"),
            Code::AlreadyExists,
            ErrorCode::TRANSACTION_ALREADY_ACTIVE,
        ),
        (
            Error::failed_precondition(
                ErrorCode::TABLE_DATA_ROWS_AFFECTED_MISMATCH,
                "影响行数与预期不符，数据可能已被修改",
            ),
            Code::FailedPrecondition,
            ErrorCode::TABLE_DATA_ROWS_AFFECTED_MISMATCH,
        ),
        (
            Error::not_found(ErrorCode::TABLE_DATA_ROW_DELETED, "该行已被删除"),
            Code::NotFound,
            ErrorCode::TABLE_DATA_ROW_DELETED,
        ),
        (
            Error::failed_precondition(
                ErrorCode::TABLE_DATA_CONFIRMATION_REQUIRED,
                "该操作预计影响 250 行，需要确认后重试",
            ),
            Code::FailedPrecondition,
            ErrorCode::TABLE_DATA_CONFIRMATION_REQUIRED,
        ),
    ];

    for (error, expected_code, expected_error_code) in cases {
        let status = to_status(error);
        assert_eq!(status.code(), expected_code);
        assert_eq!(
            status
                .metadata()
                .get(ERROR_CODE_METADATA_KEY)
                .and_then(|value| value.to_str().ok()),
            Some(expected_error_code.as_str())
        );
        // 文案必须可直接展示且不含内部细节。
        assert_ne!(status.message(), "");
        assert!(!status.message().contains("SELECT"));
        assert!(!status.message().contains("password"));
    }
}

#[test]
fn unsupported_capabilities_never_look_like_success() {
    // 未实现的驱动：能力声明为 false，调用返回 capability.unsupported。
    let status = to_status(Error::unsupported(
        ErrorCode::CAPABILITY_UNSUPPORTED,
        "该驱动未实现表数据行编辑",
    ));
    assert_eq!(status.code(), Code::Unimplemented);
    assert_eq!(
        status
            .metadata()
            .get(ERROR_CODE_METADATA_KEY)
            .and_then(|value| value.to_str().ok()),
        Some("capability.unsupported")
    );
}

/// 只实现必需方法的最小会话：验证新增能力默认「不支持」而不是「假装支持」。
struct MinimalSession;

#[tonic::async_trait]
impl DriverSession for MinimalSession {
    fn session_id(&self) -> sqarvo_common::SessionId {
        sqarvo_common::SessionId::new()
    }

    async fn execute(
        &self,
        _request: sqarvo_driver_api::ExecuteRequest,
    ) -> sqarvo_common::Result<ExecutionStart> {
        Err(Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            "未实现",
        ))
    }

    async fn fetch(
        &self,
        _query_id: &sqarvo_common::QueryId,
        _statement_index: u32,
        _cursor: Option<&str>,
        _max_rows: u32,
    ) -> sqarvo_common::Result<ResultBatch> {
        Err(Error::unsupported(
            ErrorCode::CAPABILITY_UNSUPPORTED,
            "未实现",
        ))
    }

    async fn cancel(&self, _query_id: &sqarvo_common::QueryId) -> sqarvo_common::Result<()> {
        Ok(())
    }

    async fn close(&self) -> sqarvo_common::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn minimal_session_defaults_to_unsupported() {
    let session = MinimalSession;
    assert!(
        session.table_data().is_none(),
        "未实现表数据的驱动必须返回 None，而不是空实现"
    );
    assert!(!session.supports_parameterized_statements());

    let error = session
        .execute_parameterized(ParameterizedExecuteRequest {
            query_id: sqarvo_common::QueryId::new(),
            sql: "UPDATE t SET v = ? WHERE id = ?".to_owned(),
            params: vec![Value::Int(1), Value::Int(2)],
            max_rows: None,
            timeout: None,
        })
        .await
        .expect_err("默认实现必须拒绝参数化执行");
    assert_eq!(error.code(), ErrorCode::CAPABILITY_UNSUPPORTED);
    assert_eq!(
        error.category(),
        sqarvo_common::ErrorCategory::Unsupported,
        "必须是 Unsupported 而不是执行失败"
    );

    // 事务默认实现同样保持「不支持」。
    let error = session
        .begin_transaction(sqarvo_driver_api::TransactionOptions {
            isolation: IsolationLevel::ReadCommitted,
            read_only: false,
        })
        .await
        .expect_err("默认实现必须拒绝事务");
    assert_eq!(error.code(), ErrorCode::CAPABILITY_UNSUPPORTED);
}

/// 假驱动：只用于固定游标与只读语义的形状，不是任何真实数据库的实现。
struct FakeTableData {
    /// 收到的 `page_token`，用于断言原样透传。
    seen_token: std::sync::Mutex<Option<String>>,
}

#[tonic::async_trait]
impl TableDataProvider for FakeTableData {
    fn supports_row_edits(&self) -> bool {
        false
    }

    async fn primary_key(
        &self,
        _object: &sqarvo_driver_api::ObjectRef,
    ) -> sqarvo_common::Result<PrimaryKey> {
        Ok(PrimaryKey::new(vec![
            PrimaryKeyColumn {
                name: "tenant_id".to_owned(),
                position: 1,
            },
            PrimaryKeyColumn {
                name: "order_id".to_owned(),
                position: 2,
            },
        ]))
    }

    async fn browse(
        &self,
        _object: &sqarvo_driver_api::ObjectRef,
        page: &sqarvo_driver_api::PageRequest,
    ) -> sqarvo_common::Result<TablePage> {
        self.seen_token
            .lock()
            .expect("锁可用")
            .clone_from(&page.page_token);
        let Some(token) = page.page_token.as_deref() else {
            return Ok(TablePage {
                columns: vec![ColumnMeta {
                    name: "id".to_owned(),
                    ordinal: 1,
                    column_type: ColumnType::Integer {
                        bits: 64,
                        unsigned: false,
                    },
                    nullable: false,
                    original_type_name: Some("bigint".to_owned()),
                }],
                rows: Vec::new(),
                next_page_token: Some("sqtoken-v1.eyJrZXkiOlsxXX0".to_owned()),
                has_more: true,
                read_only: true,
                read_only_reason: Some(ReadOnlyReason::DriverUnsupported),
                ordering: RowOrder::PrimaryKeyAscending,
            });
        };
        // 游标只能原样回传：篡改后必须显式拒绝，不能静默回到第一页。
        if token != "sqtoken-v1.eyJrZXkiOlsxXX0" {
            return Err(Error::invalid_argument(
                ErrorCode::TABLE_DATA_CURSOR_INVALID,
                "浏览游标非法或与目标表不匹配",
            ));
        }
        Ok(TablePage {
            columns: Vec::new(),
            rows: Vec::new(),
            next_page_token: None,
            has_more: false,
            read_only: true,
            read_only_reason: Some(ReadOnlyReason::NoPrimaryKey),
            ordering: RowOrder::Unstable,
        })
    }
}

#[tokio::test]
async fn browse_cursor_is_opaque_and_round_trips_verbatim() {
    let provider = FakeTableData {
        seen_token: std::sync::Mutex::new(None),
    };
    let object = sqarvo_driver_api::ObjectRef {
        session_id: sqarvo_common::SessionId::new(),
        qualified_name: sqarvo_driver_api::QualifiedName::new(
            Some("shop".to_owned()),
            None,
            "orders",
        ),
        kind: sqarvo_driver_api::ObjectKind::Table,
    };

    let first = provider
        .browse(&object, &sqarvo_driver_api::PageRequest::default())
        .await
        .expect("第一页");
    assert!(first.has_more);
    let cursor = first.next_page_token.clone().expect("第一页必须给出游标");
    // 不透明：客户端拿到的只是一个字符串，不含可解析的结构承诺。
    assert!(cursor.starts_with("sqtoken-v1."));
    assert_eq!(first.ordering, RowOrder::PrimaryKeyAscending);
    assert!(first.read_only);
    assert_eq!(
        first.read_only_reason,
        Some(ReadOnlyReason::DriverUnsupported),
        "驱动不支持行编辑时必须给出只读原因"
    );

    let second = provider
        .browse(
            &object,
            &sqarvo_driver_api::PageRequest {
                page_size: 100,
                page_token: Some(cursor.clone()),
            },
        )
        .await
        .expect("原样回传的游标可用");
    assert!(!second.has_more);
    assert!(second.next_page_token.is_none());
    assert_eq!(*provider.seen_token.lock().expect("锁可用"), Some(cursor));

    let error = provider
        .browse(
            &object,
            &sqarvo_driver_api::PageRequest {
                page_size: 100,
                page_token: Some("sqtoken-v1.tampered".to_owned()),
            },
        )
        .await
        .expect_err("篡改的游标必须被拒绝");
    assert_eq!(error.code(), ErrorCode::TABLE_DATA_CURSOR_INVALID);

    // 无主键表：浏览仍可用，但标注「无稳定排序」。
    let no_key = provider
        .browse(
            &object,
            &sqarvo_driver_api::PageRequest {
                page_size: 100,
                page_token: Some("sqtoken-v1.eyJrZXkiOlsxXX0".to_owned()),
            },
        )
        .await
        .expect("无主键表可浏览");
    assert_eq!(no_key.ordering, RowOrder::Unstable);
    assert_eq!(no_key.read_only_reason, Some(ReadOnlyReason::NoPrimaryKey));

    // 行编辑默认拒绝，而不是「尽力而为」。
    let error = provider
        .apply_row_changes(RowChangeRequest {
            object,
            changes: vec![RowChange::Delete {
                key: RowKey::new(vec![Value::Int(1), Value::Int(2)]),
                expected_rows_affected: 0,
            }],
        })
        .await
        .expect_err("未实现行编辑的驱动必须拒绝");
    assert_eq!(error.code(), ErrorCode::CAPABILITY_UNSUPPORTED);
}
