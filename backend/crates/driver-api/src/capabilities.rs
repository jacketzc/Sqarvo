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
    /// 支持行编辑：`TableDataProvider::supports_row_edits()` 为 `true`，
    /// 即浏览可用且 `ApplyRowChanges` 可以提交 update / delete（ADR 0017）。
    ///
    /// 与 [`crate::DriverSession::table_data`] 的一致性：这里为 `true` 的驱动，
    /// `table_data()` 必须返回 `Some`，且 `supports_row_edits()` 必须为 `true`；
    /// 具体对象是否可写（无主键、视图等）仍由 `TablePage::read_only` 逐个判定。
    pub row_editing: bool,
    /// 支持参数化语句：值经绑定的占位符进入语句，不拼进 SQL 文本。
    ///
    /// 与 [`crate::DriverSession::supports_parameterized_statements`] 保持一致：
    /// 这里为 `true` 的驱动，其会话必须实现 `execute_parameterized`，而不是返回
    /// `capability.unsupported`。能力描述是给通用层做决策用的，不允许「先声明再试」。
    pub parameterized_statements: bool,
    /// 支持「只校验证书链、不校验主机名」的 TLS 模式（`TlsMode::VerifyCa` 的契约语义）。
    ///
    /// 为 `false` 的驱动把 `VerifyCa` 归一化为 `VerifyFull`，即**多校验**一层主机名。
    /// 退化只允许朝更严的方向走，且必须显式声明：能力位为 `false` 时前端要在连接
    /// 对话框里标注该档位等同 `VerifyFull`，不得让用户以为「只校链」真的生效（ADR 0020）。
    /// 没有 TLS 的文件型驱动同样为 `false`。
    pub tls_verify_ca_chain_only: bool,
    /// 表浏览支持排序、筛选与精确总数（ADR 0022）。
    ///
    /// 与 [`crate::TableDataProvider::supports_browse_sort_filter_total`] 一致：为
    /// `false` 时前端不得提供列头排序 / 筛选 / 精确计数入口，驱动也必须忽略
    /// `BrowseOptions` 的对应字段（返回 v0.2.2 的「主键升序 + 游标」行为）。
    ///
    /// 默认 `false`：#109 只做契约增量，SQL 下推（`ORDER BY` / `WHERE` /
    /// `COUNT(*)`）在 #110 / #111 实现后才允许翻成 `true`。
    pub table_browse_sort_filter_total: bool,
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
        if self.row_editing {
            names.push("row_editing");
        }
        if self.parameterized_statements {
            names.push("parameterized_statements");
        }
        if self.tls_verify_ca_chain_only {
            names.push("tls_verify_ca_chain_only");
        }
        if self.table_browse_sort_filter_total {
            names.push("table_browse_sort_filter_total");
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

    #[test]
    fn editing_capabilities_are_appended_without_moving_old_names() {
        // 能力名是 gRPC 与前端可见的稳定标识：新增项只能追加，既有顺序不变。
        let capabilities = Capabilities {
            transactions: true,
            row_editing: true,
            parameterized_statements: true,
            ..Capabilities::default()
        };
        assert_eq!(
            capabilities.names(),
            ["transactions", "row_editing", "parameterized_statements"]
        );
    }

    #[test]
    fn minimal_driver_does_not_claim_editing_capabilities() {
        // 测试替身/最小驱动只声明连接与单条 SQL：两项编辑能力必须为 false，
        // 否则 ListDrivers 会向通用层谎报能力。
        let minimal = Capabilities::default();
        assert!(!minimal.row_editing);
        assert!(!minimal.parameterized_statements);
        assert!(!minimal.names().contains(&"row_editing"));
        assert!(!minimal.names().contains(&"parameterized_statements"));
    }

    #[test]
    fn tls_verify_ca_chain_only_is_a_positive_capability() {
        // 正向命名：缺省即「不支持只校链」，前端据此标注 verify-ca 等同 verify-full（ADR 0020）。
        // 名字只能追加，不能插进既有顺序里。
        let capabilities = Capabilities {
            transactions: true,
            tls_verify_ca_chain_only: true,
            ..Capabilities::default()
        };
        assert_eq!(
            capabilities.names(),
            ["transactions", "tls_verify_ca_chain_only"]
        );
        let minimal = Capabilities::default();
        assert!(!minimal.tls_verify_ca_chain_only);
        assert!(!minimal.names().contains(&"tls_verify_ca_chain_only"));
    }

    #[test]
    fn browse_sort_filter_total_is_opt_in_and_appended_last() {
        // ADR 0022：排序/筛选/精确计数的能力位默认 false（#110 / #111 才翻 true），
        // 名字追加在既有项之后，不改动任何既有能力的顺序。
        let minimal = Capabilities::default();
        assert!(!minimal.table_browse_sort_filter_total);
        assert!(!minimal.names().contains(&"table_browse_sort_filter_total"));

        let capabilities = Capabilities {
            transactions: true,
            row_editing: true,
            table_browse_sort_filter_total: true,
            ..Capabilities::default()
        };
        assert_eq!(
            capabilities.names(),
            [
                "transactions",
                "row_editing",
                "table_browse_sort_filter_total"
            ]
        );
    }
}
