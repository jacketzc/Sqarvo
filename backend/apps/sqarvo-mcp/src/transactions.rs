//! Bounded, process-local lookup for MCP transaction owners.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use serde_json::{Map, Value};
use sqarvo_common::{Error, ErrorCode, Result};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const MAX_TRANSACTIONS: usize = 4;
const MAX_TERMINALS: usize = 64;
const TERMINAL_RETENTION: Duration = Duration::from_secs(120);

#[derive(Clone)]
pub(crate) struct Owner {
    pub id: Uuid,
    pub lifetime: CancellationToken,
}

impl Owner {
    pub(crate) fn new(lifetime: CancellationToken) -> Self {
        Self {
            id: Uuid::new_v4(),
            lifetime,
        }
    }
}

pub(crate) type Reply = oneshot::Sender<Result<Value, Failure>>;

pub(crate) struct Failure {
    pub error: Error,
    pub state: Option<&'static str>,
    pub audit_warning: bool,
}

impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        Self {
            error,
            state: None,
            audit_warning: false,
        }
    }
}

pub(crate) struct Command {
    pub tool: &'static str,
    pub request_id: String,
    pub arguments: Map<String, Value>,
    pub cancellation: CancellationToken,
    pub reply: Reply,
}

struct Active {
    owner: Uuid,
    sender: Option<mpsc::Sender<Command>>,
}

struct Terminal {
    id: Uuid,
    owner: Uuid,
    state: &'static str,
    audit_warning: bool,
    until: Instant,
}

#[derive(Default)]
struct Entries {
    active: HashMap<Uuid, Active>,
    terminals: VecDeque<Terminal>,
}

#[derive(Default)]
pub(crate) struct TransactionRegistry {
    entries: Mutex<Entries>,
    pub shutdown: CancellationToken,
    changed: tokio::sync::Notify,
}

impl TransactionRegistry {
    fn lock(&self) -> MutexGuard<'_, Entries> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn reserve(&self, owner: &Owner) -> Result<(Uuid, mpsc::Receiver<Command>)> {
        let mut entries = self.lock();
        if self.shutdown.is_cancelled() || owner.lifetime.is_cancelled() {
            return Err(cancelled());
        }
        if entries.active.values().any(|entry| entry.owner == owner.id) {
            return Err(Error::already_exists(
                ErrorCode::TRANSACTION_ALREADY_ACTIVE,
                "该客户端已有事务或事务正在清理",
            ));
        }
        if entries.active.len() >= MAX_TRANSACTIONS {
            return Err(Error::resource_exhausted(
                ErrorCode::MCP_INVALID_ARGUMENT,
                "显式事务数量已达上限",
            ));
        }
        let id = Uuid::new_v4();
        let (sender, receiver) = mpsc::channel(1);
        entries.active.insert(
            id,
            Active {
                owner: owner.id,
                sender: Some(sender),
            },
        );
        Ok((id, receiver))
    }

    pub(crate) fn submit(&self, owner: &Owner, id: Uuid, command: Command) -> Result<(), Failure> {
        let entries = self.lock();
        if owner.lifetime.is_cancelled() || self.shutdown.is_cancelled() {
            return Err(cancelled().into());
        }
        if let Some(terminal) =
            entries.terminals.iter().rev().find(|entry| {
                entry.id == id && entry.owner == owner.id && entry.until > Instant::now()
            })
        {
            return Err(Failure {
                error: terminal_error(terminal.state),
                state: Some(terminal.state),
                audit_warning: terminal.audit_warning,
            });
        }
        let sender = entries
            .active
            .get(&id)
            .filter(|entry| entry.owner == owner.id)
            .and_then(|entry| entry.sender.as_ref())
            .ok_or_else(not_found)?;
        sender.try_send(command).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => Failure::from(Error::resource_exhausted(
                ErrorCode::from_static("mcp.transaction_busy"),
                "事务正在处理其他请求",
            )),
            mpsc::error::TrySendError::Closed(_) => not_found().into(),
        })
    }

    pub(crate) fn terminal(
        &self,
        owner: &Owner,
        id: Uuid,
        state: &'static str,
        audit_warning: bool,
    ) {
        let mut entries = self.lock();
        if let Some(active) = entries.active.get_mut(&id) {
            active.sender = None;
        }
        entries.terminals.push_back(Terminal {
            id,
            owner: owner.id,
            state,
            audit_warning,
            until: Instant::now() + TERMINAL_RETENTION,
        });
        while entries.terminals.len() > MAX_TERMINALS {
            entries.terminals.pop_front();
        }
    }

    pub(crate) fn release(&self, id: Uuid) {
        self.lock().active.remove(&id);
        self.changed.notify_one();
    }

    pub(crate) async fn shutdown(&self) {
        self.shutdown.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(12), async {
            while !self.lock().active.is_empty() {
                self.changed.notified().await;
            }
        })
        .await;
    }
}

pub(crate) fn not_found() -> Error {
    Error::not_found(ErrorCode::TRANSACTION_NOT_FOUND, "事务不存在或不可用")
}

pub(crate) fn cancelled() -> Error {
    Error::new(
        ErrorCode::QUERY_CANCELLED,
        sqarvo_common::ErrorCategory::Cancelled,
        "事务请求已取消",
    )
}

pub(crate) fn terminal_error(state: &str) -> Error {
    match state {
        "commit_unknown" => {
            Error::failed_precondition(ErrorCode::MCP_COMMIT_UNKNOWN, "数据库提交结果未知")
        }
        "rollback_unconfirmed" => Error::failed_precondition(
            ErrorCode::from_static("mcp.rollback_unconfirmed"),
            "数据库回滚未确认",
        ),
        _ => Error::failed_precondition(
            ErrorCode::from_static("mcp.transaction_ended"),
            "事务已结束",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transaction_quotas_count_closing_sessions_and_keep_owners_separate() {
        let registry = TransactionRegistry::default();
        let owners: Vec<_> = (0..=MAX_TRANSACTIONS)
            .map(|_| Owner::new(CancellationToken::new()))
            .collect();
        let mut ids = Vec::new();
        for owner in &owners[..MAX_TRANSACTIONS] {
            ids.push(registry.reserve(owner).unwrap().0);
            assert_eq!(
                registry.reserve(owner).unwrap_err().code(),
                ErrorCode::TRANSACTION_ALREADY_ACTIVE
            );
        }
        let new_owner = &owners[MAX_TRANSACTIONS];
        registry.terminal(&owners[0], ids[0], "commit_unknown", true);
        assert!(registry.reserve(new_owner).is_err());
        let command = || Command {
            tool: "commit_transaction",
            request_id: "test".into(),
            arguments: Map::new(),
            cancellation: CancellationToken::new(),
            reply: oneshot::channel().0,
        };
        let terminal = registry
            .submit(&owners[0], ids[0], command())
            .err()
            .unwrap();
        assert_eq!(terminal.error.code(), ErrorCode::MCP_COMMIT_UNKNOWN);
        assert!(terminal.audit_warning);
        let foreign = registry.submit(new_owner, ids[0], command()).err().unwrap();
        assert_eq!(foreign.error.code(), ErrorCode::TRANSACTION_NOT_FOUND);
        registry.release(ids[0]);
        assert!(registry.reserve(new_owner).is_ok());
    }
}
