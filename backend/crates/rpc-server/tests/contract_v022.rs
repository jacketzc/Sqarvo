//! v0.2.2 契约测试（#81）：proto 字段号、枚举取值与错误码 → gRPC 状态映射。
//!
//! 与 `table_data_contract.rs`（ADR 0017）同一套断言风格：直接读 `build.rs` 生成的
//! `FileDescriptorSet`，不依赖服务实现。
//!
//! **这些断言为什么会变红**（PR 里写了手工验证过程）：
//!
//! - `*_field_numbers_are_frozen` 断言的是「编号 + 名字」的完整有序列表：把
//!   `InsertRow.values` 从 1 改成 9、或把 `GetDdlResponse.source` 插到 2 号位，
//!   断言立刻失败；删除字段同理（不只是「少一个」，是整行比对）；
//! - `v022_error_codes_map_to_grpc_status` 断言每个新错误码 → gRPC code + trailing
//!   metadata：把 `table_data.unique_violation` 的分类从 `AlreadyExists` 改成别的，
//!   或删掉 `to_status` 里的 `PermissionDenied` 分支，都会失败。

use std::collections::BTreeMap;

use prost::Message;
use prost_types::{DescriptorProto, FieldDescriptorProto, FileDescriptorSet};
use sqarvo_common::{Error, ErrorCategory, ErrorCode};
use sqarvo_driver_api::{
    column_not_insertable, insert_permission_denied, metadata_permission_denied,
    not_null_violation, not_null_without_default, object_kind_unsupported, object_not_found,
    unique_violation, unsupported_metadata_filtering, unsupported_row_insertion,
};
use sqarvo_rpc_server::error::{ERROR_CODE_METADATA_KEY, to_status};
use tonic::Code;

const DESCRIPTOR_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/sqarvo-descriptor.bin"));

fn descriptor_set() -> FileDescriptorSet {
    FileDescriptorSet::decode(DESCRIPTOR_BYTES).expect("描述符可解析")
}

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

fn enum_values(file_name: &str, enum_name: &str) -> Vec<(i32, String)> {
    let set = descriptor_set();
    let file = set
        .file
        .iter()
        .find(|file| file.name() == file_name)
        .unwrap_or_else(|| panic!("描述符中缺少 {file_name}"));
    file.enum_type
        .iter()
        .find(|enumeration| enumeration.name() == enum_name)
        .unwrap_or_else(|| panic!("缺少枚举 {enum_name}"))
        .value
        .iter()
        .map(|value| (value.number(), value.name().to_owned()))
        .collect()
}

