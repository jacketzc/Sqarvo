//! 不透明分页 token 的生成与校验。
//!
//! 分页类型与上限定义在 Driver SPI 中，供驱动直接实现元数据接口。

use sqarvo_common::{Error, ErrorCode, Result};

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
    let page_size = usize::try_from(request.effective_page_size()).map_err(|_| {
        Error::invalid_argument(ErrorCode::CONFIG_INVALID, "分页大小超出可表示范围")
    })?;
    let fingerprint = fingerprint(items.iter().map(key));

    let offset = match request.page_token.as_deref() {
        None | Some("") => 0,
        Some(token) => parse_snapshot_token(token, fingerprint, items.len())?,
    };

    let mut items = items;
    let remaining = items.split_off(offset);
    let mut page_items = remaining;
    let next_page_token = if page_items.len() > page_size {
        page_items.truncate(page_size);
        Some(format!(
            "{SNAPSHOT_TOKEN_PREFIX}:{}:{fingerprint:016x}",
            offset + page_size
        ))
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
fn parse_snapshot_token(token: &str, fingerprint: u64, len: usize) -> Result<usize> {
    let mut parts = token.split(':');
    let (Some(prefix), Some(offset), Some(snapshot), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(invalid_token("分页 token 格式非法"));
    };
    if prefix != SNAPSHOT_TOKEN_PREFIX {
        return Err(invalid_token("分页 token 版本不受支持"));
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
}
