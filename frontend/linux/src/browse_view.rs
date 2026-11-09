//! 表浏览的**纯逻辑**（#118）：排序状态、筛选条件、分页/总数三态、摘要文案、
//! 列显隐与能力位门控。
//!
//! 本模块不依赖 GTK，也不发 RPC：界面（[`crate::data_page`]）、状态栏
//! （[`crate::shell`]）与 worker（[`crate::backend::worker`]）都从这里取同一份
//! 判定与文案，因此「排序后回到第一页」「估计值不得写成确定值」这类规则只有一处。
//!
//! 关键约束（ADR 0022）：
//!
//! - 排序/筛选改变后 offset 必须回到 0（[`BrowseSession::set_sort`] /
//!   [`BrowseSession::set_filter`]）；翻页只走 offset，**不再**带 `page_token`；
//! - 总数三态必须显式：`EXACT` → 「共 N 行」、`ESTIMATE` → 「约 N 行」、
//!   `UNKNOWN` → 不显示（跳过统计时额外说明）；
//! - 驱动 `table_browse_sort_filter_total = false` 时，入口隐藏而不是点了报错
//!   （[`BrowseCapabilities`]），分页器仍只用 offset。

#![allow(
    dead_code,
    reason = "纯逻辑模块：部分访问器只在单测与后续导出（#120）里使用，界面按需取用"
)]

use crate::backend::table::{TableColumn, TableEntry};
use crate::proto::common::v1::{ColumnValue, ValueKind, column_value};
use crate::proto::services::v1::{
    Filter, FilterOperator as ProtoFilterOperator, SortDirection, SortKey, TotalKind,
    filter::Condition as ProtoCondition,
};

/// 驱动是否支持「排序 / 筛选 / 总数」这一组能力。
///
/// 两个内置驱动都是 `true`，但代码路径必须存在：能力位为 `false` 时界面隐藏
/// 排序、筛选与总数入口，分页器只按 offset 走（不显示总数）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrowseCapabilities {
    /// 服务端下推排序 / 筛选并给出总数。
    pub sort_filter_total: bool,
}

impl BrowseCapabilities {
    /// 能力位名称（与 `DriverCapabilities::names()` 一致）。
    pub const CAPABILITY: &'static str = "table_browse_sort_filter_total";

    /// 两个内置驱动的能力。
    #[must_use]
    pub const fn all() -> Self {
        Self {
            sort_filter_total: true,
        }
    }

    /// 保守取值：不认识就当作不支持（隐藏入口，绝不假装支持）。
    #[must_use]
    pub const fn none() -> Self {
        Self {
            sort_filter_total: false,
        }
    }

    /// 从 `ConnectResponse.capabilities` / `ListSessions` 的能力位名称解析。
    #[must_use]
    pub fn from_names(names: &[String]) -> Self {
        Self {
            sort_filter_total: names.iter().any(|name| name == Self::CAPABILITY),
        }
    }

    /// 排序入口是否可用。
    #[must_use]
    pub fn sorting(self) -> bool {
        self.sort_filter_total
    }

    /// 筛选入口是否可用。
    #[must_use]
    pub fn filtering(self) -> bool {
        self.sort_filter_total
    }

    /// 总数入口（分页器上的总数文案）是否可用。
    #[must_use]
    pub fn totals(self) -> bool {
        self.sort_filter_total
    }
}

/// 排序方向（与 proto 同构，避免在界面里散落 `i32`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// 升序。
    Ascending,
    /// 降序。
    Descending,
}

impl Direction {
    /// 翻转方向。
    #[must_use]
    pub fn flipped(self) -> Self {
        match self {
            Self::Ascending => Self::Descending,
            Self::Descending => Self::Ascending,
        }
    }

    /// 服务端枚举。
    #[must_use]
    pub fn to_proto(self) -> SortDirection {
        match self {
            Self::Ascending => SortDirection::Asc,
            Self::Descending => SortDirection::Desc,
        }
    }

    /// 文案（摘要与 tooltip 用）。
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Ascending => "ASC",
            Self::Descending => "DESC",
        }
    }
}

/// 一个排序键（列名 + 方向）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortSpec {
    /// 列名（必须来自 `GetColumns`）。
    pub column: String,
    /// 方向。
    pub direction: Direction,
}

/// 表头上的排序标记。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortMarker {
    /// 该列未参与排序。
    None,
    /// 升序；`priority` 仅多列排序时大于 0（用于显示序号）。
    Ascending {
        /// 多列排序里的次序（1 起；单列时为 0）。
        priority: usize,
    },
    /// 降序。
    Descending {
        /// 多列排序里的次序（1 起；单列时为 0）。
        priority: usize,
    },
}

impl SortMarker {
    /// 标记文本（`▲` / `▼` / `▲2`）。空标记返回空串。
    #[must_use]
    pub fn text(self) -> String {
        let (glyph, priority) = match self {
            Self::None => return String::new(),
            Self::Ascending { priority } => ("▲", priority),
            Self::Descending { priority } => ("▼", priority),
        };
        if priority > 1 {
            format!("{glyph}{priority}")
        } else {
            glyph.to_owned()
        }
    }
}

/// 排序状态：有序的键列表（多列排序 = 多个键）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SortOrder {
    keys: Vec<SortSpec>,
}

impl SortOrder {
    /// 空排序（= 服务端默认的「主键升序」）。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 从既有键恢复（回到第一页时保持排序）。
    #[must_use]
    pub fn from_keys(keys: Vec<SortSpec>) -> Self {
        Self { keys }
    }

