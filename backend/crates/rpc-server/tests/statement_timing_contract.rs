//! 逐语句耗时契约测试（#238）：`StatementSummary.elapsed_millis = 6`。
//!
//! 与 `contract_v022.rs` / `contract_v023.rs` 同一套断言风格：直接读 `build.rs` 生成的
//! `FileDescriptorSet`，不依赖服务实现，再补一条线格式（wire format）兼容断言。
//!
//! **这些断言为什么会变红**：
//!
//! - `statement_summary_fields_are_frozen`：字段号/名字有序列表一旦变动就失败——
//!   把 `elapsed_millis` 插到 5 号位、改名为 `duration`、把 6 号挪作他用都会红；
//! - `elapsed_millis_is_a_fresh_uint64_field`：锁类型（`uint64`）与标签（singular），
//!   防止有人换成 `optional int32` / `google.protobuf.Duration` 之类的不同语义；
//! - `old_payload_without_elapsed_millis_still_decodes`：「老服务端发的、不含 6 号的
//!   payload 新客户端照样解析」；
//! - `unmeasured_statement_encodes_identically_to_old_payload`：`elapsed_millis = 0`
//!   时编码结果与老 payload 逐字节相同（proto3 默认值不上线），即老客户端无感。

use prost::Message;
use prost_types::field_descriptor_proto::{Label, Type};
use prost_types::{DescriptorProto, FieldDescriptorProto, FileDescriptorSet};

const DESCRIPTOR_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/sqarvo-descriptor.bin"));
const QUERY_PROTO: &str = "services/v1/query.proto";

fn descriptor_set() -> FileDescriptorSet {
    FileDescriptorSet::decode(DESCRIPTOR_BYTES).expect("描述符可解析")
}

fn statement_summary_descriptor() -> DescriptorProto {
    descriptor_set()
        .file
        .iter()
        .find(|file| file.name() == QUERY_PROTO)
        .unwrap_or_else(|| panic!("描述符中缺少 {QUERY_PROTO}"))
        .message_type
        .iter()
        .find(|message| message.name() == "StatementSummary")
        .expect("缺少 StatementSummary")
        .clone()
}

fn field<'a>(message: &'a DescriptorProto, name: &str) -> &'a FieldDescriptorProto {
    message
        .field
        .iter()
        .find(|field| field.name() == name)
        .unwrap_or_else(|| panic!("{} 缺少字段 {name}", message.name()))
}

/// 字段编号与名字的完整有序列表：字段号发布后不得复用、不得重排。
#[test]
fn statement_summary_fields_are_frozen() {
    let summary = statement_summary_descriptor();
    let actual: Vec<(i32, &str)> = summary
        .field
        .iter()
        .map(|field| (field.number(), field.name()))
        .collect();
    assert_eq!(
        actual,
        vec![
            (1, "statement_index"),
            (2, "sql_preview"),
            (3, "result_meta"),
            (4, "rows_affected"),
            (5, "has_result_set"),
            // #238：纯新增，号 6 之后不得复用。
            (6, "elapsed_millis"),
        ],
        "StatementSummary 的字段编号/名字发生变化：6 号已发布，不得复用或重排"
    );
}

/// 6 号是新字段：必须是 `uint64` 的 singular 字段，语义是「毫秒」。
#[test]
fn elapsed_millis_is_a_fresh_uint64_field() {
    let summary = statement_summary_descriptor();
    let elapsed = field(&summary, "elapsed_millis");
    assert_eq!(elapsed.number(), 6, "elapsed_millis 必须是 6 号");
    assert_eq!(
        elapsed.r#type(),
        Type::Uint64,
        "毫秒数必须是无符号整数：改用 Duration 消息会改变 0 的语义与兼容路径"
    );
    assert_eq!(
        elapsed.label(),
        Label::Optional,
        "proto3 的 singular 字段（非 repeated，也不需要 optional presence）"
    );
    assert!(
        elapsed.default_value().is_empty(),
        "不设自定义默认值：未测量就靠 proto3 的 0"
    );
}

/// proto 线格式的 wire type：0 = varint，2 = 长度前缀。
const WIRE_VARINT: u64 = 0;
const WIRE_LEN: u64 = 2;

