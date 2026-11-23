//! MCP Resources（#210 实现；V3.1 §7）。
//!
//! 四个资源与内容契约、RFC 3986 路径段编解码纪律（每段独立 encode；解码一次、
//! 校验合法性、**解码后才做授权**）都写在 Issue #210 的验收里；本模块只允许
//! #210 触碰，授权检查必须经由 [`crate::access`]，禁止绕过。
//!
//! # 四类资源
//!
//! | URI | 内容 |
//! |---|---|
//! | `database://connections` | 已授权连接概览（`alias` / `driver_id` / `connection_status`） |
//! | `database://{connection_alias}/{database}/overview` | 库级摘要（驱动、版本、能力、表/视图计数） |
//! | `database://{connection_alias}/{database}/schema` | 对象目录（表/视图名 + 种类 + 注释） |
//! | `database://{connection_alias}/{database}/tables/{table}` | 单表结构（列定义 + 主键） |
//!
//! 四者字段**互不重复**：overview 只给计数不给名字，schema 只给名字不给列明细，
//! tables 只给单表列定义。这条分工由 `tests/resources_uri.rs` 的字段清单测试钉死。
//!
//! # URI 编码契约（V3.1 §7.1）
//!
//! 1. 先按原始 `/` 拆分路径段，**再**对每段独立 percent-decode；
//! 2. 只解码一次：解码失败（非法 `%` 序列 / 非法 UTF-8）立即拒绝；
//! 3. 解码结果若仍含合法的 `%XX` 三元组，按**双重编码**拒绝（否则「解码一次」与
//!    「解码两次」会给出两个不同的标识符，把编码歧义变成权限绕过的通道）；
//! 4. 解码完成之后才做 alias / database / table 的授权与存在性判断；此后不再解码。
//!
//! 编码侧只保留 RFC 3986 unreserved（`A-Z a-z 0-9 - . _ ~`），其余字节一律
//! `%XX`（大写十六进制）。`/`、`%`、空格、中文都会被编码，因此「表名里有 `/`」
//! 不会变成两个路径段。
//!
//! # 错误语义（二选一，已钉死）
//!
//! - **URI 自身不合法**（scheme 不符、段数不符、空段、非法 `%`、非法 UTF-8、
//!   双重编码）→ `mcp.invalid_argument` → JSON-RPC `-32602`；
//! - **资源不存在或未授权**（alias 未知/未启用、库/表不存在、驱动报错）→
//!   `mcp.connection_not_found` / `mcp.database_not_found` / `mcp.table_not_found`…
//!   → JSON-RPC `-32002`；授权拒绝（alias 存在但 `enabled: false`）也归一化成
//!   `-32002`，不向 Agent 泄漏「这个 alias 在配置里存在」。
//!
//! # 输出规模
//!
//! 对象目录受驱动单页上限（[`sqarvo_driver_api::MAX_PAGE_SIZE`] = 500）约束，
//! 超出时 `truncated=true` 并给出 `guidance`（指向 `list_tables` 工具）。整体
//! JSON 超过生效限额 `max_result_bytes`（默认 1 MiB）时返回 `mcp.result_too_large`
//! 而不是交付半截结构——截一半的 JSON 对 Agent 毫无用处。
//!
//! # 与工具的关系
//!
//! 资源**不受** `enabled_tools` 管辖：那是 V3.1 §3.1 为「工具」定义的开关，
//! 表资源与 `describe_table` 共用列元数据读取、映射与序列化类型
//! （[`ColumnDetail`] / [`PrimaryKeyColumnView`]），字段形状由等价性测试锁定。

use std::collections::BTreeMap;

use rmcp::model::{Resource, ResourceTemplate};
use serde::Serialize;
use sqarvo_common::{Error, ErrorCategory, ErrorCode, Result};
use sqarvo_connection::SessionHandle;
use sqarvo_driver_api::{
    MAX_PAGE_SIZE, ObjectFilter, ObjectKind, ObjectRef, PageRequest, QualifiedName,
};
use tokio_util::sync::CancellationToken;

use crate::access::AccessService;
use crate::tools::schema::{
    ColumnDetail, PrimaryKeyColumnView, primary_key_columns, read_table_columns,
};

/// 资源 URI 的 scheme 前缀。
const SCHEME_PREFIX: &str = "database://";

/// 静态资源：已授权连接清单。
pub const CONNECTIONS_URI: &str = "database://connections";

