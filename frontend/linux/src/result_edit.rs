//! 查询结果的单表编辑（v0.2.5）。
//!
//! 查询标签的结果区默认只读。当结果来自**唯一顶层 `FROM`**、且结果列覆盖了目标表的
//! 主键时，它其实等价于「一张表的当前页」——这里把这份结果接上数据页同一套编辑引擎
//! （[`TableDraft`] / [`InsertDraft`]），于是状态栏的 `+ - ✓ ↶` 在查询标签里也能用。
//!
//! # 边界（刻意不做猜测）
//!
//! - JOIN / 聚合 / 子查询 / 多个顶层 FROM：`table_from_sql` 返回 `None`，编辑整体禁用，
//!   状态栏 tooltip 说明「结果不是单表查询」。
//! - 结果集不含主键列：禁用（没有主键就定位不到要改的那一行）。
//! - 目标表解析不到（名字是 CTE 别名 / 临时表 / 视图）：禁用并在 tooltip 里写明原因。
//! - 只对**终态**结果启用（读完 / 触顶 / 取消）：还在流式加载时行集还会增长，草稿会对不上。
//!
//! 显示口径：改过的单元格按当前值重绘（与数据页一致），但没有行级脏色/删除线——
//! 那需要结果区的行模型带上草稿状态；本版把「脏行 N / 已标记删除 N 行」放在状态圆点
//! 与状态行里，先保证提交语义正确。

use std::cell::RefCell;

use crate::backend::query::{RowView, format_cell};
use crate::backend::table::{TableColumn, TableRow};
use crate::proto::common::v1::ColumnValue;
use crate::proto::services::v1::ObjectRef;
use crate::table_data::{InsertDraft, RiskAssessment, RowFailure, TableDraft};

/// 查询结果的编辑态（一份结果集一个）。
pub struct ResultEdit {
    /// 目标表（`session_id` 必须是**本标签自己的会话**：事务与提交都走它）。
    pub object: ObjectRef,
    /// 列（与结果列一一对应，名字来自结果集、类型来自结果集的类型标签）。
    pub columns: Vec<TableColumn>,
    /// 主键列名（升序按主键位置）。
    pub primary_key: Vec<String>,
    draft: RefCell<TableDraft>,
    /// 「新增行」草稿：同一时刻最多一行（与数据页一致）。
    insert: RefCell<Option<InsertDraft>>,
}

impl ResultEdit {
    /// 由已加载的结果行建立编辑态（只在终态调用）。
    ///
    /// `columns` 必须与 `rows` 的值顺序一致；`primary_key` 必须都能在 `columns` 里找到
    /// ——调用方负责先校验（见 [`Self::pk_coverage_gap`]）。
    pub fn build(
        object: ObjectRef,
        columns: Vec<TableColumn>,
        primary_key: Vec<String>,
        rows: &[RowView],
    ) -> Self {
        let table_rows: Vec<TableRow> = rows
            .iter()
            .map(|row| TableRow {
                number: row.number,
                values: row.values.clone(),
                cells: row.cells.clone(),
                edit_texts: row
                    .values
                    .iter()
                    .map(crate::table_data::edit_text)
                    .collect(),
            })
            .collect();
        let draft = TableDraft::new(columns.clone(), primary_key.clone(), &table_rows, None);
        Self {
            object,
            columns,
            primary_key,
            draft: RefCell::new(draft),
            insert: RefCell::new(None),
        }
    }

    /// 结果列是否覆盖了所有主键列；缺哪一列就返回那一列的名字。
    ///
    /// 没有主键列就没法 `WHERE 主键 = …`，编辑与删除都必须关掉（不是「尽量」）。
    #[must_use]
    pub fn pk_coverage_gap(column_names: &[String], primary_key: &[String]) -> Option<String> {
        primary_key
            .iter()
            .find(|key| !column_names.iter().any(|name| name == *key))
            .cloned()
    }

    /// 脏行数（含标记删除）。
    #[must_use]
    pub fn dirty_rows(&self) -> usize {
        let draft = self.draft.borrow();
        let inserted = usize::from(self.insert.borrow().is_some());
        draft.dirty_rows() + inserted
    }

    /// 待提交条数（脏行 + 新行）。
    #[must_use]
    pub fn pending(&self) -> usize {
        let draft = self.draft.borrow();
        let inserted = usize::from(self.insert.borrow().is_some());
        draft.risk().total() + inserted
    }

