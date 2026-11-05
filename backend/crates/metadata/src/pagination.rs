//! 不透明分页 token 的生成与校验。
//!
//! 分页类型与上限定义在 Driver SPI 中，供驱动直接实现元数据接口。

use sqarvo_common::{Error, ErrorCode, Result};
use sqarvo_driver_api::ObjectFilter;

pub use sqarvo_driver_api::{DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE, Page, PageRequest};

/// 对内存中的有序集合分页。
///
/// token 为不透明字符串（当前实现是偏移量）。调用方只允许原样回传，
/// 解析失败一律视为 `invalid_argument`，而不是默默返回第一页。
pub fn paginate<T>(items: Vec<T>, request: &PageRequest) -> Result<Page<T>> {
    let page_size = usize::try_from(request.effective_page_size()).map_err(|_| {
        Error::invalid_argument(ErrorCode::CONFIG_INVALID, "分页大小超出可表示范围")
    })?;

    let offset = match request.page_token.as_deref() {
        None | Some("") => 0,
        Some(token) => token
            .parse::<usize>()
            .map_err(|_| Error::invalid_argument(ErrorCode::CONFIG_INVALID, "分页 token 非法"))?,
    };

    if offset > items.len() {
        return Err(Error::invalid_argument(
            ErrorCode::CONFIG_INVALID,
            "分页 token 超出数据范围",
        ));
    }

    let mut items = items;
    let remaining = items.split_off(offset);
    let mut page_items = remaining;
    let next_page_token = if page_items.len() > page_size {
        page_items.truncate(page_size);
        Some((offset + page_size).to_string())
    } else {
        None
    };

    let truncated = next_page_token.is_some();
    Ok(Page {
        items: page_items,
        next_page_token,
        truncated,
    })
}

/// 快照分页 token 的版本前缀。
const SNAPSHOT_TOKEN_PREFIX: &str = "v1";

/// 带过滤条件的分页 token 版本前缀（v0.2.2，#84）。
///
/// 过滤结果的 token 必须能自证「属于哪一套过滤条件」，所以在集合指纹之外多带一个
/// 条件标签，格式 `v2:<offset>:<16 位指纹>:<16 位条件标签>`。版本号不同而不是给
/// v1 追加字段：老 token 与新 token 不能互相冒充，解析失败一律 `config.invalid`。
const FILTERED_TOKEN_PREFIX: &str = "v2";

/// 过滤条件在分页 token 里的标签：规范化字符串的 FNV-1a 64 位摘要。
///
/// 用 [`ObjectFilter::fingerprint_input`] 的规范化字符串而不是结构体哈希：
/// 那个字符串是契约的一部分（`kinds` 顺序无关），标签必须与它同源。
#[must_use]
pub fn filter_tag(filter: &ObjectFilter) -> String {
    format!(
        "{:016x}",
        fingerprint(std::iter::once(filter.fingerprint_input().as_str()))
    )
}

/// 对内存中的有序集合做**带快照校验**的分页。
///
/// 与 [`paginate`] 的区别是 token 里多带一个对象名集合的指纹：
/// `v1:<offset>:<16 位十六进制指纹>`。指纹由调用方提供的 `key` 按列表顺序计算，
/// 因此翻页期间只要对象名集合发生变化（新增、删除、改名），后续页就会显式报
/// `metadata.snapshot_changed`，而不是用旧偏移量切一份已经错位的数据。
///
/// 调用方必须保证 `items` 已按 `key` 稳定排序；同一 `key` 不得重复。
///
/// 错误语义（与 [`paginate`] 一致，非法 token 绝不回退到第一页）：
///
/// - token 结构非法或偏移量越界：`config.invalid`；
/// - 对象名集合已变化：`metadata.snapshot_changed`（`failed_precondition`）。
pub fn paginate_by_key<T, F>(items: Vec<T>, request: &PageRequest, key: F) -> Result<Page<T>>
where
    F: Fn(&T) -> &str,
{
    paginate_with_tag(items, request, key, None)
}

/// 与 [`paginate_by_key`] 相同，但 token 里钉住过滤条件（v0.2.2，#84）。
///
/// 错误语义与 [`paginate_by_key`] 一致，只多一条：
///
/// - token 携带的过滤标签与本次 `filter_tag` 不同 → `config.invalid`
///   （`INVALID_ARGUMENT`）。调用方改了过滤条件却继续用旧 token，属于请求自相矛盾，
///   不是「数据变了」，所以与 `metadata.snapshot_changed` 区分开（ADR 0019）。
pub fn paginate_by_key_filtered<T, F>(
    items: Vec<T>,
    request: &PageRequest,
    key: F,
    filter_tag: &str,
) -> Result<Page<T>>
where
    F: Fn(&T) -> &str,
{
    paginate_with_tag(items, request, key, Some(filter_tag))
}