    /// 当前键。
    #[must_use]
    pub fn keys(&self) -> &[SortSpec] {
        &self.keys
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// 点击表头：切换该列。
    ///
    /// - 未排序的列：升序追加（`extend = false` 时先清空其它键，单列排序）；
    /// - 已升序的列：翻成降序；已降序的列：取消该列的排序；
    /// - `extend = true`（Shift）时保留其它列，形成多列排序。
    pub fn toggle(&mut self, column: &str, extend: bool) {
        let existing = self.keys.iter().position(|key| key.column == column);
        if !extend && self.keys.len() > 1 {
            // 非 Shift 点击：只保留被点的这一列（其余全部去掉）。
            let direction = existing
                .map(|index| self.keys[index].direction.flipped())
                .unwrap_or(Direction::Ascending);
            self.keys.clear();
            self.keys.push(SortSpec {
                column: column.to_owned(),
                direction,
            });
            return;
        }
        if !extend && existing.is_none() {
            self.keys.clear();
        }
        match existing {
            Some(index) => {
                if self.keys[index].direction == Direction::Ascending {
                    // 升序 → 降序；
                    self.keys[index].direction = Direction::Descending;
                } else {
                    // 降序 → 取消（多列排序里只摘掉这一列）。
                    self.keys.remove(index);
                }
            }
            None => self.keys.push(SortSpec {
                column: column.to_owned(),
                direction: Direction::Ascending,
            }),
        }
    }

    /// 面板「添加排序」：设置某列的方向。
    ///
    /// 与表头点击的 [`Self::toggle`] 不同，这里**不**清空其它键：面板里的排序是一张
    /// 有序清单，用户显式指定方向；已存在的列就地改方向并保持它原来的次序。
    pub fn set_key(&mut self, column: &str, direction: Direction) {
        match self.keys.iter_mut().find(|key| key.column == column) {
            Some(key) => key.direction = direction,
            None => self.keys.push(SortSpec {
                column: column.to_owned(),
                direction,
            }),
        }
    }

    /// 删掉第 `index` 个键（越界忽略）。
    pub fn remove(&mut self, index: usize) {
        if index < self.keys.len() {
            self.keys.remove(index);
        }
    }

    /// 上移一个键（越界或已在首位时忽略）。
    pub fn move_up(&mut self, index: usize) {
        if index > 0 && index < self.keys.len() {
            self.keys.swap(index - 1, index);
        }
    }

    /// 下移一个键（越界或已在末位时忽略）。
    pub fn move_down(&mut self, index: usize) {
        if index + 1 < self.keys.len() {
            self.keys.swap(index, index + 1);
        }
    }

    /// 清空排序。
    pub fn clear(&mut self) {
        self.keys.clear();
    }

    /// 某列表头的标记。
    #[must_use]
    pub fn marker(&self, column: &str) -> SortMarker {
        let Some(index) = self.keys.iter().position(|key| key.column == column) else {
            return SortMarker::None;
        };
        let priority = if self.keys.len() > 1 { index + 1 } else { 0 };
        match self.keys[index].direction {
            Direction::Ascending => SortMarker::Ascending { priority },
            Direction::Descending => SortMarker::Descending { priority },
        }
    }

    /// 服务端排序键。
    #[must_use]
    pub fn to_proto(&self) -> Vec<SortKey> {
        self.keys
            .iter()
            .map(|key| SortKey {
                column: key.column.clone(),
                direction: key.direction.to_proto() as i32,
            })
            .collect()
    }

    /// 摘要里的 `ORDER BY` 片段（空排序时为空串）。
    #[must_use]
    pub fn describe(&self) -> String {
        self.keys
            .iter()
            .map(|key| format!("{} {}", quote_ident(&key.column), key.direction.label()))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// 筛选算子（界面清单与 proto 一一对应）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    /// `=`
    Eq,
    /// `<>`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `LIKE`
    Like,
    /// `IS NULL`
    IsNull,
    /// `IS NOT NULL`
    IsNotNull,
}

impl Operator {
    /// 面板里的全部算子（顺序即下拉顺序）。
    pub const ALL: [Self; 9] = [
        Self::Eq,
        Self::Ne,
        Self::Lt,
        Self::Le,
        Self::Gt,
        Self::Ge,
        Self::Like,
        Self::IsNull,
        Self::IsNotNull,
    ];

    /// 下拉文案：**只写符号**。
    ///
    /// 「= 等于」这类翻译是给不懂符号的人看的，但在数据库客户端里读者就是懂的人：
    /// 中文后缀把下拉撑宽、还要在窄面板里换行，收益是负的。`LIKE` / `IS NULL` 直接
    /// 写 SQL 原文（它们没有通行符号）。
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Eq => "=",
            Self::Ne => "≠",
            Self::Lt => "<",
            Self::Le => "≤",
            Self::Gt => ">",
            Self::Ge => "≥",
            Self::Like => "LIKE",
            Self::IsNull => "IS NULL",
            Self::IsNotNull => "IS NOT NULL",
        }
    }

    /// 是否需要一个值（`IS NULL` 类算子不需要）。
    #[must_use]
    pub fn needs_value(self) -> bool {
        !matches!(self, Self::IsNull | Self::IsNotNull)
    }

    /// 服务端枚举。
    #[must_use]
    pub fn to_proto(self) -> ProtoFilterOperator {
        match self {
            Self::Eq => ProtoFilterOperator::Eq,
            Self::Ne => ProtoFilterOperator::Ne,
            Self::Lt => ProtoFilterOperator::Lt,
            Self::Le => ProtoFilterOperator::Le,
            Self::Gt => ProtoFilterOperator::Gt,
            Self::Ge => ProtoFilterOperator::Ge,
            Self::Like => ProtoFilterOperator::Like,
            Self::IsNull => ProtoFilterOperator::IsNull,
            Self::IsNotNull => ProtoFilterOperator::IsNotNull,
        }
    }