    /// 风险判定（行数阈值 / 是否含删除），与数据页同一份。
    #[must_use]
    pub fn risk(&self) -> RiskAssessment {
        self.draft.borrow().risk()
    }

    /// 是否有待提交的新行。
    #[must_use]
    pub fn has_insert(&self) -> bool {
        self.insert.borrow().is_some()
    }

    /// 当前显示值（草稿里的当前值；越界返回 `None`）。
    #[must_use]
    pub fn current_values(&self, row: usize) -> Option<Vec<ColumnValue>> {
        self.draft
            .borrow()
            .rows()
            .get(row)
            .map(|draft_row| draft_row.current.clone())
    }

    /// 按草稿当前值重建一行的显示（提交前网格里看到的总是编辑器里的值）。
    #[must_use]
    pub fn row_view(&self, row: usize) -> Option<RowView> {
        let draft = self.draft.borrow();
        let draft_row = draft.rows().get(row)?;
        Some(RowView {
            number: draft_row.number,
            cells: draft_row.current.iter().map(format_cell).collect(),
            values: draft_row.current.clone(),
        })
    }

    /// 用文本改一个单元格。
    pub fn set_text(&self, row: usize, column: usize, text: &str) -> Result<(), String> {
        self.draft
            .borrow_mut()
            .set_cell_from_text(row, column, text)
    }

    /// 把单元格显式设为 NULL。
    pub fn set_null(&self, row: usize, column: usize) -> Result<(), String> {
        self.draft.borrow_mut().set_cell_null(row, column)
    }

    /// 标记删除若干行（不是立刻写库）。
    pub fn delete(&self, rows: &[usize]) -> Result<usize, String> {
        let mut draft = self.draft.borrow_mut();
        let mut count = 0;
        for row in rows {
            draft.delete_row(*row)?;
            count += 1;
        }
        Ok(count)
    }

    /// 放弃全部未提交的编辑与删除标记，并清掉新行草稿。
    pub fn discard(&self) {
        self.draft.borrow_mut().discard();
        self.insert.borrow_mut().take();
    }

    /// 是否可以开始一笔新行草稿（已有新行或已有脏行时不允许）。
    pub fn can_insert(&self) -> bool {
        self.insert.borrow().is_none() && self.draft.borrow().dirty_rows() == 0
    }

    /// 开始一笔新行草稿（所有列先按「省略，交给数据库默认值」）。
    pub fn begin_insert(&self) -> Result<(), String> {
        if self.insert.borrow().is_some() {
            return Err("已经有一行待插入的新行：先提交或放弃它".to_owned());
        }
        self.insert
            .borrow_mut()
            .replace(InsertDraft::new(self.columns.clone()));
        Ok(())
    }

    /// 新行某个单元格的显示（`None` = 没有新行）。
    #[must_use]
    pub fn insert_cell_view(&self, column: usize) -> Option<crate::table_data::InsertCellView> {
        let insert = self.insert.borrow();
        let insert = insert.as_ref()?;
        Some(insert.view(column))
    }

    /// 改新行的某个单元格（文本 / NULL）。
    pub fn set_insert_text(&self, column: usize, text: &str) -> Result<(), String> {
        let mut insert = self.insert.borrow_mut();
        let Some(insert) = insert.as_mut() else {
            return Err("还没有待插入的新行".to_owned());
        };
        insert.set_text(column, text)
    }

    /// 把新行的某个单元格设为 NULL。
    pub fn set_insert_null(&self, column: usize) -> Result<(), String> {
        let mut insert = self.insert.borrow_mut();
        let Some(insert) = insert.as_mut() else {
            return Err("还没有待插入的新行".to_owned());
        };
        insert.set_null(column)
    }

    /// 构建提交批次（更新 / 删除 + 新行）。
    pub fn batch(
        &self,
    ) -> Result<
        (
            crate::table_data::ChangeBatch,
            Option<crate::proto::services::v1::InsertRow>,
        ),
        String,
    > {
        let batch = self.draft.borrow().build_batch();
        let insert_row = match self.insert.borrow().as_ref() {
            Some(insert) => Some(insert.to_row()?),
            None => None,
        };
        Ok((batch, insert_row))
    }

    /// 提交结果回填：成功的行清掉脏标记，失败的逐行留在草稿里。
    pub fn apply_results(
        &self,
        batch: &crate::table_data::ChangeBatch,
        results: &[crate::proto::services::v1::RowChangeResult],
    ) -> Vec<RowFailure> {
        self.draft.borrow_mut().apply_results(batch, results)
    }
}