/// 分页实现：`tag == None` 走 v1 token，否则走带过滤标签的 v2 token。
fn paginate_with_tag<T, F>(
    items: Vec<T>,
    request: &PageRequest,
    key: F,
    tag: Option<&str>,
) -> Result<Page<T>>
where
    F: Fn(&T) -> &str,
{
    let page_size = usize::try_from(request.effective_page_size()).map_err(|_| {
        Error::invalid_argument(ErrorCode::CONFIG_INVALID, "分页大小超出可表示范围")
    })?;
    let fingerprint = fingerprint(items.iter().map(key));

    let offset = match request.page_token.as_deref() {
        None | Some("") => 0,
        Some(token) => parse_snapshot_token(token, fingerprint, items.len(), tag)?,
    };

    let mut items = items;
    let remaining = items.split_off(offset);
    let mut page_items = remaining;
    let next_page_token = if page_items.len() > page_size {
        page_items.truncate(page_size);
        let next = offset + page_size;
        Some(match tag {
            Some(tag) => format!("{FILTERED_TOKEN_PREFIX}:{next}:{fingerprint:016x}:{tag}"),
            None => format!("{SNAPSHOT_TOKEN_PREFIX}:{next}:{fingerprint:016x}"),
        })
    } else {
        None
    };

    let truncated = next_page_token.is_some();
    Ok(Page {
        items: page_items,
        next_page_token,
        truncated,
    })
}

/// 解析快照 token，失败时给出可区分的错误。
///
/// `tag == Some` 时要求 token 是 v2 且标签一致：标签不一致说明「换了过滤条件」，
/// 是请求非法（`config.invalid`），不是对象集合变化。
fn parse_snapshot_token(
    token: &str,
    fingerprint: u64,
    len: usize,
    tag: Option<&str>,
) -> Result<usize> {
    let parts: Vec<&str> = token.split(':').collect();
    let (expected_prefix, expected_parts) = match tag {
        Some(_) => (FILTERED_TOKEN_PREFIX, 4),
        None => (SNAPSHOT_TOKEN_PREFIX, 3),
    };
    if parts.len() != expected_parts {
        return Err(invalid_token("分页 token 格式非法"));
    }
    let (prefix, offset, snapshot) = (parts[0], parts[1], parts[2]);
    if prefix != expected_prefix {
        return Err(invalid_token("分页 token 版本不受支持"));
    }
    if let Some(expected_tag) = tag {
        let token_tag = u64::from_str_radix(parts[3], 16)
            .map_err(|_| invalid_token("分页 token 的过滤条件标签非法"))?;
        let expected_tag = u64::from_str_radix(expected_tag, 16)
            .map_err(|_| invalid_token("分页 token 的过滤条件标签非法"))?;
        if token_tag != expected_tag {
            return Err(invalid_token(
                "分页 token 与本次过滤条件不一致，请从第一页重新搜索",
            ));
        }
    }
    let offset = offset
        .parse::<usize>()
        .map_err(|_| invalid_token("分页 token 偏移量非法"))?;
    let snapshot =
        u64::from_str_radix(snapshot, 16).map_err(|_| invalid_token("分页 token 快照指纹非法"))?;
    if offset > len {
        return Err(invalid_token("分页 token 超出数据范围"));
    }
    if snapshot != fingerprint {
        return Err(Error::new(
            ErrorCode::from_static("metadata.snapshot_changed"),
            sqarvo_common::ErrorCategory::FailedPrecondition,
            "对象列表在翻页期间发生变化，请从第一页重新读取",
        ));
    }
    Ok(offset)
}

fn invalid_token(message: &'static str) -> Error {
    Error::invalid_argument(ErrorCode::CONFIG_INVALID, message)
}

