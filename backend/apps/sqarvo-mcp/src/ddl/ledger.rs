//! Cross-process execution slot and durable replay protection.

use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use ring::hmac;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use sqarvo_common::{Error, ErrorCode, Result};
use sqarvo_driver_api::Secret;
use sqarvo_local_store::credentials::CredentialStore;
use sqarvo_local_store::file_lock::{LockMode, try_lock};
use uuid::Uuid;

const TTL: i64 = 86_400;
const FUTURE_SKEW: i64 = 60;
const MAX_RECORDS: u32 = 4096;
const KEY_REFERENCE: &str = "sqarvo:conn/1734d779-18d0-4ad7-94e7-b7d00b591f2f";

pub(super) fn has_unresolved(directory: &Path) -> Result<bool> {
    let path = directory.join("ddl-state.sqlite3");
    if !path.try_exists().map_err(|_| storage())? {
        return Ok(false);
    }
    let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| storage())?;
    db.busy_timeout(std::time::Duration::from_millis(250))
        .map_err(|_| storage())?;
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM operations WHERE unresolved = 1)",
        [],
        |row| row.get(0),
    )
    .map_err(|_| storage())
}

pub(super) struct Slot {
    _lock: File,
    path: PathBuf,
}

pub(super) enum Reservation {
    New,
    Existing(Value),
}

impl Slot {
    pub(super) fn acquire(directory: &Path) -> Result<Self> {
        let lock = private_file(&directory.join("ddl.lock"))?;
        if !try_lock(&lock, LockMode::Exclusive).map_err(|_| storage())? {
            return Err(Error::failed_precondition(
                ErrorCode::MCP_DDL_BUSY,
                "A schema change is already running",
            ));
        }
        Ok(Self {
            _lock: lock,
            path: directory.join("ddl-state.sqlite3"),
        })
    }

    fn open(&self) -> Result<Connection> {
        private_file(&self.path)?;
        let db = Connection::open(&self.path).map_err(|_| storage())?;
        db.execute_batch(
            "PRAGMA journal_mode=DELETE;
             PRAGMA synchronous=EXTRA;
             CREATE TABLE IF NOT EXISTS operations (
                id TEXT PRIMARY KEY, stamp INTEGER NOT NULL,
                digest BLOB NOT NULL, target TEXT NOT NULL,
                response TEXT, unresolved INTEGER NOT NULL CHECK (unresolved IN (0,1))
             );
             CREATE TABLE IF NOT EXISTS clock (last_seen INTEGER NOT NULL);
             INSERT INTO clock SELECT 0 WHERE NOT EXISTS (SELECT 1 FROM clock);",
        )
        .map_err(|_| storage())?;
        Ok(db)
    }

    pub(super) fn reserve(
        &self,
        id: Uuid,
        canonical: &[u8],
        target: &Value,
        store: &dyn CredentialStore,
    ) -> Result<Reservation> {
        self.reserve_with(
            id,
            canonical,
            target,
            store,
            time::OffsetDateTime::now_utc().unix_timestamp(),
        )
    }