    /// 摘要里的算子文本。
    #[must_use]
    pub fn sql(self) -> &'static str {
        match self {
            Self::Eq => "=",
            Self::Ne => "<>",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::Like => "LIKE",
            Self::IsNull => "IS NULL",
            Self::IsNotNull => "IS NOT NULL",
        }
    }

    /// 从下拉下标反查（越界退回下标 0，不 panic）。
    #[must_use]
    pub fn from_index(index: u32) -> Self {
        usize::try_from(index)
            .ok()
            .and_then(|index| Self::ALL.get(index).copied())
            .unwrap_or(Self::Eq)
    }

    /// 下拉下标。
    #[must_use]
    pub fn index(self) -> u32 {
        u32::try_from(Self::ALL.iter().position(|op| *op == self).unwrap_or(0)).unwrap_or(0)
    }
}

/// 一个筛选条件（`列 算子 值`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Condition {
    /// 列名。
    pub column: String,
    /// 算子。
    pub operator: Operator,
    /// 用户输入的值原文（`IS NULL` 时为 `None`）。
    pub value: Option<String>,
}

impl Condition {
    /// 摘要里的一个条件（值按 SQL 字面量展示，仅用于只读摘要）。
    #[must_use]
    pub fn describe(&self) -> String {
        let column = quote_ident(&self.column);
        if self.operator.needs_value() {
            format!(
                "{column} {} {}",
                self.operator.sql(),
                summary_value(self.value.as_deref().unwrap_or(""))
            )
        } else {
            format!("{column} {}", self.operator.sql())
        }
    }
}

/// 筛选条件集合（v0.2.3 只支持 AND）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FilterSet {
    conditions: Vec<Condition>,
}

impl FilterSet {
    /// 空筛选（= 不过滤）。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前条件。
    #[must_use]
    pub fn conditions(&self) -> &[Condition] {
        &self.conditions
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.conditions.is_empty()
    }

    /// 追加一条（同列同算子的旧条件先去掉：面板里重复条件没有意义，也会让摘要难读）。
    pub fn push(&mut self, condition: Condition) {
        self.conditions.retain(|existing| {
            existing.column != condition.column || existing.operator != condition.operator
        });
        self.conditions.push(condition);
    }

    /// 删掉第 `index` 条（越界忽略）。
    pub fn remove(&mut self, index: usize) {
        if index < self.conditions.len() {
            self.conditions.remove(index);
        }
    }

    /// 就地替换第 `index` 条（面板里点条件编辑后「确定」）。
    ///
    /// 与 [`Self::push`] 的去重规则一致：换完不能和**别的**条件重复（同列同算子），
    /// 否则返回可读原因，让用户当场改，而不是让两条一样的条件进摘要。
    pub fn replace(&mut self, index: usize, condition: Condition) -> Result<(), String> {
        if index >= self.conditions.len() {
            return Err("筛选条件已变化：请重新打开筛选面板".to_owned());
        }
        let duplicate = self.conditions.iter().enumerate().any(|(other, existing)| {
            other != index
                && existing.column == condition.column
                && existing.operator == condition.operator
        });
        if duplicate {
            return Err(format!(
                "已经有同样的条件：{} {}",
                condition.column,
                condition.operator.label()
            ));
        }
        self.conditions[index] = condition;
        Ok(())
    }

    /// 清空。
    pub fn clear(&mut self) {
        self.conditions.clear();
    }

    /// 摘要里的 `WHERE` 片段（条件之间 AND；空集为空串）。
    #[must_use]
    pub fn describe(&self) -> String {
        self.conditions
            .iter()
            .map(Condition::describe)
            .collect::<Vec<_>>()
            .join(" AND ")
    }

    /// 构造 proto `Filter`（按列类型转换值；未知列 / 类型不兼容在这里就报错）。
    pub fn to_proto(&self, columns: &[TableColumn]) -> Result<Option<Filter>, String> {
        if self.conditions.is_empty() {
            return Ok(None);
        }
        let mut proto = Vec::with_capacity(self.conditions.len());
        for condition in &self.conditions {
            let column = columns
                .iter()
                .find(|column| column.name == condition.column)
                .ok_or_else(|| {
                    format!("筛选列 {} 不在当前表的列清单里（未知列）", condition.column)
                })?;
            let value = if condition.operator.needs_value() {
                Some(condition_value(
                    column,
                    condition.operator,
                    condition.value.as_deref().unwrap_or_default(),
                )?)
            } else {
                None
            };
            proto.push(ProtoCondition {
                column: condition.column.clone(),
                operator: condition.operator.to_proto() as i32,
                value,
            });
        }
        Ok(Some(Filter { conditions: proto }))
    }
}

/// 把用户输入按列类型转成 proto 值。
///
/// `LIKE` 一律按文本匹配（服务端只接受匹配串）；数值/时间列走
/// [`crate::table_data::parse_cell_input`]，解析失败直接返回可读原因——
/// 不把类型不兼容的值发到服务端等它报语法错。
pub fn condition_value(
    column: &TableColumn,
    operator: Operator,
    text: &str,
) -> Result<ColumnValue, String> {
    if operator == Operator::Like {
        return Ok(ColumnValue {
            kind: ValueKind::Text as i32,
            value: Some(column_value::Value::TextValue(text.to_owned())),
            truncated: false,
            original_type_name: column.type_label.clone(),
        });
    }
    crate::table_data::parse_cell_input(text, column).map_err(|error| {
        format!(
            "筛选值「{text}」与列 {} 的类型 {} 不兼容：{error}",
            column.name, column.type_label
        )
    })
}