#[cfg(test)]
mod tests {
    use super::ResultEdit;
    use crate::backend::query::{CellStyle, CellView, RowView};
    use crate::backend::table::TableColumn;
    use crate::column_info::ColumnDefaultState;
    use crate::proto::common::v1::{ColumnValue, ValueKind, column_value};
    use crate::proto::services::v1::{ObjectKind, ObjectRef};

    fn int(value: i64) -> ColumnValue {
        ColumnValue {
            kind: ValueKind::Int64 as i32,
            value: Some(column_value::Value::Int64Value(value)),
            truncated: false,
            original_type_name: "integer".to_owned(),
        }
    }

    fn text(value: &str) -> ColumnValue {
        ColumnValue {
            kind: ValueKind::Text as i32,
            value: Some(column_value::Value::TextValue(value.to_owned())),
            truncated: false,
            original_type_name: "text".to_owned(),
        }
    }

    fn column(name: &str, ordinal: u32, primary: bool) -> TableColumn {
        TableColumn {
            name: name.to_owned(),
            ordinal,
            kind: crate::proto::common::v1::ColumnTypeKind::ColumnTypeText,
            type_label: "text".to_owned(),
            nullable: true,
            unsigned: false,
            with_time_zone: false,
            scale: None,
            default_value: ColumnDefaultState::Unknown,
            character_set: None,
            collation: None,
            primary_key_position: primary.then_some(1),
        }
    }

    fn row(number: u64, values: Vec<ColumnValue>) -> RowView {
        RowView {
            number,
            cells: values
                .iter()
                .map(|_| CellView {
                    text: String::new(),
                    style: CellStyle::Normal,
                    tooltip: String::new(),
                    truncated: false,
                })
                .collect(),
            values,
        }
    }

    fn object() -> ObjectRef {
        ObjectRef {
            session_id: "sess".to_owned(),
            catalog: "main".to_owned(),
            schema: String::new(),
            name: "users".to_owned(),
            kind: ObjectKind::Table as i32,
        }
    }

    fn edit() -> ResultEdit {
        ResultEdit::build(
            object(),
            vec![column("id", 0, true), column("name", 1, false)],
            vec!["id".to_owned()],
            &[
                row(1, vec![int(1), text("张三")]),
                row(2, vec![int(2), text("李四")]),
            ],
        )
    }

    #[test]
    fn edit_then_delete_produces_one_update_and_one_delete() {
        let edit = edit();
        edit.set_text(0, 1, "王五").expect("改值");
        edit.delete(&[1]).expect("标删");
        assert_eq!(edit.dirty_rows(), 2);
        let (batch, insert) = edit.batch().expect("构建批次");
        assert!(insert.is_none());
        assert_eq!(batch.updates.len(), 1);
        assert_eq!(batch.deletes.len(), 1);
        // 定位用的是**原始**主键值，不是改后值。
        let key = batch.updates[0].key.as_ref().expect("有主键");
        assert_eq!(key.values.len(), 1);
        assert_eq!(key.values[0].column, "id");
        assert_eq!(key.values[0].value.as_ref(), Some(&int(1)));
    }

    #[test]
    fn discard_clears_everything() {
        let edit = edit();
        edit.set_text(0, 1, "王五").expect("改值");
        edit.delete(&[1]).expect("标删");
        edit.discard();
        assert_eq!(edit.dirty_rows(), 0);
        let (batch, _) = edit.batch().expect("构建批次");
        assert!(batch.is_empty());
    }

    #[test]
    fn insert_draft_only_carries_the_columns_that_were_filled() {
        let edit = edit();
        assert!(edit.can_insert());
        edit.begin_insert().expect("开始新行");
        assert!(edit.has_insert());
        edit.set_insert_text(1, "新行").expect("填值");
        let (batch, insert) = edit.batch().expect("构建批次");
        assert!(batch.is_empty());
        let insert = insert.expect("有新行");
        assert_eq!(insert.values.len(), 1, "只带填过的列");
    }

    #[test]
    fn pk_coverage_reports_the_missing_column() {
        assert_eq!(
            ResultEdit::pk_coverage_gap(&["id".to_owned(), "name".to_owned()], &["id".to_owned()]),
            None
        );
        assert_eq!(
            ResultEdit::pk_coverage_gap(&["name".to_owned()], &["id".to_owned()]).as_deref(),
            Some("id")
        );
    }
}