/// 新 message 的字段号从 1 开始，且既有 message 只追加。
#[test]
fn metadata_v022_field_numbers_are_frozen() {
    let metadata = messages("services/v1/metadata.proto");

    // 既有字段 1/2 不动，过滤只能是 3。
    assert_fields(
        &metadata["ListNodesRequest"],
        &[(1, "parent"), (2, "page"), (3, "filter")],
    );
    assert_fields(&metadata["NodeFilter"], &[(1, "name"), (2, "kinds")]);
    // 新 message 字段从 1 开始（不是 3、不是 7）。
    assert_fields(
        &metadata["ObjectNameFilter"],
        &[(1, "contains"), (2, "exact")],
    );

    // 「包含 / 精确匹配」必须是 oneof：同时给两个值必须在协议层就互斥，
    // 不能靠服务端事后判断「两个都给了，用哪个？」。
    let oneofs: Vec<&str> = metadata["ObjectNameFilter"]
        .oneof_decl
        .iter()
        .map(prost_types::OneofDescriptorProto::name)
        .collect();
    assert_eq!(oneofs, vec!["match"], "名称过滤必须是 oneof match");
    for name in ["contains", "exact"] {
        assert_eq!(
            field(&metadata["ObjectNameFilter"], name).oneof_index,
            Some(0),
            "{name} 必须属于 oneof match"
        );
    }

    // DDL：原文/拼装/来源说明只能在 1、2 之后追加。
    assert_fields(
        &metadata["GetDdlResponse"],
        &[
            (1, "ddl"),
            (2, "exact"),
            (3, "source"),
            (4, "source_detail"),
            (5, "generator_version"),
        ],
    );
    assert_eq!(
        field(&metadata["GetDdlResponse"], "source").type_name(),
        ".sqarvo.services.v1.DdlSource"
    );

    assert_eq!(
        enum_values("services/v1/metadata.proto", "DdlSource"),
        vec![
            (0, "DDL_SOURCE_UNSPECIFIED".to_owned()),
            (1, "DDL_SOURCE_DATABASE".to_owned()),
            (2, "DDL_SOURCE_SYNTHESIZED".to_owned()),
        ],
        "DDL 来源枚举只追加、不复用数字"
    );

    // 上层层级（Catalog / Schema / Index / Sequence）沿用既有取值，不得重排。
    assert_eq!(
        enum_values("services/v1/metadata.proto", "ObjectKind"),
        vec![
            (0, "OBJECT_KIND_UNSPECIFIED".to_owned()),
            (1, "OBJECT_KIND_CATALOG".to_owned()),
            (2, "OBJECT_KIND_SCHEMA".to_owned()),
            (3, "OBJECT_KIND_TABLE".to_owned()),
            (4, "OBJECT_KIND_VIEW".to_owned()),
            (5, "OBJECT_KIND_MATERIALIZED_VIEW".to_owned()),
            (6, "OBJECT_KIND_COLUMN".to_owned()),
            (7, "OBJECT_KIND_INDEX".to_owned()),
            (8, "OBJECT_KIND_SEQUENCE".to_owned()),
            (9, "OBJECT_KIND_FUNCTION".to_owned()),
            (10, "OBJECT_KIND_PROCEDURE".to_owned()),
            (11, "OBJECT_KIND_TRIGGER".to_owned()),
            (12, "OBJECT_KIND_TYPE".to_owned()),
            (13, "OBJECT_KIND_OTHER".to_owned()),
        ]
    );
}

/// 插入行：新 message 字段从 1 开始；`acknowledge_risk` 是显式布尔。
#[test]
fn insert_rows_field_numbers_are_frozen() {
    let table_data = messages("services/v1/table_data.proto");

    assert_fields(&table_data["InsertRow"], &[(1, "values")]);
    assert_eq!(
        field(&table_data["InsertRow"], "values").type_name(),
        ".sqarvo.services.v1.ColumnChange",
        "插入值复用行变更的列值类型，不得退化成字符串或新造重复类型"
    );

    assert_fields(
        &table_data["InsertRowsRequest"],
        &[
            (1, "session_id"),
            (2, "object"),
            (3, "transaction_id"),
            (4, "rows"),
            (5, "acknowledge_risk"),
        ],
    );
    assert_eq!(
        field(&table_data["InsertRowsRequest"], "acknowledge_risk").r#type(),
        prost_types::field_descriptor_proto::Type::Bool,
        "风险确认必须是显式布尔，不能靠「有没有行」推断"
    );

    assert_fields(
        &table_data["InsertRowResult"],
        &[
            (1, "row_index"),
            (2, "outcome"),
            (3, "key"),
            (4, "rows_affected"),
            (5, "error_code"),
            (6, "message"),
        ],
    );
    // 主键回传复用 `RowKey`（复合主键按主键顺序给出全部列）。
    assert_eq!(
        field(&table_data["InsertRowResult"], "key").type_name(),
        ".sqarvo.services.v1.RowKey"
    );

    assert_fields(
        &table_data["InsertRowsResponse"],
        &[
            (1, "results"),
            (2, "total_rows_affected"),
            (3, "clamped"),
            (4, "deferred_rows"),
            (5, "rolled_back"),
            (6, "inserted_rows"),
        ],
    );

    assert_eq!(
        enum_values("services/v1/table_data.proto", "InsertOutcome"),
        vec![
            (0, "INSERT_OUTCOME_UNSPECIFIED".to_owned()),
            (1, "INSERT_OUTCOME_INSERTED".to_owned()),
            (2, "INSERT_OUTCOME_FAILED".to_owned()),
        ]
    );

    // 插入请求不得携带 SQL 文本字段（与 ApplyRowChanges 同一条底线）。
    let set = descriptor_set();
    let file = set
        .file
        .iter()
        .find(|file| file.name() == "services/v1/table_data.proto")
        .expect("描述符含 table_data.proto");
    for message in &file.message_type {
        if !message.name().contains("Insert") {
            continue;
        }
        assert!(
            message
                .field
                .iter()
                .all(|field| !field.name().contains("sql")),
            "{} 不得携带 SQL",
            message.name()
        );
    }
}

