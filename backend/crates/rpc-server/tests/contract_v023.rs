//! v0.2.3 契约测试（#109）：`BrowseTable` 的 offset / 排序 / 筛选 / 总数增量。
//!
//! 与 `contract_v022.rs` 同一套断言风格：直接读 `build.rs` 生成的
//! `FileDescriptorSet`，不依赖服务实现。
//!
//! **这些断言为什么会变红**：
//!
//! - `browse_table_field_numbers_are_frozen` / `browse_table_response_...` 断言的是
//!   「编号 + 名字」的完整有序列表：把 `offset` 从 4 改成 3（复用已发布的
//!   `page_token` 号）、把 `total_rows` 插到 7 号位、删除任一旧字段都会失败；
//! - `sort_filter_and_total_enums_are_frozen` 断言枚举取值：改动
//!   `ROW_ORDER_REQUESTED_SORT = 3` 或 `TOTAL_KIND_EXACT = 2` 会立刻变红；
//! - `filter_condition_is_nested_and_appended` 锁住 `Filter.Condition` 的嵌套结构与
//!   字段号：把 `value` 改成字符串、或把 `Filter` 摊平成顶层 message 都会失败。

use std::collections::BTreeMap;

use prost::Message;
use prost_types::field_descriptor_proto::Type;
use prost_types::{DescriptorProto, FieldDescriptorProto, FileDescriptorSet};

const DESCRIPTOR_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/sqarvo-descriptor.bin"));
const TABLE_DATA_PROTO: &str = "services/v1/table_data.proto";

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

/// 请求只追加：旧字段 1/2/3 原样保留，新字段恰好是 4/5/6/7。
#[test]
fn browse_table_request_field_numbers_are_frozen() {
    let table_data = messages(TABLE_DATA_PROTO);
    assert_fields(
        &table_data["BrowseTableRequest"],
        &[
            (1, "object"),
            (2, "page_size"),
            (3, "page_token"),
            (4, "offset"),
            (5, "sort"),
            (6, "filter"),
            (7, "include_total"),
            // #127：只追加，8 / 9 留空未用（按 Issue 约定取 10）。
            (10, "timeout_millis"),
        ],
    );

    let request = &table_data["BrowseTableRequest"];
    assert_eq!(
        field(request, "offset").r#type(),
        Type::Uint64,
        "offset 是无符号行偏移，不能改成 int32/string"
    );
    assert_eq!(
        field(request, "sort").label(),
        prost_types::field_descriptor_proto::Label::Repeated,
        "sort 必须可重复：多列排序按给出顺序生效"
    );
    assert_eq!(
        field(request, "sort").type_name(),
        ".sqarvo.services.v1.SortKey"
    );
    assert_eq!(
        field(request, "filter").type_name(),
        ".sqarvo.services.v1.Filter"
    );
    assert_eq!(
        field(request, "include_total").r#type(),
        Type::Bool,
        "是否请求总数必须是显式布尔，不能靠 total_rows 是否为 0 推断"
    );
    // #127：计数的超时上界。必须是毫秒整数（0 = 服务端默认值），
    // 不是字符串、不是布尔——「有没有超时」不能靠别的字段推断。
    assert_eq!(
        field(request, "timeout_millis").r#type(),
        Type::Uint32,
        "timeout_millis 是无符号毫秒数，0 表示服务端默认值"
    );
    assert_eq!(
        field(request, "timeout_millis").label(),
        prost_types::field_descriptor_proto::Label::Optional,
        "timeout_millis 是单值字段（proto3 标量），不得改成 repeated"
    );
}

/// 响应只追加：旧字段 1-7 不动，新字段恰好是 8/9。
#[test]
fn browse_table_response_field_numbers_are_frozen() {
    let table_data = messages(TABLE_DATA_PROTO);
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
            (8, "total_rows"),
            (9, "total_kind"),
            // #127：只追加，10 留空未用（按 Issue 约定取 11）。
            (11, "total_skipped"),
        ],
    );

    let response = &table_data["BrowseTableResponse"];
    assert_eq!(field(response, "total_rows").r#type(), Type::Uint64);
    assert_eq!(
        field(response, "total_kind").type_name(),
        ".sqarvo.services.v1.TotalKind",
        "总数三态必须由独立枚举表达，不能用 total_rows 是否缺省推断"
    );
    // #127：「请求了但没统计完」必须是独立布尔，不能塞进 total_kind 的新枚举值
    // （那会让旧客户端把「跳过」读成 UNKNOWN 以外的语义），也不能用 total_rows = 0 表达。
    assert_eq!(
        field(response, "total_skipped").r#type(),
        Type::Bool,
        "total_skipped 必须与「没请求总数」可区分，且不得占用 TotalKind 的取值"
    );
}

