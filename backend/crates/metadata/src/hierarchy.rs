//! 对象层级展开的公共逻辑：稳定排序、子节点键与分页（v0.2.2，#82）。
//!
//! `ListNodes` 从 v0.2.2 起要按层展开（Catalog → 表/视图/序列 → 列/索引/触发器）。
//! 两个驱动的子节点来自不同的系统表（`sqlite_master` / `information_schema`），
//! 但下面三件事必须完全一致，否则同一个不透明 token 在两个驱动上的语义就会不同：
//!
//! 1. **顺序**：先按种类（契约 `ObjectKind` 的声明顺序），再按限定名。调用方只需
//!    原样回传 token，不必自己排序；
//! 2. **键唯一**：一张表的子节点里可以同时有名为 `x` 的列与名为 `x` 的索引。键必须
//!    带种类，否则两个不同对象会产出同一个分页键，token 指纹就分不清「列 `x`」与
//!    「索引 `x`」——快照校验形同虚设；
//! 3. **作用域进指纹**：`main` 与 `temp` 里可以有同名的表。父节点标识折进每个键，
//!    于是「从 A 拿到的 token 拿去翻 B 的页」会因为指纹不同被拒绝
//!    （`metadata.snapshot_changed`），而不是按偏移量切出一页错位的数据。
//!
//! 为什么不用 `LIMIT/OFFSET` 服务端分页：SQLite 与 MariaDB 的目录/列/索引查询都
//! 要跨 `sqlite_master`、`pragma_*`、`information_schema.*` 三类来源，且对象集合在
//! 翻页期间可能变化。v0.2.1 的选择（读当前层全集 + 名字集合指纹）在 v0.2.2 沿用到
//! 每一层，代价与替代方案见 ADR 0019 的「重新评估条件」。

use sqarvo_common::Result;
use sqarvo_driver_api::{ObjectKind, ObjectNode, Page, PageRequest};

use crate::pagination::paginate_by_key;

/// 子节点排序权重：契约里 `ObjectKind` 的声明顺序，末尾是数据库特有的种类。
///
/// 与 `ObjectFilter` 指纹用的顺序一致：同一次展开在两次调用之间必须给出同一个顺序，
/// 否则「翻页无重无漏」无从谈起。
#[must_use]
pub fn kind_order(kind: &ObjectKind) -> u8 {
    match kind {
        ObjectKind::Catalog => 1,
        ObjectKind::Schema => 2,
        ObjectKind::Table => 3,
        ObjectKind::View => 4,
        ObjectKind::MaterializedView => 5,
        ObjectKind::Column => 6,
        ObjectKind::Index => 7,
        ObjectKind::Sequence => 8,
        ObjectKind::Function => 9,
        ObjectKind::Procedure => 10,
        ObjectKind::Trigger => 11,
        ObjectKind::Type => 12,
        ObjectKind::Other(_) => 13,
    }
}

/// 按（种类、限定名）稳定排序；同一 `(种类, 名称)` 的重复项只保留第一个。
///
/// 列/索引/触发器混在一层时，名称可能跨种类重名；排序键必须包含种类，否则顺序
/// 依赖数据库返回行的顺序，翻页结果不稳定。
pub fn sort_children(nodes: &mut Vec<ObjectNode>) {
    nodes.sort_by(|left, right| {
        kind_order(&left.kind)
            .cmp(&kind_order(&right.kind))
            .then_with(|| {
                left.qualified_name
                    .to_string()
                    .cmp(&right.qualified_name.to_string())
            })
    });
    nodes.dedup_by(|left, right| {
        left.kind == right.kind && left.qualified_name == right.qualified_name
    });
}

/// 子节点在分页键里的稳定标识：`<作用域>␟<种类>:<限定名>`。
///
/// 种类是键的一部分：同一张表的列与索引可以同名，去掉种类会让两个不同对象撞到
/// 同一个键，指纹就无法区分「列 `x` 变成索引 `x`」这类变化。
///
/// `␟`（U+001F）不会出现在正常的对象名里，用它分隔作用域与对象名，避免
/// `a␟b` 与 `a`+`b` 这类拼接撞车（`paginate_by_key` 的指纹本身也用同一分隔符）。
#[must_use]
pub fn child_key(scope: &str, node: &ObjectNode) -> String {
    format!(
        "{scope}\u{1f}{}:{}",
        node.kind.as_str(),
        node.qualified_name
    )
}