/// FNV-1a 64 位指纹，输入按 `\u{1f}` 分隔，避免 `ab`+`c` 与 `a`+`bc` 撞在一起。
fn fingerprint<'a>(keys: impl Iterator<Item = &'a str>) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    for key in keys {
        for byte in key.as_bytes().iter().copied().chain([0x1f]) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(PRIME);
        }
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_page_and_token_roundtrip() {
        let items: Vec<u32> = (0..5).collect();
        let request = PageRequest {
            page_size: 2,
            page_token: None,
        };
        let first = paginate(items.clone(), &request).unwrap();
        assert_eq!(first.items, [0, 1]);
        assert_eq!(first.next_page_token.as_deref(), Some("2"));
        assert!(first.truncated);

        let second = paginate(
            items,
            &PageRequest {
                page_size: 2,
                page_token: first.next_page_token,
            },
        )
        .unwrap();
        assert_eq!(second.items, [2, 3]);
        assert_eq!(second.next_page_token.as_deref(), Some("4"));
    }

    #[test]
    fn last_page_has_no_token() {
        let items: Vec<u32> = (0..4).collect();
        let page = paginate(
            items,
            &PageRequest {
                page_size: 2,
                page_token: Some("2".to_owned()),
            },
        )
        .unwrap();
        assert_eq!(page.items, [2, 3]);
        assert!(page.next_page_token.is_none());
        assert!(!page.truncated);
    }

    #[test]
    fn invalid_tokens_are_rejected() {
        let items: Vec<u32> = (0..4).collect();
        let request = PageRequest {
            page_size: 2,
            page_token: Some("abc".to_owned()),
        };
        let error = paginate(items.clone(), &request).unwrap_err();
        assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);

        let request = PageRequest {
            page_size: 2,
            page_token: Some("99".to_owned()),
        };
        assert!(paginate(items, &request).is_err());
    }

    #[test]
    fn page_size_is_bounded() {
        assert_eq!(
            PageRequest::default().effective_page_size(),
            DEFAULT_PAGE_SIZE
        );
        assert_eq!(
            PageRequest {
                page_size: u32::MAX,
                page_token: None
            }
            .effective_page_size(),
            MAX_PAGE_SIZE
        );
    }

    // ---- 带快照校验的分页（#45）----

    fn names(count: usize) -> Vec<String> {
        (0..count).map(|index| format!("t{index:02}")).collect()
    }

    fn request(page_size: u32, token: Option<String>) -> PageRequest {
        PageRequest {
            page_size,
            page_token: token,
        }
    }

    #[test]
    fn snapshot_pagination_walks_every_item_once() {
        let items = names(7);
        let mut collected = Vec::new();
        let mut token = None;
        let mut pages = 0;
        loop {
            let page = paginate_by_key(items.clone(), &request(2, token), String::as_str).unwrap();
            assert!(page.items.len() <= 2);
            collected.extend(page.items);
            pages += 1;
            if let Some(next) = page.next_page_token {
                token = Some(next);
                assert!(page.truncated);
            } else {
                assert!(!page.truncated);
                break;
            }
            assert!(pages <= 10, "分页未收敛");
        }
        assert_eq!(pages, 4, "7 项按每页 2 项应分 4 页");
        assert_eq!(collected, items, "翻页必须无重复、无遗漏且保持顺序");
    }

    #[test]
    fn snapshot_token_rejects_tampering_and_overflow() {
        let items = names(5);
        for token in [
            "abc",
            "v1:2",
            "v2:2:0000000000000000",
            "v1:x:0000000000000000",
            "v1:2:zz",
        ] {
            let error = paginate_by_key(
                items.clone(),
                &request(2, Some(token.to_owned())),
                String::as_str,
            )
            .unwrap_err();
            assert_eq!(error.code(), ErrorCode::CONFIG_INVALID, "token={token}");
            assert_eq!(
                error.category(),
                sqarvo_common::ErrorCategory::InvalidArgument
            );
        }

        // 偏移量超过数据量：同样显式报错，而不是返回空页。
        let first = paginate_by_key(items.clone(), &request(2, None), String::as_str).unwrap();
        let token = first.next_page_token.expect("应返回 token");
        let overflow = token.replace(":2:", ":99:");
        assert!(paginate_by_key(items, &request(2, Some(overflow)), String::as_str).is_err());
    }

    #[test]
    fn snapshot_change_is_reported_instead_of_slicing_stale_offsets() {
        let items = names(5);
        let first = paginate_by_key(items.clone(), &request(2, None), String::as_str).unwrap();
        let token = first.next_page_token.expect("应返回 token");

        // 翻页期间删掉一个对象：旧 token 必须失效，不能继续按偏移量切。
        let error = paginate_by_key(
            items[1..].to_vec(),
            &request(2, Some(token.clone())),
            String::as_str,
        )
        .unwrap_err();
        assert_eq!(
            error.code(),
            ErrorCode::from_static("metadata.snapshot_changed")
        );
        assert_eq!(
            error.category(),
            sqarvo_common::ErrorCategory::FailedPrecondition
        );

        // 同名集合（数量不变）仍然可用。
        let second = paginate_by_key(items, &request(2, Some(token)), String::as_str).unwrap();
        assert_eq!(second.items, ["t02", "t03"]);
    }

    // ---- 过滤条件进 token 指纹（#84）----

    fn filter(name: &str, kinds: Vec<sqarvo_driver_api::ObjectKind>) -> ObjectFilter {
        ObjectFilter {
            name: Some(sqarvo_driver_api::NameMatch::Contains(name.to_owned())),
            kinds,
        }
    }

    #[test]
    fn filtered_tokens_walk_every_item_once() {
        let items = names(5);
        let tag = filter_tag(&filter("t", Vec::new()));
        let mut collected = Vec::new();
        let mut token = None;
        let mut pages = 0;
        loop {
            let page =
                paginate_by_key_filtered(items.clone(), &request(2, token), String::as_str, &tag)
                    .unwrap();
            collected.extend(page.items);
            pages += 1;
            match page.next_page_token {
                Some(next) => {
                    assert!(next.starts_with("v2:"), "过滤分页必须用 v2 token：{next}");
                    token = Some(next);
                }
                None => break,
            }
            assert!(pages <= 10, "分页未收敛");
        }
        assert_eq!(collected, items);
    }

    #[test]
    fn changing_the_filter_rejects_the_old_token() {
        let items = names(5);
        let first_tag = filter_tag(&filter("t", Vec::new()));
        let first =
            paginate_by_key_filtered(items.clone(), &request(2, None), String::as_str, &first_tag)
                .unwrap();
        let token = first.next_page_token.expect("应返回 token");

        // 同一份结果集、同一偏移量：只有过滤条件变了。旧 token 必须 config.invalid，
        // 而不是「刚好也切在 2」继续翻页。
        let second_tag = filter_tag(&filter("t0", Vec::new()));
        let error = paginate_by_key_filtered(
            items.clone(),
            &request(2, Some(token.clone())),
            String::as_str,
            &second_tag,
        )
        .unwrap_err();
        assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);
        assert_eq!(
            error.category(),
            sqarvo_common::ErrorCategory::InvalidArgument
        );

        // 种类集合也算条件：`kinds` 顺序无关（同集合 → 同一个标签），加一个种类就失效。
        let with_kinds = filter_tag(&filter("t", vec![sqarvo_driver_api::ObjectKind::Table]));
        let order_flipped = filter_tag(&filter(
            "t",
            vec![
                sqarvo_driver_api::ObjectKind::View,
                sqarvo_driver_api::ObjectKind::Table,
            ],
        ));
        let table_only = filter_tag(&filter("t", vec![sqarvo_driver_api::ObjectKind::Table]));
        assert_eq!(with_kinds, table_only);
        assert_ne!(with_kinds, order_flipped);
        assert_ne!(second_tag, with_kinds);

        let error = paginate_by_key_filtered(
            items.clone(),
            &request(2, Some(token.clone())),
            String::as_str,
            &with_kinds,
        )
        .unwrap_err();
        assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);

        // 不过滤的分页 token（v1）也不能拿来给过滤结果翻页：版本不同 → config.invalid。
        let unfiltered = paginate_by_key(items.clone(), &request(2, None), String::as_str).unwrap();
        let v1 = unfiltered.next_page_token.expect("应返回 token");
        let error =
            paginate_by_key_filtered(items, &request(2, Some(v1)), String::as_str, &first_tag)
                .unwrap_err();
        assert_eq!(error.code(), ErrorCode::CONFIG_INVALID);
    }

    #[test]
    fn filtered_token_still_reports_snapshot_changes() {
        let items = names(4);
        let tag = filter_tag(&filter("t", Vec::new()));
        let first =
            paginate_by_key_filtered(items.clone(), &request(2, None), String::as_str, &tag)
                .unwrap();
        let token = first.next_page_token.expect("应返回 token");

        // 条件没变，但结果集变了：仍然是 snapshot_changed（可从第一页重读），
        // 与「换了条件」的 config.invalid 分开。
        let error = paginate_by_key_filtered(
            items[1..].to_vec(),
            &request(2, Some(token)),
            String::as_str,
            &tag,
        )
        .unwrap_err();
        assert_eq!(
            error.code(),
            ErrorCode::from_static("metadata.snapshot_changed")
        );
    }

    #[test]
    fn malformed_filtered_tokens_are_rejected() {
        let items = names(3);
        let tag = filter_tag(&filter("t", Vec::new()));
        for token in [
            "v2:1",
            "v2:1:0000000000000000",
            "v1:1:0000000000000000",
            "v2:x:0000000000000000:0000000000000000",
            "v2:1:zz:0000000000000000",
            "v2:1:0000000000000000:zz",
        ] {
            let error = paginate_by_key_filtered(
                items.clone(),
                &request(1, Some(token.to_owned())),
                String::as_str,
                &tag,
            )
            .unwrap_err();
            assert_eq!(error.code(), ErrorCode::CONFIG_INVALID, "token={token}");
        }
    }
}