/// 总数三态（来自 `BrowseTableResponse.total_kind`，客户端**不推断**）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TotalInfo {
    /// 服务端口径。
    pub kind: TotalKind,
    /// 总行数（`kind = UNKNOWN` 时无意义）。
    pub rows: u64,
    /// 请求了精确计数但被跳过（超时/中断）。
    pub skipped: bool,
}

impl TotalInfo {
    /// 未请求 / 驱动给不出。
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            kind: TotalKind::Unknown,
            rows: 0,
            skipped: false,
        }
    }

    /// 从 proto 字段构造。
    #[must_use]
    pub fn from_proto(kind: i32, rows: u64, skipped: bool) -> Self {
        let kind = TotalKind::try_from(kind).unwrap_or(TotalKind::Unknown);
        if kind == TotalKind::Unknown {
            return Self {
                kind,
                rows: 0,
                skipped,
            };
        }
        Self {
            kind,
            rows,
            skipped: false,
        }
    }

    /// 是否有一个可展示的数量。
    #[must_use]
    pub fn is_known(self) -> bool {
        self.kind != TotalKind::Unknown
    }

    /// 总数文案：`共 N 行` / `约 N 行` / 跳过统计 / `None`。
    ///
    /// **估计值不得写成确定值**：`ESTIMATE` 必须带「约」。
    #[must_use]
    pub fn text(self) -> Option<String> {
        match self.kind {
            TotalKind::Exact => Some(format!("共 {} 行", self.rows)),
            TotalKind::Estimate => Some(format!("约 {} 行", self.rows)),
            TotalKind::Unknown if self.skipped => Some("未统计（超出计数预算）".to_owned()),
            TotalKind::Unknown => None,
        }
    }
}

/// 分页器状态：offset 分页 + 总数三态。
///
/// `offset` 是**服务端**的起始行（0 起）；界面页码从 1 起。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PagerState {
    /// 起始行偏移。
    pub offset: u64,
    /// 每页行数。
    pub page_size: u32,
    /// 总数。
    pub total: TotalInfo,
    /// 本页是否还有下一页（服务端说的；越界页为 `false`）。
    pub has_more: bool,
    /// 本页实际返回的行数。
    pub rows: u64,
}

impl PagerState {
    /// 构造。
    #[must_use]
    pub fn new(offset: u64, page_size: u32, total: TotalInfo, has_more: bool, rows: u64) -> Self {
        Self {
            offset,
            page_size: page_size.max(1),
            total,
            has_more,
            rows,
        }
    }

    /// 页码（1 起）。
    #[must_use]
    pub fn page_index(self) -> u64 {
        self.offset / u64::from(self.page_size) + 1
    }

    /// 页数（`total` 未知时为 `None`——不编造页数）。
    #[must_use]
    pub fn page_count(self) -> Option<u64> {
        if !self.total.is_known() {
            return None;
        }
        let size = u64::from(self.page_size);
        if self.total.rows == 0 {
            return Some(1);
        }
        Some(self.total.rows.div_ceil(size))
    }

    /// 本页第一行的行号（1 起；本页为空时仍指向 `offset + 1`）。
    #[must_use]
    pub fn first_row(self) -> u64 {
        self.offset + 1
    }

    /// 本页最后一行的行号。
    #[must_use]
    pub fn last_row(self) -> u64 {
        self.offset + self.rows
    }

    /// 是否能回首页。
    #[must_use]
    pub fn can_first(self) -> bool {
        self.offset > 0
    }

    /// 是否能回上一页。
    #[must_use]
    pub fn can_previous(self) -> bool {
        self.offset > 0
    }

    /// 是否能去下一页。
    #[must_use]
    pub fn can_next(self) -> bool {
        self.has_more
    }

    /// 是否能去末页（需要知道总数）。
    #[must_use]
    pub fn can_last(self) -> bool {
        let Some(count) = self.page_count() else {
            return false;
        };
        count > self.page_index()
    }

    /// 首页 offset。
    #[must_use]
    pub fn first_offset(self) -> u64 {
        0
    }

    /// 上一页 offset。
    #[must_use]
    pub fn previous_offset(self) -> u64 {
        self.offset.saturating_sub(u64::from(self.page_size))
    }

    /// 下一页 offset（末页时保持原位）。
    #[must_use]
    pub fn next_offset(self) -> u64 {
        self.offset + u64::from(self.page_size)
    }

    /// 末页 offset（总数未知时 `None`）。
    #[must_use]
    pub fn last_offset(self) -> Option<u64> {
        let count = self.page_count()?;
        Some((count.saturating_sub(1)) * u64::from(self.page_size))
    }

    /// 页码文案：`第 1 / 3 页`（页数未知时只给当前页）。
    #[must_use]
    pub fn page_label(self) -> String {
        match self.page_count() {
            Some(count) => format!("第 {} / {} 页", self.page_index(), count.max(1)),
            None => format!("第 {} 页", self.page_index()),
        }
    }

    /// 记录位置：本页在表里的**行号区间**（`第 501–1000 行`）。
    ///
    /// 这里**不写页码、也不写总数**：页码与总数只在分页器旁出现一次（状态栏行 1），
    /// 两处都写就成了同一句话说三遍（旧版还在表格工具条下方写过一遍）。行号区间是
    /// 分页器给不出的信息——用户真正想知道的是「我现在看到第几行到第几行」。
    ///
    /// 本页为空时如实说明，不编造区间：首页空表就是「空表」，offset 越界说明 offset。
    #[must_use]
    pub fn position_text(self) -> String {
        if self.rows == 0 {
            return if self.offset > 0 {
                format!("本页无数据（offset {}）", self.offset)
            } else {
                "空表".to_owned()
            };
        }
        let (first, last) = (self.first_row(), self.last_row());
        if first == last {
            format!("第 {first} 行")
        } else {
            format!("第 {first}–{last} 行")
        }
    }
}