/// `database://{connection_alias}/{database}/overview` 模板。
pub const OVERVIEW_TEMPLATE: &str = "database://{connection_alias}/{database}/overview";

/// `database://{connection_alias}/{database}/schema` 模板。
pub const SCHEMA_TEMPLATE: &str = "database://{connection_alias}/{database}/schema";

/// `database://{connection_alias}/{database}/tables/{table}` 模板。
pub const TABLE_TEMPLATE: &str = "database://{connection_alias}/{database}/tables/{table}";

/// 所有资源的 MIME 类型：结构化 JSON。
pub const MIME_TYPE: &str = "application/json";

/// 超过单页上限时的分页指引（V3.1 §7.2「对大型结构控制响应大小」）。
const PAGINATION_GUIDANCE: &str =
    "对象数量超过单页上限（500），本资源只列出当前页；完整清单请用 list_tables 工具按页读取";

// ---------------------------------------------------------------------------
// URI 编解码
// ---------------------------------------------------------------------------

/// 路径段编码：RFC 3986 unreserved 之外一律 `%XX`（大写）。
#[must_use]
pub fn encode_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            // 大写十六进制：同一标识符只有一种编码形式，便于比较与断言。
            encoded.push('%');
            encoded.push(hex_digit(byte >> 4));
            encoded.push(hex_digit(byte & 0x0F));
        }
    }
    encoded
}

/// 路径段解码：只解码一次，任何非法形态都拒绝。
///
/// 拒绝条件（顺序即检查顺序）：
/// 1. `%` 后不足两位十六进制；
/// 2. 解码后的字节不是合法 UTF-8；
/// 3. 解码后仍含合法 `%XX` 三元组（双重编码）。
///
/// 未编码的原始 UTF-8（客户端直接塞进 URI 的中文等）按字面接受：它已经是标识符本身。
pub fn decode_segment(raw: &str) -> Result<String> {
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = bytes.get(index + 1).copied().and_then(hex_value);
            let low = bytes.get(index + 2).copied().and_then(hex_value);
            let (Some(high), Some(low)) = (high, low) else {
                return Err(uri_error(
                    "URI 路径段的百分号编码不合法：% 后必须是两位十六进制数字",
                ));
            };
            decoded.push(high * 16 + low);
            index += 3;
            continue;
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    let decoded =
        String::from_utf8(decoded).map_err(|_| uri_error("URI 路径段解码后不是合法 UTF-8"))?;
    if contains_percent_triplet(&decoded) {
        return Err(uri_error(
            "URI 路径段疑似双重编码（解码后仍含 %XX 序列）；每个路径段只允许编码一次",
        ));
    }
    Ok(decoded)
}

/// 解码后是否仍含 `%` + 两位十六进制（= 还能再解一次）。
fn contains_percent_triplet(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && bytes.get(index + 1).copied().and_then(hex_value).is_some()
            && bytes.get(index + 2).copied().and_then(hex_value).is_some()
        {
            return true;
        }
        index += 1;
    }
    false
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn hex_digit(value: u8) -> char {
    match value {
        0..=9 => char::from(b'0' + value),
        _ => char::from(b'A' + value - 10),
    }
}

fn uri_error(message: &str) -> Error {
    Error::invalid_argument(ErrorCode::MCP_INVALID_ARGUMENT, message)
}

// ---------------------------------------------------------------------------
// URI 解析
// ---------------------------------------------------------------------------

/// 一次 `resources/read` 指向的资源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceRequest {
    /// `database://connections`。
    Connections,
    /// `database://{alias}/{database}/overview`。
    Overview {
        /// 连接 alias（已解码）。
        alias: String,
        /// 数据库名（已解码）。
        database: String,
    },
    /// `database://{alias}/{database}/schema`。
    Schema {
        /// 连接 alias（已解码）。
        alias: String,
        /// 数据库名（已解码）。
        database: String,
    },
    /// `database://{alias}/{database}/tables/{table}`。
    Table {
        /// 连接 alias（已解码）。
        alias: String,
        /// 数据库名（已解码）。
        database: String,
        /// 表/视图名（已解码，可含 `/`、`%`、空格、中文）。
        table: String,
    },
}