/// 字段键：`field_number << 3 | wire_type`（proto 编码规范）。
fn key(field: u64, wire_type: u64) -> u64 {
    (field << 3) | wire_type
}

// 低位 7 位（<= 0x7f）与终止字节（< 0x80）都装得下 `u8`，这里的截断是有意且安全的。
#[allow(clippy::cast_possible_truncation)]
fn push_varint(buf: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        buf.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    buf.push(value as u8);
}

/// 手工拼一个「#238 之前」的 `StatementSummary` 线格式：只有 1/2/4/5 号，
/// 没有 6 号。新客户端（含 `elapsed_millis`）解析它必须成功且得到 0。
#[test]
fn old_payload_without_elapsed_millis_still_decodes() {
    use sqarvo_rpc_server::proto::services::v1::StatementSummary as ProtoStatementSummary;

    let mut payload = Vec::new();
    // 1: statement_index = 3
    push_varint(&mut payload, key(1, WIRE_VARINT));
    push_varint(&mut payload, 3);
    // 2: sql_preview = "SELECT 1"
    push_varint(&mut payload, key(2, WIRE_LEN));
    push_varint(&mut payload, 8);
    payload.extend_from_slice(b"SELECT 1");
    // 4: rows_affected = 7
    push_varint(&mut payload, key(4, WIRE_VARINT));
    push_varint(&mut payload, 7);
    // 5: has_result_set = true
    push_varint(&mut payload, key(5, WIRE_VARINT));
    push_varint(&mut payload, 1);

    let decoded = ProtoStatementSummary::decode(payload.as_slice())
        .expect("不含 6 号的旧 payload 必须能解析（proto3 未知/缺失字段都不报错）");
    assert_eq!(decoded.statement_index, 3);
    assert_eq!(decoded.sql_preview, "SELECT 1");
    assert_eq!(decoded.rows_affected, 7);
    assert!(decoded.has_result_set);
    assert_eq!(
        decoded.elapsed_millis, 0,
        "缺字段的默认值就是 0，即「未测量」；客户端不得把它当 0 秒"
    );
}

/// 反向兼容：`elapsed_millis = 0`（未测量）时编码结果与老服务端发的 payload 逐字节相同，
/// 老客户端读新服务端的响应看不到任何新字段。
#[test]
fn unmeasured_statement_encodes_identically_to_old_payload() {
    use sqarvo_rpc_server::proto::services::v1::StatementSummary as ProtoStatementSummary;

    // 老服务端会发的字节：1/2/4/5 号，6 号根本不出现。
    let mut old_payload = Vec::new();
    push_varint(&mut old_payload, key(1, WIRE_VARINT));
    push_varint(&mut old_payload, 3);
    push_varint(&mut old_payload, key(2, WIRE_LEN));
    push_varint(&mut old_payload, 8);
    old_payload.extend_from_slice(b"SELECT 1");
    push_varint(&mut old_payload, key(4, WIRE_VARINT));
    push_varint(&mut old_payload, 7);
    push_varint(&mut old_payload, key(5, WIRE_VARINT));
    push_varint(&mut old_payload, 1);

    let same_statement = ProtoStatementSummary {
        statement_index: 3,
        sql_preview: "SELECT 1".to_owned(),
        result_meta: None,
        rows_affected: 7,
        has_result_set: true,
        elapsed_millis: 0,
    }
    .encode_to_vec();
    assert_eq!(
        same_statement, old_payload,
        "未测量（0）不得上线任何字节：老客户端必须读到与 #238 之前完全相同的消息"
    );

    // 有测量值时才出现 6 号（键 = 6 << 3 = 0x30），且能往返。
    let measured = ProtoStatementSummary {
        elapsed_millis: 42,
        ..ProtoStatementSummary::default()
    }
    .encode_to_vec();
    assert!(
        measured.contains(&0x30),
        "有测量值时字段 6 的键必须上线，实际字节 {measured:?}"
    );
    let decoded = ProtoStatementSummary::decode(measured.as_slice()).expect("可解析");
    assert_eq!(decoded.elapsed_millis, 42);
}
