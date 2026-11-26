//! Status summary used before the first database connection.

/// 状态栏「驱动数 · 浏览会话数」文案。
///
/// 纯函数：不接触 GTK，便于单测（断言启动态确实是 0 条浏览会话）。
#[must_use]
pub fn idle_status_text(driver_count: usize, browse_sessions: usize) -> String {
    crate::l10n::tf(
        "status_bar.idle_summary",
        &[
            ("drivers", driver_count.to_string()),
            ("sessions", browse_sessions.to_string()),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_status_reports_driver_and_session_counts() {
        assert_eq!(idle_status_text(2, 0), "驱动 2 · 浏览会话 0");
        assert_eq!(idle_status_text(0, 0), "驱动 0 · 浏览会话 0");
    }
}