/// 新 message 的字段号从 1 开始；`Filter.Condition` 是嵌套类型，值复用 `ColumnValue`。
#[test]
fn sort_filter_and_total_messages_are_appended() {
    let table_data = messages(TABLE_DATA_PROTO);

    assert_fields(&table_data["SortKey"], &[(1, "column"), (2, "direction")]);
    assert_eq!(
        field(&table_data["SortKey"], "direction").type_name(),
        ".sqarvo.services.v1.SortDirection"
    );

    assert_fields(&table_data["Filter"], &[(1, "conditions")]);
    let conditions = field(&table_data["Filter"], "conditions");
    assert_eq!(
        conditions.label(),
        prost_types::field_descriptor_proto::Label::Repeated,
        "条件列表必须可重复（v0.2.3 全部 AND）"
    );
    assert_eq!(
        conditions.type_name(),
        ".sqarvo.services.v1.Filter.Condition",
        "Condition 是 Filter 的嵌套类型，不得摊平成顶层 message"
    );

    let filter = &table_data["Filter"];
    let condition = filter
        .nested_type
        .iter()
        .find(|nested| nested.name() == "Condition")
        .expect("Filter 必须内嵌 Condition");
    assert_fields(condition, &[(1, "column"), (2, "operator"), (3, "value")]);
    assert_eq!(
        field(condition, "operator").type_name(),
        ".sqarvo.services.v1.FilterOperator"
    );
    assert_eq!(
        field(condition, "value").type_name(),
        ".sqarvo.common.v1.ColumnValue",
        "筛选值复用强类型 ColumnValue（参数化绑定），不得退化成字符串"
    );
}

/// 枚举取值只追加：`RowOrder` 新值恰好是 3，旧值（0/1/2）不动。
#[test]
fn sort_filter_and_total_enums_are_frozen() {
    assert_eq!(
        enum_values(TABLE_DATA_PROTO, "RowOrder"),
        vec![
            (0, "ROW_ORDER_UNSPECIFIED".to_owned()),
            (1, "ROW_ORDER_PRIMARY_KEY_ASCENDING".to_owned()),
            (2, "ROW_ORDER_UNSTABLE".to_owned()),
            (3, "ROW_ORDER_REQUESTED_SORT".to_owned()),
        ],
        "ROW_ORDER_REQUESTED_SORT 必须是追加的 3，不得重排或复用旧值"
    );

    assert_eq!(
        enum_values(TABLE_DATA_PROTO, "SortDirection"),
        vec![
            (0, "SORT_DIRECTION_UNSPECIFIED".to_owned()),
            (1, "SORT_DIRECTION_ASC".to_owned()),
            (2, "SORT_DIRECTION_DESC".to_owned()),
        ]
    );

    assert_eq!(
        enum_values(TABLE_DATA_PROTO, "FilterOperator"),
        vec![
            (0, "FILTER_OPERATOR_UNSPECIFIED".to_owned()),
            (1, "FILTER_OPERATOR_EQ".to_owned()),
            (2, "FILTER_OPERATOR_NE".to_owned()),
            (3, "FILTER_OPERATOR_LT".to_owned()),
            (4, "FILTER_OPERATOR_LE".to_owned()),
            (5, "FILTER_OPERATOR_GT".to_owned()),
            (6, "FILTER_OPERATOR_GE".to_owned()),
            (7, "FILTER_OPERATOR_LIKE".to_owned()),
            (8, "FILTER_OPERATOR_IS_NULL".to_owned()),
            (9, "FILTER_OPERATOR_IS_NOT_NULL".to_owned()),
        ],
        "筛选算子只追加：旧客户端读到未知值时应当保守拒绝，而不是猜一个算子"
    );

    assert_eq!(
        enum_values(TABLE_DATA_PROTO, "TotalKind"),
        vec![
            (0, "TOTAL_KIND_UNKNOWN".to_owned()),
            (1, "TOTAL_KIND_ESTIMATE".to_owned()),
            (2, "TOTAL_KIND_EXACT".to_owned()),
        ],
        "总数三态必须显式：UNKNOWN / ESTIMATE / EXACT"
    );
}

/// 新字段不得携带 SQL 文本：排序与筛选只能给列名、方向、算子与参数化值。
#[test]
fn browse_increment_carries_no_sql_text() {
    let table_data = messages(TABLE_DATA_PROTO);
    for message in ["BrowseTableRequest", "SortKey", "Filter"] {
        assert!(
            table_data[message]
                .field
                .iter()
                .all(|field| !field.name().contains("sql")),
            "{message} 不得携带 SQL 文本"
        );
    }
    let condition = table_data["Filter"]
        .nested_type
        .iter()
        .find(|nested| nested.name() == "Condition")
        .expect("Condition 存在");
    assert!(
        condition
            .field
            .iter()
            .all(|field| !field.name().contains("sql")),
        "Filter.Condition 不得携带 SQL 文本"
    );
}