/// 一次浏览的状态：offset + 排序 + 筛选 + 总数。
///
/// 这是「排序/筛选必须回到第一页」的唯一实现点：改排序或筛选都会把 `offset`
/// 清 0，翻页只改 `offset`。worker 用它驱动请求，界面用它渲染分页器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowseSession {
    /// 起始行。
    pub offset: u64,
    /// 每页行数（沿用现有值）。
    pub page_size: u32,
    /// 排序。
    pub sort: SortOrder,
    /// 筛选。
    pub filter: FilterSet,
    /// 服务端给的排序保证（`RowOrder`，用于文案）。
    pub ordering: i32,
}

impl BrowseSession {
    /// 新会话（第一页，无排序，无筛选）。
    #[must_use]
    pub fn new(page_size: u32) -> Self {
        Self {
            offset: 0,
            page_size: page_size.max(1),
            sort: SortOrder::new(),
            filter: FilterSet::new(),
            ordering: 0,
        }
    }

    /// 是否在第一页。
    #[must_use]
    pub fn is_first_page(&self) -> bool {
        self.offset == 0
    }

    /// 换排序：**回到第一页**（排序变了，旧的 offset 没有意义）。
    pub fn set_sort(&mut self, sort: SortOrder) {
        self.sort = sort;
        self.offset = 0;
    }

    /// 换筛选：**回到第一页**。
    pub fn set_filter(&mut self, filter: FilterSet) {
        self.filter = filter;
        self.offset = 0;
    }

    /// 跳页（只改 offset；越界由服务端如实返回空行）。
    pub fn goto(&mut self, offset: u64) {
        self.offset = offset;
    }

    /// 分页器快照。
    #[must_use]
    pub fn pager(&self, total: TotalInfo, has_more: bool, rows: u64) -> PagerState {
        PagerState::new(self.offset, self.page_size, total, has_more, rows)
    }

    /// 只读摘要：`SELECT … FROM … WHERE … ORDER BY … LIMIT …, …`。
    ///
    /// 只用于展示，不执行：值以字面量形式呈现（实际请求参数化绑定）。
    #[must_use]
    pub fn sql_summary(&self, table: &TableEntry, columns: &[TableColumn]) -> String {
        let names = if columns.is_empty() {
            "*".to_owned()
        } else {
            columns
                .iter()
                .map(|column| quote_ident(&column.name))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut sql = format!("SELECT {names} FROM {}", quote_ident(&table.display_name));
        let where_clause = self.filter.describe();
        if !where_clause.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&where_clause);
        }
        let order = self.sort.describe();
        if !order.is_empty() {
            sql.push_str(" ORDER BY ");
            sql.push_str(&order);
        }
        sql.push_str(&format!(" LIMIT {}, {}", self.offset, self.page_size));
        sql
    }
}

/// 列显隐（只影响渲染，不影响导出/复制口径）。
///
/// 「可见列」的唯一来源：导出（#120）与复制走 [`Self::visible_indices`]，
/// 与网格渲染看到的是同一份清单。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ColumnVisibility {
    hidden: Vec<usize>,
}

impl ColumnVisibility {
    /// 全部可见。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 切换一列。
    pub fn toggle(&mut self, index: usize) {
        if let Some(position) = self.hidden.iter().position(|hidden| *hidden == index) {
            self.hidden.remove(position);
        } else {
            self.hidden.push(index);
            self.hidden.sort_unstable();
        }
    }

    /// 显式设置可见性。
    pub fn set_visible(&mut self, index: usize, visible: bool) {
        let hidden = self.hidden.contains(&index);
        if visible && hidden {
            self.hidden.retain(|hidden| *hidden != index);
        } else if !visible && !hidden {
            self.hidden.push(index);
            self.hidden.sort_unstable();
        }
    }

    /// 该列是否可见（未知下标按可见处理：不让新列无声消失）。
    #[must_use]
    pub fn is_visible(&self, index: usize) -> bool {
        !self.hidden.contains(&index)
    }

    /// 隐藏的列数。
    #[must_use]
    pub fn hidden_count(&self) -> usize {
        self.hidden.len()
    }

    /// 可见列下标（按原列顺序）。
    #[must_use]
    pub fn visible_indices(&self, column_count: usize) -> Vec<usize> {
        (0..column_count)
            .filter(|index| self.is_visible(*index))
            .collect()
    }

    /// 可见列数。
    #[must_use]
    pub fn visible_count(&self, column_count: usize) -> usize {
        self.visible_indices(column_count).len()
    }

    /// 重置（换表时调用：旧表的隐藏列不能带到新表）。
    pub fn reset(&mut self) {
        self.hidden.clear();
    }
}

/// 相对时间：`<1m` / `Nm` / `Nh` / `Nd`。
#[must_use]
pub fn relative_age(seconds: u64) -> String {
    if seconds < 60 {
        "<1m".to_owned()
    } else if seconds < 3_600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3_600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}