/// 解析资源 URI：先按原始 `/` 拆段，再逐段解码一次，最后判断形状。
///
/// 顺序是安全契约的一部分：**解码成功之后**调用方才可能拿去做授权；这里只做
/// 形状判断，不接触配置。
pub fn parse(uri: &str) -> Result<ResourceRequest> {
    let Some(rest) = uri.strip_prefix(SCHEME_PREFIX) else {
        return Err(uri_error(
            "资源 URI 必须以 database:// 开头（例如 database://connections）",
        ));
    };
    let raw_segments: Vec<&str> = rest.split('/').collect();
    if raw_segments.iter().any(|segment| segment.is_empty()) {
        return Err(uri_error(
            "资源 URI 含空路径段（多余或缺失的 /）：路径段必须非空",
        ));
    }
    if raw_segments.len() > 4 {
        return Err(uri_error(
            "资源 URI 路径段过多：最长形态是 database://{alias}/{database}/tables/{table}",
        ));
    }
    let mut segments = Vec::with_capacity(raw_segments.len());
    for raw in &raw_segments {
        segments.push(decode_segment(raw)?);
    }
    match segments.as_slice() {
        [single] if single == "connections" => Ok(ResourceRequest::Connections),
        [alias, database, resource] if resource == "overview" => Ok(ResourceRequest::Overview {
            alias: alias.clone(),
            database: database.clone(),
        }),
        [alias, database, resource] if resource == "schema" => Ok(ResourceRequest::Schema {
            alias: alias.clone(),
            database: database.clone(),
        }),
        [alias, database, prefix, table] if prefix == "tables" => Ok(ResourceRequest::Table {
            alias: alias.clone(),
            database: database.clone(),
            table: table.clone(),
        }),
        _ => Err(uri_error(
            "无法识别的资源 URI：只支持 database://connections、\
             database://{connection_alias}/{database}/overview、…/schema、\
             database://{connection_alias}/{database}/tables/{table}",
        )),
    }
}

// ---------------------------------------------------------------------------
// 资源清单（resources/list 与 resources/templates/list）
// ---------------------------------------------------------------------------

/// `resources/list` 的内容：只有静态资源。
///
/// 模板资源由 [`templates`] 单独播发；这里刻意不按连接枚举具体 URI——那需要
/// 打开数据库、枚举库，与「`resources/list` 不发起数据库连接」冲突。
#[must_use]
pub fn list() -> Vec<Resource> {
    vec![
        Resource::new(CONNECTIONS_URI, "database-connections")
            .with_title("已授权连接概览")
            .with_description(
                "mcp.json 中已启用的连接：alias、driver_id、connection_status。不含凭据、\
                 用户名与连接串，也不会主动连接数据库。",
            )
            .with_mime_type(MIME_TYPE),
    ]
}

/// `resources/templates/list` 的内容：三个参数化资源。
///
/// 模板里的变量（`{connection_alias}` / `{database}` / `{table}`）按 RFC 6570 简单
/// 展开：取值必须**逐段** percent-encode（[`encode_segment`]），因此表名里的 `/`
/// 不会变成新的路径段。
#[must_use]
pub fn templates() -> Vec<ResourceTemplate> {
    vec![
        ResourceTemplate::new(OVERVIEW_TEMPLATE, "database-overview")
            .with_title("数据库概览")
            .with_description(
                "库级摘要：connection_alias、driver_id、database_name、server_version、\
                 schema_support、table_count、view_count。不含表结构明细。\
                 三个变量各自做 RFC 3986 路径段编码。",
            )
            .with_mime_type(MIME_TYPE),
        ResourceTemplate::new(SCHEMA_TEMPLATE, "database-schema")
            .with_title("数据库对象目录")
            .with_description(
                "对象目录：database_name、object_count、by_kind、objects（表/视图名 + 种类 + \
                 注释）。不含逐表字段明细。超过 500 个对象时 truncated=true。",
            )
            .with_mime_type(MIME_TYPE),
        ResourceTemplate::new(TABLE_TEMPLATE, "database-table")
            .with_title("表结构")
            .with_description(
                "单表结构：列定义（名称、序号、类型、NULL 属性、默认值、字符集/排序规则）与\
                 主键列。`{table}` 段必须 percent-encode（空格、`/`、`%`、中文都要编码）。",
            )
            .with_mime_type(MIME_TYPE),
    ]
}

// ---------------------------------------------------------------------------
// 内容契约
// ---------------------------------------------------------------------------