/// 对某一层的子节点排序并分页。
///
/// `scope` 是父节点的稳定标识（根为 `root:<当前库>`，目录为 `catalog:<库名>`，
/// 表/视图为 `table:<库名>.<对象名>`）。它只进指纹，不出现在 token 明文里——
/// token 仍然是不透明字符串，格式与 v0.2.1 相同（`v1:<offset>:<16 位指纹>`）。
///
/// # Errors
///
/// - token 结构非法、版本未知、偏移量越界：`config.invalid`（`INVALID_ARGUMENT`）；
/// - token 的指纹与本次集合（含父节点作用域）不符：`metadata.snapshot_changed`
///   （`FAILED_PRECONDITION`），调用方应从第一页重读。
pub fn paginate_children(
    mut nodes: Vec<ObjectNode>,
    scope: &str,
    page: &PageRequest,
) -> Result<Page<ObjectNode>> {
    sort_children(&mut nodes);
    let keyed: Vec<(String, ObjectNode)> = nodes
        .into_iter()
        .map(|node| (child_key(scope, &node), node))
        .collect();
    let paged = paginate_by_key(keyed, page, |(key, _)| key.as_str())?;
    Ok(Page {
        items: paged.items.into_iter().map(|(_, node)| node).collect(),
        next_page_token: paged.next_page_token,
        truncated: paged.truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqarvo_driver_api::QualifiedName;

    fn node(database: &str, kind: ObjectKind, name: &str) -> ObjectNode {
        ObjectNode {
            qualified_name: QualifiedName::new(Some(database.to_owned()), None, name),
            kind,
            display_name: name.to_owned(),
            has_children: false,
            comment: None,
        }
    }

    fn page(size: u32, token: Option<String>) -> PageRequest {
        PageRequest {
            page_size: size,
            page_token: token,
        }
    }

    /// 混合种类、且列与索引同名：翻页必须无重复、无遗漏。
    #[test]
    fn mixed_kinds_with_duplicate_names_page_without_gaps() {
        let mut nodes = Vec::new();
        for index in 1..=4 {
            nodes.push(node("main", ObjectKind::Column, &format!("c{index}")));
        }
        for index in 1..=4 {
            nodes.push(node("main", ObjectKind::Index, &format!("c{index}")));
        }
        // 列与索引同名，单靠名字做键会撞车。
        nodes.push(node("main", ObjectKind::Column, "shared"));
        nodes.push(node("main", ObjectKind::Index, "shared"));

        let mut collected = Vec::new();
        let mut token = None;
        let mut pages = 0;
        loop {
            let page = paginate_children(nodes.clone(), "table:main.t", &page(3, token.clone()))
                .expect("分页");
            assert!(page.items.len() <= 3);
            for item in &page.items {
                let key = child_key("table:main.t", item);
                assert!(
                    !collected.contains(&key),
                    "分页键重复：两种不同对象撞到同一个键 {key}"
                );
                collected.push(key);
            }
            pages += 1;
            assert!(pages <= 20, "分页未收敛");
            match page.next_page_token {
                Some(next) => token = Some(next),
                None => break,
            }
        }
        assert_eq!(collected.len(), nodes.len(), "翻页必须无遗漏");
        // 列排在索引之前（种类顺序），同名对象各自出现一次。
        assert_eq!(pages, 4, "10 个对象按每页 3 个应分 4 页");
    }

    #[test]
    fn ordering_is_kind_then_name() {
        let mut nodes = vec![
            node("main", ObjectKind::Index, "a"),
            node("main", ObjectKind::Table, "z"),
            node("main", ObjectKind::Column, "b"),
            node("main", ObjectKind::Column, "a"),
        ];
        sort_children(&mut nodes);
        let got: Vec<(&str, &str)> = nodes
            .iter()
            .map(|node| (node.kind.as_str(), node.display_name.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("table", "z"),
                ("column", "a"),
                ("column", "b"),
                ("index", "a")
            ]
        );
    }

    /// 同一个 token 不能跨父节点复用：作用域不同 → 指纹不同。
    #[test]
    fn token_from_another_scope_is_rejected() {
        let nodes = vec![
            node("main", ObjectKind::Table, "a"),
            node("main", ObjectKind::Table, "b"),
        ];
        let first = paginate_children(nodes.clone(), "catalog:main", &page(1, None)).unwrap();
        let token = first.next_page_token.expect("应返回 token");
        let error = paginate_children(nodes, "catalog:temp", &page(1, Some(token)))
            .expect_err("跨作用域必须拒绝");
        assert_eq!(
            error.code(),
            sqarvo_common::ErrorCode::from_static("metadata.snapshot_changed")
        );
    }

    #[test]
    fn duplicate_rows_collapse_to_one_object() {
        let nodes = vec![
            node("main", ObjectKind::Index, "dup"),
            node("main", ObjectKind::Index, "dup"),
        ];
        let page = paginate_children(nodes, "catalog:main", &page(10, None)).unwrap();
        assert_eq!(page.items.len(), 1, "同一对象不能出现两次");
    }
}
