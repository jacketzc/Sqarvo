//! 连接授权与 lazy 会话生命周期（#207 实现；V3.1 §3.3/§9）。
//!
//! 契约要点：
//! - 每个 alias 至多一个 `SessionHandle`，首次工具调用才 `SessionManager::connect`；
//!   `list_connections` 只读状态、不触发网络；
//! - 状态机 `not_connected → connected / failed`；失败记录错误分类（不含凭据），
//!   下次调用可重试；断连允许一次「重连 + 重试」；
//! - 进程级 `tokio::sync::Semaphore(≤4, 默认 2)` + `QueryRegistry`
//!   （`QueryLimits.max_concurrent` 同步设置）；同 alias 由会话执行锁天然串行
//!   （ADR 0008），这一点对工具描述如实声明；
//! - 退出清理：`sessions.close_all()` + `queries.remove_session`，仿
//!   backend-service main.rs:130-171/:197 的装配顺序。