/// 一个已授权连接的可见信息（`database://connections` 的条目）。
#[derive(Debug, Clone, Serialize)]
pub struct ConnectionEntry {
    /// 连接 alias。
    pub alias: String,
    /// 驱动标识（`sqlite` / `mariadb`）。
    pub driver_id: String,
    /// `not_connected` / `connected` / `failed`。
    pub connection_status: String,
}

/// `database://connections` 的内容。
#[derive(Debug, Clone, Serialize)]
pub struct ConnectionsPayload {
    /// 已授权连接（按 alias 升序）。
    pub connections: Vec<ConnectionEntry>,
    /// 连接数量。
    pub count: i64,
}

/// 驱动的层级与能力摘要（`overview.schema_support`）。
#[derive(Debug, Clone, Serialize)]
pub struct SchemaSupport {
    /// 是否存在独立 schema 层级（来自驱动 `Capabilities.schemas`）。
    pub has_schema_level: bool,
    /// 驱动声明的能力稳定名（`Capabilities::names()`，顺序由驱动决定）。
    pub capabilities: Vec<String>,
}

/// `database://{alias}/{database}/overview` 的内容。
#[derive(Debug, Clone, Serialize)]
pub struct OverviewPayload {
    /// 连接 alias。
    pub connection_alias: String,
    /// 驱动标识。
    pub driver_id: String,
    /// 数据库名。
    pub database_name: String,
    /// 服务端版本。
    ///
    /// 当前 Driver SPI 只在 `test_connection` 的报告里给 `server_version`，
    /// 需要额外握手；V1 不为此发起额外连接，因此固定为 `null`（不虚构）。
    pub server_version: Option<String>,
    /// schema 层级支持情况（来自现有 `Capabilities`）。
    pub schema_support: SchemaSupport,
    /// 表数量；拿不到（驱动不支持过滤下推，或超过单页上限）时为 `null`。
    pub table_count: Option<i64>,
    /// 视图数量；同上。
    pub view_count: Option<i64>,
    /// 计数是否因为「超过单页上限」而无法给出精确值。
    pub counts_truncated: bool,
}

/// 对象目录里的一个条目。
#[derive(Debug, Clone, Serialize)]
pub struct ObjectEntry {
    /// 对象名（展示名）。
    pub name: String,
    /// 对象种类（`table` / `view` / …，驱动原始种类名）。
    pub kind: String,
    /// 数据库中的注释原文；驱动给不出时为 `null`。
    pub comment: Option<String>,
}

/// `database://{alias}/{database}/schema` 的内容。
#[derive(Debug, Clone, Serialize)]
pub struct SchemaPayload {
    /// 连接 alias。
    pub connection_alias: String,
    /// 驱动标识。
    pub driver_id: String,
    /// 数据库名。
    pub database_name: String,
    /// 对象总数（`objects` 的长度）。
    pub object_count: i64,
    /// 按种类计数（`table` / `view` / …）。
    pub by_kind: BTreeMap<String, i64>,
    /// 对象列表（受单页上限约束）。
    pub objects: Vec<ObjectEntry>,
    /// 是否因单页上限而没有列全。
    pub truncated: bool,
    /// 截断时的分页指引；未截断时不出现。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guidance: Option<String>,
}

/// `database://{alias}/{database}/tables/{table}` 的内容。
///
/// 列定义与主键复用 `describe_table` 的输出类型，字段与工具完全一致。
#[derive(Debug, Clone, Serialize)]
pub struct TablePayload {
    /// 连接 alias。
    pub connection_alias: String,
    /// 数据库名。
    pub database: String,
    /// 表/视图名。
    pub table: String,
    /// 列定义（按驱动顺序）。
    pub columns: Vec<ColumnDetail>,
    /// 列数量。
    pub column_count: i64,
    /// 主键列（按主键顺序）；视图或没有主键时为空。
    pub primary_key: Vec<PrimaryKeyColumnView>,
    /// 是否存在主键。
    pub has_primary_key: bool,
}

/// 资源读取结果。
#[derive(Debug, Clone)]
pub struct ResourcePayload {
    /// 请求的 URI（原样回显进 `contents[].uri`）。
    pub uri: String,
    /// `application/json` 文本。
    pub json: String,
}

