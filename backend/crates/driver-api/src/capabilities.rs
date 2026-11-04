//! 驱动能力描述符。
//!
//! 能力必须显式声明。通用业务层只依据这里的信息决定是否允许某项操作，
//! 不允许通过尝试执行来「试探」是否支持。

/// 驱动声明的能力集合。
///
/// 全部为 `false` 表示最小能力：只能连接与执行单条 SQL。
///
/// 这里刻意使用布尔矩阵而不是位标志：能力项数量有限、可读性优先，
/// 且需要逐项序列化为字符串列表。
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// 支持显式事务。
    pub transactions: bool,
    /// 支持取消正在执行的查询。
    pub query_cancellation: bool,
    /// 支持分批/流式读取结果集。
    pub streaming_results: bool,
    /// 支持一次提交多条语句。
    pub multiple_statements: bool,
    /// 支持生成 DDL。
    pub ddl_generation: bool,
    /// 支持服务端分页（而不是拉全量后切分）。
    pub server_side_pagination: bool,
    /// 存在 Catalog 层级。
    pub catalogs: bool,
    /// 存在 Schema 层级。
    pub schemas: bool,
}

impl Capabilities {
    /// 已支持能力的稳定名称列表，用于 gRPC 能力描述与日志。
    #[must_use]
    pub fn names(&self) -> Vec<&'static str> {
        let mut names = Vec::new();
        if self.transactions {
            names.push("transactions");
        }
        if self.query_cancellation {
            names.push("query_cancellation");
        }
        if self.streaming_results {
            names.push("streaming_results");
        }
        if self.multiple_statements {
            names.push("multiple_statements");
        }
        if self.ddl_generation {
            names.push("ddl_generation");
        }
        if self.server_side_pagination {
            names.push("server_side_pagination");
        }
        if self.catalogs {
            names.push("catalogs");
        }
        if self.schemas {
            names.push("schemas");
        }
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_has_no_capabilities() {
        assert_eq!(Capabilities::default().names(), [] as [&str; 0]);
    }

    #[test]
    fn names_are_stable() {
        let capabilities = Capabilities {
            transactions: true,
            query_cancellation: true,
            ..Capabilities::default()
        };
        assert_eq!(capabilities.names(), ["transactions", "query_cancellation"]);
    }
}