    fn reserve_with(
        &self,
        id: Uuid,
        canonical: &[u8],
        target: &Value,
        store: &dyn CredentialStore,
        now: i64,
    ) -> Result<Reservation> {
        let stamp = id
            .get_timestamp()
            .and_then(|timestamp| i64::try_from(timestamp.to_unix().0).ok())
            .ok_or_else(storage)?;
        let mut db = self.open()?;
        let tx = db.transaction().map_err(|_| storage())?;
        let last: i64 = tx
            .query_row("SELECT last_seen FROM clock", [], |row| row.get(0))
            .map_err(|_| storage())?;
        if now < last || stamp < now - TTL || stamp > now + FUTURE_SKEW {
            return Err(Error::invalid_argument(
                ErrorCode::MCP_INVALID_ARGUMENT,
                "operation_id expired, is dated in the future, or the system clock moved backward",
            ));
        }
        let count: u32 = tx
            .query_row("SELECT count(*) FROM operations", [], |row| row.get(0))
            .map_err(|_| storage())?;
        let secret = match store.get(KEY_REFERENCE) {
            Ok(secret) => secret,
            Err(error) if error.code() == ErrorCode::CREDENTIAL_NOT_FOUND && count == 0 => {
                let mut random = [0_u8; 32];
                getrandom::fill(&mut random).map_err(|_| storage())?;
                let mut encoded = String::with_capacity(64);
                for byte in random {
                    write!(&mut encoded, "{byte:02x}").map_err(|_| storage())?;
                }
                let secret = Secret::new(encoded);
                store.set(KEY_REFERENCE, &secret)?;
                secret
            }
            Err(error) => return Err(error),
        };
        let key = hmac::Key::new(hmac::HMAC_SHA256, secret.expose().as_bytes());
        let digest = hmac::sign(&key, canonical);
        let id = id.to_string();
        let existing = tx
            .query_row(
                "SELECT digest, response FROM operations WHERE id = ?",
                [&id],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .optional()
            .map_err(|_| storage())?;
        if let Some((old_digest, response)) = existing {
            if hmac::verify(&key, canonical, &old_digest).is_err() {
                return Err(Error::failed_precondition(
                    ErrorCode::MCP_DDL_OPERATION_CONFLICT,
                    "operation_id has already been used for another schema change",
                ));
            }
            return Ok(Reservation::Existing(match response {
                Some(response) => serde_json::from_str(&response).map_err(|_| storage())?,
                None => unknown(&id, target),
            }));
        }
        let unresolved: Option<(String, String)> = tx
            .query_row(
                "SELECT id, target FROM operations WHERE unresolved = 1 LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|_| storage())?;
        if unresolved.is_some() {
            return Err(Error::failed_precondition(
                ErrorCode::MCP_DDL_UNRESOLVED,
                "Execution is paused pending verification of a schema change",
            ));
        }
        tx.execute("UPDATE clock SET last_seen = ?", [now])
            .map_err(|_| storage())?;
        tx.execute(
            "DELETE FROM operations WHERE unresolved = 0 AND stamp < ?",
            [now - TTL],
        )
        .map_err(|_| storage())?;
        let retained: u32 = tx
            .query_row("SELECT count(*) FROM operations", [], |row| row.get(0))
            .map_err(|_| storage())?;
        if retained >= MAX_RECORDS {
            return Err(Error::resource_exhausted(
                ErrorCode::MCP_DDL_LEDGER_FULL,
                "The schema change log is full",
            ));
        }
        tx.execute(
            "INSERT INTO operations VALUES (?, ?, ?, ?, NULL, 1)",
            params![id, stamp, digest.as_ref(), target.to_string()],
        )
        .map_err(|_| storage())?;
        tx.commit().map_err(|_| storage())?;
        Ok(Reservation::New)
    }

    pub(super) fn unresolved(&self, id: Uuid) -> Result<(Value, Value)> {
        let db = self.open()?;
        let key = id.to_string();
        let (target, response): (String, Option<String>) = db
            .query_row(
                "SELECT target, response FROM operations WHERE id = ? AND unresolved = 1",
                [&key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|_| storage())?
            .ok_or_else(|| {
                Error::not_found(
                    ErrorCode::MCP_DDL_UNRESOLVED,
                    "This operation has no pending barrier",
                )
            })?;
        let target = serde_json::from_str(&target).map_err(|_| storage())?;
        let response = response.map_or_else(
            || Ok(unknown(&key, &target)),
            |response| serde_json::from_str(&response).map_err(|_| storage()),
        )?;
        Ok((target, response))
    }

    pub(super) fn finish(&self, id: Uuid, response: &Value, unresolved: bool) -> Result<()> {
        let db = self.open()?;
        let changed = db.execute(
            "UPDATE operations SET response = ?, unresolved = ? WHERE id = ? AND unresolved = 1",
            params![response.to_string(), unresolved, id.to_string()],
        ).map_err(|_| storage())?;
        if changed != 1 {
            return Err(storage());
        }
        Ok(())
    }
}

fn unknown(id: &str, target: &Value) -> Value {
    serde_json::json!({
        "operation_id": id, "target": target, "state": "outcome_unknown", "effect": "unknown",
        "error": {
            "code": "mcp.ddl_outcome_unknown", "category": "failed_precondition",
            "message": "The schema change operation has no persisted final state. Check the database and audit log", "retryable": false
        }
    })
}

fn private_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path).map_err(|_| storage())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|_| storage())?;
    }
    Ok(file)
}

fn storage() -> Error {
    Error::internal("Could not verify the persisted DDL execution state")
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqarvo_local_store::MemoryCredentialStore;
    use time::OffsetDateTime;

    fn operation_id(timestamp: i64, sequence: u8) -> Uuid {
        let millis = u64::try_from(timestamp).unwrap() * 1000;
        let mut bytes = [0; 16];
        bytes[..6].copy_from_slice(&millis.to_be_bytes()[2..]);
        bytes[6] = 0x70;
        bytes[8] = 0x80;
        bytes[15] = sequence;
        Uuid::from_bytes(bytes)
    }

    #[test]
    fn crashed_reservation_blocks_different_ids_and_retains_unknown_status() {
        let base = tempfile::tempdir().unwrap();
        let keyring = MemoryCredentialStore::new();
        let target = serde_json::json!({"table":"fixture"});
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let operation = operation_id(now, 1);
        let slot = Slot::acquire(base.path()).unwrap();
        assert!(matches!(
            slot.reserve_with(operation, b"plan", &target, &keyring, now),
            Ok(Reservation::New)
        ));
        assert!(has_unresolved(base.path()).unwrap());
        assert_eq!(
            Slot::acquire(base.path()).err().unwrap().code(),
            ErrorCode::MCP_DDL_BUSY
        );
        drop(slot);
        let slot = Slot::acquire(base.path()).unwrap();
        let replay = slot
            .reserve_with(operation, b"plan", &target, &keyring, now)
            .unwrap();
        assert!(
            matches!(replay, Reservation::Existing(value) if value["state"] == "outcome_unknown")
        );
        assert_eq!(
            slot.reserve_with(operation, b"different", &target, &keyring, now)
                .err()
                .unwrap()
                .code(),
            ErrorCode::MCP_DDL_OPERATION_CONFLICT
        );
        assert_eq!(
            slot.reserve_with(operation_id(now, 2), b"plan", &target, &keyring, now)
                .err()
                .unwrap()
                .code(),
            ErrorCode::MCP_DDL_UNRESOLVED
        );
        let (_, response) = slot.unresolved(operation).unwrap();
        slot.finish(operation, &response, false).unwrap();
        assert!(!has_unresolved(base.path()).unwrap());
        assert!(
            matches!(slot.reserve_with(operation, b"plan", &target, &keyring, now),
            Ok(Reservation::Existing(value)) if value["state"] == "outcome_unknown")
        );
    }
}