/// 新 RPC 只能追加在 `TableDataService` 末尾。
#[test]
fn insert_rows_rpc_is_appended_only() {
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
            (
                "CancelRowChanges",
                ".sqarvo.services.v1.CancelRowChangesRequest",
                ".sqarvo.common.v1.Empty"
            ),
            // #81：插入新行，追加在末尾（单行 = rows 只有一条）。
            (
                "InsertRows",
                ".sqarvo.services.v1.InsertRowsRequest",
                ".sqarvo.services.v1.InsertRowsResponse"
            ),
        ]
    );
}

/// 新错误码 → gRPC 状态码 + trailing metadata 的映射。
#[test]
fn v022_error_codes_map_to_grpc_status() {
    let cases: Vec<(Error, Code, &str)> = vec![
        (
            unique_violation("email"),
            Code::AlreadyExists,
            "table_data.unique_violation",
        ),
        (
            not_null_violation("email"),
            Code::InvalidArgument,
            "table_data.not_null_violation",
        ),
        (
            not_null_without_default("email"),
            Code::InvalidArgument,
            "table_data.not_null_without_default",
        ),
        (
            column_not_insertable("created_at"),
            Code::InvalidArgument,
            "table_data.column_not_insertable",
        ),
        (
            insert_permission_denied("orders"),
            Code::PermissionDenied,
            "table_data.permission_denied",
        ),
        (
            object_not_found("shop.orders"),
            Code::NotFound,
            "metadata.object_not_found",
        ),
        (
            object_kind_unsupported(&sqarvo_driver_api::ObjectKind::Column, "GetDdl"),
            Code::InvalidArgument,
            "metadata.object_kind_unsupported",
        ),
        (
            metadata_permission_denied("shop.orders"),
            Code::PermissionDenied,
            "metadata.permission_denied",
        ),
        (
            sqarvo_driver_api::filter_invalid("过滤器里重复的对象种类：table"),
            Code::InvalidArgument,
            "metadata.filter_invalid",
        ),
    ];

    for (error, expected_code, expected_error_code) in cases {
        let status = to_status(error);
        assert_eq!(
            status.code(),
            expected_code,
            "{expected_error_code} 的 gRPC 状态码漂移"
        );
        assert_eq!(
            status
                .metadata()
                .get(ERROR_CODE_METADATA_KEY)
                .and_then(|value| value.to_str().ok()),
            Some(expected_error_code),
            "{expected_error_code} 未写入 trailing metadata"
        );
        // 文案可直接展示：不得含 SQL、参数值、凭据。
        assert!(!status.message().contains("INSERT "));
        assert!(!status.message().contains("password"));
    }
}

/// 「未实现」必须映射成 `UNIMPLEMENTED`（与「执行失败」严格区分）。
#[test]
fn unimplemented_paths_map_to_unimplemented() {
    for error in [
        unsupported_row_insertion(),
        unsupported_metadata_filtering(),
    ] {
        assert_eq!(error.code(), ErrorCode::CAPABILITY_UNSUPPORTED);
        let status = to_status(error);
        assert_eq!(status.code(), Code::Unimplemented);
        assert_eq!(
            status
                .metadata()
                .get(ERROR_CODE_METADATA_KEY)
                .and_then(|value| value.to_str().ok()),
            Some("capability.unsupported")
        );
    }

    // 分类表本身也不得被改动：Unsupported 只能落到 UNIMPLEMENTED。
    assert_eq!(
        to_status(Error::new(
            ErrorCode::INTERNAL,
            ErrorCategory::Unsupported,
            "x"
        ))
        .code(),
        Code::Unimplemented
    );
}