/// 给标识符加双引号（摘要用；值不经过这里）。
fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// 摘要里的值：数字/布尔按字面量，其它按单引号字符串（只用于展示）。
fn summary_value(value: &str) -> String {
    let trimmed = value.trim();
    let numeric = matches!(
        trimmed.parse::<f64>().ok(),
        Some(number) if number.is_finite()
    );
    if numeric || matches!(trimmed, "true" | "false") {
        return trimmed.to_owned();
    }
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::common::v1::{ColumnMeta, ColumnType};
    use crate::proto::services::v1::ObjectRef;

    fn column(name: &str, kind: i32, label: &str) -> TableColumn {
        TableColumn::from_meta(&ColumnMeta {
            name: name.to_owned(),
            ordinal: 0,
            r#type: Some(ColumnType {
                kind,
                original_type_name: label.to_owned(),
                precision: None,
                scale: None,
                length: None,
                unsigned: Some(false),
                bits: None,
                with_time_zone: None,
                element_type: None,
            }),
            nullable: true,
            default_value: None,
            character_set: None,
            collation: None,
        })
    }

    fn table(name: &str) -> TableEntry {
        TableEntry {
            reference: ObjectRef::default(),
            display_name: name.to_owned(),
            kind: crate::proto::services::v1::ObjectKind::Table,
        }
    }

    fn text_columns() -> Vec<TableColumn> {
        vec![
            column(
                "id",
                crate::proto::common::v1::ColumnTypeKind::ColumnTypeInteger as i32,
                "int",
            ),
            column(
                "name",
                crate::proto::common::v1::ColumnTypeKind::ColumnTypeText as i32,
                "varchar(64)",
            ),
        ]
    }

    // ---- 排序 ----

    #[test]
    fn header_click_cycles_ascending_descending_and_off() {
        let mut sort = SortOrder::new();
        sort.toggle("name", false);
        assert_eq!(sort.keys().len(), 1);
        assert_eq!(sort.keys()[0].direction, Direction::Ascending);
        sort.toggle("name", false);
        assert_eq!(sort.keys()[0].direction, Direction::Descending);
        sort.toggle("name", false);
        assert!(sort.is_empty(), "再点一次应当取消该列的排序");
    }

    #[test]
    fn plain_click_replaces_the_other_keys_but_shift_click_extends() {
        let mut sort = SortOrder::new();
        sort.toggle("name", false);
        sort.toggle("name", true);
        sort.toggle("id", true);
        assert_eq!(
            sort.keys()
                .iter()
                .map(|key| key.column.as_str())
                .collect::<Vec<_>>(),
            vec!["name", "id"]
        );
        assert_eq!(sort.marker("id"), SortMarker::Ascending { priority: 2 });
        // 不带 Shift 点第三列：只剩它自己。
        sort.toggle("name", false);
        assert_eq!(sort.keys().len(), 1);
        assert_eq!(sort.keys()[0].column, "name");
    }

    #[test]
    fn sort_proto_keeps_order_and_direction() {
        let mut sort = SortOrder::new();
        sort.toggle("id", false);
        sort.toggle("id", false);
        let keys = sort.to_proto();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].column, "id");
        assert_eq!(keys[0].direction, SortDirection::Desc as i32);
    }

    #[test]
    fn sort_marker_text_is_visible_and_ordered() {
        assert_eq!(SortMarker::None.text(), "");
        assert_eq!(SortMarker::Ascending { priority: 0 }.text(), "▲");
        assert_eq!(SortMarker::Descending { priority: 2 }.text(), "▼2");
    }

    #[test]
    fn panel_set_key_keeps_the_other_keys_and_updates_in_place() {
        let mut sort = SortOrder::new();
        sort.set_key("name", Direction::Ascending);
        sort.set_key("id", Direction::Descending);
        assert_eq!(
            sort.keys()
                .iter()
                .map(|key| (key.column.as_str(), key.direction))
                .collect::<Vec<_>>(),
            vec![
                ("name", Direction::Ascending),
                ("id", Direction::Descending)
            ],
            "面板里加排序不能像表头点击那样清空其它键"
        );
        // 同一列再设一次：就地改方向，次序不变（多列排序的优先级不能因为改方向跳位）。
        sort.set_key("name", Direction::Descending);
        assert_eq!(sort.keys()[0].column, "name");
        assert_eq!(sort.keys()[0].direction, Direction::Descending);
        assert_eq!(sort.keys().len(), 2);
    }

    #[test]
    fn panel_sort_rows_can_be_moved_and_removed() {
        let mut sort = SortOrder::new();
        sort.set_key("a", Direction::Ascending);
        sort.set_key("b", Direction::Ascending);
        sort.set_key("c", Direction::Ascending);
        sort.move_up(2);
        assert_eq!(
            sort.keys()
                .iter()
                .map(|key| key.column.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "c", "b"]
        );
        // 首位上移 / 末位下移都是空操作，不 panic。
        sort.move_up(0);
        sort.move_down(2);
        assert_eq!(sort.keys()[0].column, "a");
        assert_eq!(sort.keys()[2].column, "b");
        sort.remove(1);
        assert_eq!(
            sort.keys()
                .iter()
                .map(|key| key.column.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        sort.remove(9);
        sort.clear();
        assert!(sort.is_empty());
    }

    #[test]
    fn sort_marker_shows_priority_for_multi_column_sorts() {
        let mut sort = SortOrder::new();
        sort.set_key("id", Direction::Descending);
        sort.set_key("name", Direction::Ascending);
        assert_eq!(sort.marker("id"), SortMarker::Descending { priority: 1 });
        assert_eq!(sort.marker("name"), SortMarker::Ascending { priority: 2 });
        sort.remove(0);
        assert_eq!(
            sort.marker("name"),
            SortMarker::Ascending { priority: 0 },
            "只剩一列时不显示次序数字"
        );
    }

    // ---- 筛选 ----

    #[test]
    fn filter_builds_and_conditions_with_typed_values() {
        let columns = text_columns();
        let mut filter = FilterSet::new();
        filter.push(Condition {
            column: "id".to_owned(),
            operator: Operator::Ge,
            value: Some("10".to_owned()),
        });
        filter.push(Condition {
            column: "name".to_owned(),
            operator: Operator::Like,
            value: Some("ab%".to_owned()),
        });
        let proto = filter
            .to_proto(&columns)
            .expect("应当能构造")
            .expect("非空");
        assert_eq!(proto.conditions.len(), 2);
        assert_eq!(proto.conditions[0].operator, ProtoFilterOperator::Ge as i32);
        assert_eq!(
            proto.conditions[0].value.as_ref().map(|value| value.kind),
            Some(ValueKind::Int64 as i32)
        );
        // LIKE 一律文本，即使列是数值列也不做数值解析。
        assert_eq!(
            proto.conditions[1].value.as_ref().map(|value| value.kind),
            Some(ValueKind::Text as i32)
        );
        assert_eq!(filter.describe(), "\"id\" >= 10 AND \"name\" LIKE 'ab%'");
    }

    #[test]
    fn filter_rejects_unknown_columns_and_incompatible_values() {
        let columns = text_columns();
        let mut filter = FilterSet::new();
        filter.push(Condition {
            column: "missing".to_owned(),
            operator: Operator::Eq,
            value: Some("1".to_owned()),
        });
        let error = filter.to_proto(&columns).expect_err("未知列必须报错");
        assert!(error.contains("未知列"), "{error}");

        let mut filter = FilterSet::new();
        filter.push(Condition {
            column: "id".to_owned(),
            operator: Operator::Lt,
            value: Some("abc".to_owned()),
        });
        let error = filter.to_proto(&columns).expect_err("类型不兼容必须报错");
        assert!(error.contains("不兼容"), "{error}");
    }

    #[test]
    fn null_operators_need_no_value() {
        let columns = text_columns();
        let mut filter = FilterSet::new();
        filter.push(Condition {
            column: "name".to_owned(),
            operator: Operator::IsNull,
            value: None,
        });
        let proto = filter
            .to_proto(&columns)
            .expect("应当能构造")
            .expect("非空");
        assert!(proto.conditions[0].value.is_none());
        assert!(!Operator::IsNull.needs_value());
        assert!(Operator::Eq.needs_value());
    }

    #[test]
    fn empty_filter_is_none_not_an_empty_message() {
        let filter = FilterSet::new();
        assert!(
            filter
                .to_proto(&text_columns())
                .expect("空集合法")
                .is_none()
        );
        assert_eq!(filter.describe(), "");
    }

    #[test]
    fn replace_edits_a_condition_in_place_and_refuses_duplicates() {
        let mut filter = FilterSet::new();
        filter.push(Condition {
            column: "name".to_owned(),
            operator: Operator::Like,
            value: Some("ab%".to_owned()),
        });
        filter.push(Condition {
            column: "id".to_owned(),
            operator: Operator::Ge,
            value: Some("10".to_owned()),
        });
        // 就地改第一条：次序不变，内容变了。
        filter
            .replace(
                0,
                Condition {
                    column: "name".to_owned(),
                    operator: Operator::Like,
                    value: Some("xy%".to_owned()),
                },
            )
            .expect("同列同算子的自我替换不是重复");
        assert_eq!(filter.conditions()[0].value.as_deref(), Some("xy%"));
        assert_eq!(filter.conditions()[1].column, "id");
        // 改成和另一条同列同算子：拒绝，并给出可读原因（不是静默吞掉）。
        let error = filter
            .replace(
                0,
                Condition {
                    column: "id".to_owned(),
                    operator: Operator::Ge,
                    value: Some("20".to_owned()),
                },
            )
            .expect_err("与另一条重复应当被拒绝");
        assert!(error.contains("已经有同样的条件"), "实际：{error}");
        // 越界：给出可读原因，不 panic。
        assert!(
            filter
                .replace(
                    9,
                    Condition {
                        column: "id".to_owned(),
                        operator: Operator::Eq,
                        value: Some("1".to_owned()),
                    },
                )
                .is_err()
        );
    }

    // ---- 总数与分页 ----

    #[test]
    fn total_text_never_promotes_an_estimate_to_an_exact_number() {
        let exact = TotalInfo::from_proto(TotalKind::Exact as i32, 42, false);
        assert_eq!(exact.text().as_deref(), Some("共 42 行"));
        let estimate = TotalInfo::from_proto(TotalKind::Estimate as i32, 4200, false);
        assert_eq!(estimate.text().as_deref(), Some("约 4200 行"));
        assert!(!estimate.text().expect("估计值有文案").contains("共 "));
        assert_eq!(TotalInfo::unknown().text(), None);
        let skipped = TotalInfo::from_proto(TotalKind::Unknown as i32, 0, true);
        assert_eq!(skipped.text().as_deref(), Some("未统计（超出计数预算）"));
    }

    #[test]
    fn pager_math_covers_first_previous_next_and_last() {
        let total = TotalInfo::from_proto(TotalKind::Exact as i32, 1_200, false);
        let pager = PagerState::new(500, 500, total, true, 500);
        assert_eq!(pager.page_index(), 2);
        assert_eq!(pager.page_count(), Some(3));
        assert_eq!(pager.page_label(), "第 2 / 3 页");
        assert!(pager.can_first() && pager.can_previous() && pager.can_next() && pager.can_last());
        assert_eq!(pager.previous_offset(), 0);
        assert_eq!(pager.next_offset(), 1_000);
        assert_eq!(pager.last_offset(), Some(1_000));
        assert_eq!(pager.first_row(), 501);
        assert_eq!(pager.last_row(), 1_000);

        let last = PagerState::new(1_000, 500, total, false, 200);
        assert!(!last.can_next(), "末页 has_more=false 时不能再下一页");
        assert!(!last.can_last());
        assert_eq!(last.page_index(), 3);
    }

    #[test]
    fn unknown_total_does_not_invent_page_counts_or_last_page() {
        let pager = PagerState::new(0, 500, TotalInfo::unknown(), true, 500);
        assert_eq!(pager.page_count(), None);
        assert_eq!(pager.page_label(), "第 1 页");
        assert!(!pager.can_last(), "总数未知时不给末页");
        assert_eq!(pager.last_offset(), None);
        assert_eq!(pager.position_text(), "第 1–500 行");
    }

    #[test]
    fn position_text_is_a_row_span_without_repeating_page_or_total() {
        // 行号区间是记录位置唯一的口径：页码与总数只在分页器旁出现一次。
        let exact = TotalInfo::from_proto(TotalKind::Exact as i32, 1200, false);
        assert_eq!(
            PagerState::new(0, 500, exact, true, 500).position_text(),
            "第 1–500 行"
        );
        assert_eq!(
            PagerState::new(500, 500, exact, true, 500).position_text(),
            "第 501–1000 行"
        );
        let estimate = TotalInfo::from_proto(TotalKind::Estimate as i32, 1200, false);
        assert_eq!(
            PagerState::new(1000, 500, estimate, false, 200).position_text(),
            "第 1001–1200 行"
        );
        // 单行页不写区间。
        assert_eq!(
            PagerState::new(0, 500, TotalInfo::unknown(), false, 1).position_text(),
            "第 1 行"
        );
        // 空页 / 空表如实说明，不编造区间。
        assert_eq!(
            PagerState::new(0, 500, TotalInfo::unknown(), false, 0).position_text(),
            "空表"
        );
        assert_eq!(
            PagerState::new(1500, 500, exact, false, 0).position_text(),
            "本页无数据（offset 1500）"
        );
    }

    #[test]
    fn sorting_and_filtering_reset_to_the_first_page() {
        let mut session = BrowseSession::new(500);
        session.goto(1_500);
        assert_eq!(session.offset, 1_500);
        let mut sort = SortOrder::new();
        sort.toggle("name", false);
        session.set_sort(sort);
        assert_eq!(session.offset, 0, "换排序后 offset 必须回到 0");

        session.goto(1_000);
        let mut filter = FilterSet::new();
        filter.push(Condition {
            column: "name".to_owned(),
            operator: Operator::Eq,
            value: Some("x".to_owned()),
        });
        session.set_filter(filter);
        assert_eq!(session.offset, 0, "换筛选后 offset 必须回到 0");
    }

    #[test]
    fn summary_reads_like_a_select_with_limit() {
        let mut session = BrowseSession::new(500);
        session.goto(1_000);
        let mut sort = SortOrder::new();
        sort.toggle("name", false);
        session.set_sort(sort);
        session.goto(1_000);
        let mut filter = FilterSet::new();
        filter.push(Condition {
            column: "name".to_owned(),
            operator: Operator::Like,
            value: Some("a%".to_owned()),
        });
        session.filter = filter;
        // 注意：直接改 filter 不重置 offset（set_filter 才重置），摘要按当前 offset。
        let sql = session.sql_summary(&table("items"), &text_columns());
        assert_eq!(
            sql,
            "SELECT \"id\", \"name\" FROM \"items\" WHERE \"name\" LIKE 'a%' ORDER BY \"name\" ASC LIMIT 1000, 500"
        );
        assert!(sql.starts_with("SELECT"));
    }

    #[test]
    fn summary_without_sort_or_filter_is_still_a_complete_select() {
        let session = BrowseSession::new(100);
        let sql = session.sql_summary(&table("items"), &text_columns());
        assert_eq!(sql, "SELECT \"id\", \"name\" FROM \"items\" LIMIT 0, 100");
    }

    // ---- 列显隐 / 能力位 / 相对时间 ----

    #[test]
    fn column_visibility_keeps_a_queryable_visible_list() {
        let mut visibility = ColumnVisibility::new();
        assert_eq!(visibility.visible_indices(3), vec![0, 1, 2]);
        visibility.toggle(1);
        assert!(!visibility.is_visible(1));
        assert_eq!(visibility.visible_indices(3), vec![0, 2]);
        assert_eq!(visibility.hidden_count(), 1);
        visibility.set_visible(1, true);
        assert_eq!(visibility.visible_indices(3), vec![0, 1, 2]);
        visibility.set_visible(2, false);
        visibility.reset();
        assert_eq!(visibility.visible_count(3), 3);
    }

    #[test]
    fn capability_gate_hides_every_entry_when_the_driver_says_no() {
        let supported =
            BrowseCapabilities::from_names(&["table_browse_sort_filter_total".to_owned()]);
        assert!(supported.sorting() && supported.filtering() && supported.totals());
        let unsupported = BrowseCapabilities::from_names(&["transactions".to_owned()]);
        assert!(!unsupported.sorting());
        assert!(!unsupported.filtering());
        assert!(!unsupported.totals());
        assert!(!BrowseCapabilities::none().sort_filter_total);
        assert!(BrowseCapabilities::all().sort_filter_total);
    }

    #[test]
    fn relative_age_is_minutes_below_an_hour() {
        assert_eq!(relative_age(0), "<1m");
        assert_eq!(relative_age(59), "<1m");
        assert_eq!(relative_age(60), "1m");
        assert_eq!(relative_age(3_599), "59m");
        assert_eq!(relative_age(3_600), "1h");
        assert_eq!(relative_age(90_000), "1d");
    }
}
