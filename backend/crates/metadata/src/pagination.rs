//! 不透明分页 token 的生成与校验。

use sqarvo_common::{Error, ErrorCode, Result};

/// 默认页大小。
pub const DEFAULT_PAGE_SIZE: u32 = 100;
/// 单页上限，防止前端用超大 `page_size` 一次性拉全量。
pub const MAX_PAGE_SIZE: u32 = 500;

/// 分页请求。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageRequest {
    /// 期望条数；0 表示使用默认值。
    pub page_size: u32,
    /// 上一页返回的 token。
    pub page_token: Option<String>,
}

impl PageRequest {
    /// 归一化后的页大小。
    #[must_use]
    pub fn effective_page_size(&self) -> u32 {
        if self.page_size == 0 {
            DEFAULT_PAGE_SIZE
        } else {
            self.page_size.min(MAX_PAGE_SIZE)
        }
    }
}

/// 分页结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    /// 本页数据。
    pub items: Vec<T>,
    /// 下一页 token；`None` 表示没有更多数据。
    pub next_page_token: Option<String>,
    /// 是否因为页大小上限而截断。
    pub truncated: bool,
}

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
}