/// 读取一个资源。
///
/// 流程：解析 URI → 按资源类型经 [`AccessService`] 授权并取数 → 序列化 →
/// 检查整体大小。授权（alias 是否存在/启用）一律发生在解码之后。
pub async fn read(access: &AccessService, uri: &str) -> Result<ResourcePayload> {
    let request = parse(uri)?;
    let json = match &request {
        ResourceRequest::Connections => {
            to_json(&connections(access).await.map_err(authorization_error)?)?
        }
        ResourceRequest::Overview { alias, database } => {
            to_json(&overview(access, alias, database).await?)?
        }
        ResourceRequest::Schema { alias, database } => {
            to_json(&schema(access, alias, database).await?)?
        }
        ResourceRequest::Table {
            alias,
            database,
            table,
        } => to_json(&table_payload(access, alias, database, table).await?)?,
    };
    let size = u64::try_from(json.len()).unwrap_or(u64::MAX);
    let limit = access.limits().max_result_bytes;
    if size > limit {
        return Err(Error::new(
            ErrorCode::MCP_RESULT_TOO_LARGE,
            ErrorCategory::ResourceExhausted,
            format!(
                "资源内容 {size} 字节，超过单次上限 {limit} 字节；请改用粒度更细的工具\
                 （list_tables / describe_table / execute_readonly_query）分批读取"
            ),
        ));
    }
    Ok(ResourcePayload {
        uri: uri.to_owned(),
        json,
    })
}

/// 序列化一个资源载荷。
///
/// 载荷都是本模块自己的结构体，序列化失败只可能是内部缺陷；退化成 `mcp.internal`
/// 而不是 panic（协议层不该因为一个不可能的失败把整个 session 打掉）。
fn to_json<T: Serialize>(payload: &T) -> Result<String> {
    serde_json::to_string(payload)
        .map_err(|error| Error::internal(format!("资源序列化失败：{error}")))
}

/// `database://connections`：只读配置与懒连接状态，不触网。
pub async fn connections(access: &AccessService) -> Result<ConnectionsPayload> {
    let connections: Vec<ConnectionEntry> = access
        .statuses()
        .await?
        .into_iter()
        .map(|status| ConnectionEntry {
            alias: status.alias,
            driver_id: status.driver_id,
            connection_status: status.connection_status.to_owned(),
        })
        .collect();
    let count = i64::try_from(connections.len()).unwrap_or(i64::MAX);
    Ok(ConnectionsPayload { connections, count })
}

/// `…/overview`：库级摘要。
async fn overview(access: &AccessService, alias: &str, database: &str) -> Result<OverviewPayload> {
    let alias = alias.to_owned();
    let database_name = database.to_owned();
    let (driver_id, capabilities, tables, views) = access
        .run(&alias, |handle| {
            let database = database_name.clone();
            async move {
                let summary = handle.summary();
                let driver_id = summary.driver_id.as_str().to_owned();
                let capabilities: Vec<String> = summary
                    .capabilities
                    .iter()
                    .map(|name| (*name).to_owned())
                    .collect();
                let tables = count_objects(&handle, &database, ObjectKind::Table).await?;
                let views = count_objects(&handle, &database, ObjectKind::View).await?;
                Ok((driver_id, capabilities, tables, views))
            }
        })
        .await
        .map_err(authorization_error)?;
    Ok(OverviewPayload {
        connection_alias: alias,
        driver_id,
        database_name,
        server_version: None,
        schema_support: SchemaSupport {
            has_schema_level: capabilities.iter().any(|name| name == "schemas"),
            capabilities,
        },
        table_count: tables.count,
        view_count: views.count,
        counts_truncated: tables.truncated || views.truncated,
    })
}

/// 单类对象的计数结果。
struct ObjectCount {
    /// 精确数量；`None` 表示拿不到可靠值。
    count: Option<i64>,
    /// 是否因为超过单页上限而拿不到精确值。
    truncated: bool,
}

/// 经 `list_children_filtered` 一次统计某类对象。
///
/// 驱动没有实现过滤下推时返回 `capability.unsupported`——那不是故障，而是
/// 「本驱动给不出可靠计数」，如实降级成 `None` 而不是把全量读回来自己数。
async fn count_objects(
    handle: &SessionHandle,
    database: &str,
    kind: ObjectKind,
) -> Result<ObjectCount> {
    let filter = ObjectFilter {
        name: None,
        kinds: vec![kind],
    };
    let page = handle
        .list_children_filtered(Some(catalog_ref(handle, database)), &filter, &first_page())
        .await;
    match page {
        Ok(page) => {
            let truncated = page.truncated || page.next_page_token.is_some();
            let count = if truncated {
                None
            } else {
                Some(i64::try_from(page.items.len()).unwrap_or(i64::MAX))
            };
            Ok(ObjectCount { count, truncated })
        }
        Err(error) if error.code() == ErrorCode::CAPABILITY_UNSUPPORTED => Ok(ObjectCount {
            count: None,
            truncated: false,
        }),
        Err(error) => Err(error),
    }
}

/// `…/schema`：对象目录（表/视图名 + 种类）。
async fn schema(access: &AccessService, alias: &str, database: &str) -> Result<SchemaPayload> {
    let alias = alias.to_owned();
    let database_name = database.to_owned();
    let (driver_id, page) = access
        .run(&alias, |handle| {
            let database = database_name.clone();
            async move {
                let driver_id = handle.summary().driver_id.as_str().to_owned();
                let page = handle
                    .list_children(Some(catalog_ref(&handle, &database)), &first_page())
                    .await?;
                Ok((driver_id, page))
            }
        })
        .await
        .map_err(authorization_error)?;

    let objects: Vec<ObjectEntry> = page
        .items
        .iter()
        .map(|node| ObjectEntry {
            name: node.display_name.clone(),
            kind: node.kind.as_str().to_owned(),
            comment: node.comment.clone(),
        })
        .collect();
    let mut by_kind: BTreeMap<String, i64> = BTreeMap::new();
    for object in &objects {
        let counter = by_kind.entry(object.kind.clone()).or_insert(0);
        *counter = counter.saturating_add(1);
    }
    let truncated = page.truncated || page.next_page_token.is_some();
    Ok(SchemaPayload {
        connection_alias: alias,
        driver_id,
        database_name,
        object_count: i64::try_from(objects.len()).unwrap_or(i64::MAX),
        by_kind,
        objects,
        truncated,
        guidance: truncated.then(|| PAGINATION_GUIDANCE.to_owned()),
    })
}

/// `…/tables/{table}`：单表结构。
async fn table_payload(
    access: &AccessService,
    alias: &str,
    database: &str,
    table: &str,
) -> Result<TablePayload> {
    let alias = alias.to_owned();
    let database_name = database.to_owned();
    let table_name = table.to_owned();
    let (columns, primary_key) = access
        .run_metadata(&alias, &CancellationToken::new(), {
            let database = database_name.clone();
            let table = table_name.clone();
            move |handle, read| {
                let object = table_ref(&handle, &database, &table);
                read_table_columns(handle, object, read)
            }
        })
        .await
        .map_err(authorization_error)?;
    let primary_key = primary_key_columns(&primary_key);
    Ok(TablePayload {
        connection_alias: alias,
        database: database_name,
        table: table_name,
        column_count: i64::try_from(columns.len()).unwrap_or(i64::MAX),
        has_primary_key: !primary_key.is_empty(),
        columns,
        primary_key,
    })
}

/// 库（catalog）层的对象引用：catalog = 库名，name 为空 + `kind = Catalog`。
fn catalog_ref(handle: &SessionHandle, database: &str) -> ObjectRef {
    ObjectRef {
        session_id: handle.summary().session_id,
        qualified_name: QualifiedName::new(Some(database.to_owned()), None, String::new()),
        kind: ObjectKind::Catalog,
    }
}

/// 表层对象引用。
fn table_ref(handle: &SessionHandle, database: &str, table: &str) -> ObjectRef {
    ObjectRef {
        session_id: handle.summary().session_id,
        qualified_name: QualifiedName::new(Some(database.to_owned()), None, table.to_owned()),
        kind: ObjectKind::Table,
    }
}

/// 第一页请求（页大小取驱动上限）。
fn first_page() -> PageRequest {
    PageRequest {
        page_size: MAX_PAGE_SIZE,
        page_token: None,
    }
}

/// 把授权失败归一化成「资源不存在」。
///
/// 未授权和不存在继续使用资源的 -32002 通道。全局拒绝在查找连接前发生，
/// 可以保留原始原因，同时避免披露连接是否存在。
fn authorization_error(error: Error) -> Error {
    match error.code() {
        ErrorCode::MCP_CONNECTION_NOT_FOUND => {
            tracing::warn!(
                code = %error.code(),
                "资源请求被拒绝：连接未授权（原始子码见本行，不回传给 Agent）"
            );
            Error::not_found(
                ErrorCode::MCP_CONNECTION_NOT_FOUND,
                "资源不存在或该连接未授权；请用 resources/list 与 list_connections 核对别名",
            )
        }
        _ => error,
    }
}
