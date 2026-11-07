//! 这里只保存可重建元数据，原始命令和包体保存在 .qscap。
use crate::{capture::Entry, contract::Session};
use qscope_core::native;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

type Error = Box<dyn std::error::Error + Send + Sync>;

#[derive(Clone)]
pub struct Store(Arc<Mutex<Connection>>);

#[derive(Serialize, Deserialize)]
pub struct Snapshot {
    pub checkpoint: native::Checkpoint,
    pub summary: Session,
    pub prefix_sha256: String,
}

impl Store {
    pub fn open(directory: &Path) -> Result<Self, Error> {
        qscope_automation::runtime::regular_directory(directory)?;
        let path = directory.join("captures-v1.sqlite");
        qscope_automation::runtime::no_links(&path)?;
        let mut connection = Connection::open(&path)?;
        if !connection
            .query_row("PRAGMA schema_version", [], |row| row.get::<_, u32>(0))
            .is_ok()
        {
            drop(connection);
            let invalid = directory.join(format!(
                "captures-invalid-{}.sqlite",
                qscope_automation::runtime::instance_id()?
            ));
            std::fs::rename(&path, invalid)?;
            connection = Connection::open(&path)?;
        }
        connection.busy_timeout(std::time::Duration::from_secs(2))?;
        let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version > 1 {
            return Err("A newer QScope index schema requires a compatible product version".into());
        }
        connection.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=NORMAL; PRAGMA max_page_count=262144;
            CREATE TABLE IF NOT EXISTS snapshots (capture TEXT PRIMARY KEY, state TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS events (capture TEXT NOT NULL, position INTEGER NOT NULL, metadata TEXT NOT NULL, PRIMARY KEY(capture, position));
            PRAGMA user_version=1;")?;
        Ok(Self(Arc::new(Mutex::new(connection))))
    }

    pub fn load(&self, id: &str) -> Result<Option<(Snapshot, Vec<Entry>)>, Error> {
        let connection = self.0.lock().map_err(|_| "Index lock unavailable")?;
        let snapshot = connection
            .query_row(
                "SELECT state FROM snapshots WHERE capture=?1 AND length(state)<=16384",
                [id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(snapshot) = snapshot else {
            return Ok(None);
        };
        let snapshot: Snapshot = serde_json::from_str(&snapshot)?;
        let mut statement = connection.prepare(
            "SELECT metadata FROM events WHERE capture=?1 AND length(metadata)<=8192 ORDER BY position LIMIT 250001",
        )?;
        let rows = statement.query_map([id], |row| row.get::<_, String>(0))?;
        let mut entries: Vec<Entry> = Vec::new();
        for row in rows {
            let row = row?;
            if row.len() > 8192 || entries.len() >= 250000 {
                return Err("Index metadata limit exceeded".into());
            }
            let entry: Entry = serde_json::from_str(&row)?;
            if entry.meta.event_id != entry.id.to_string()
                || entries
                    .last()
                    .is_some_and(|previous| previous.id >= entry.id)
            {
                return Err("Invalid index ordering".into());
            }
            entries.push(entry);
        }
        if snapshot.summary.records != entries.len().to_string() || snapshot.summary.id != id {
            return Err("Index snapshot is incomplete".into());
        }
        Ok(Some((snapshot, entries)))
    }

    pub fn save(
        &self,
        id: &str,
        first: usize,
        entries: &[Entry],
        snapshot: Snapshot,
    ) -> Result<(), Error> {
        let mut connection = self.0.lock().map_err(|_| "Index lock unavailable")?;
        let transaction = connection.transaction()?;
        if first == 0 {
            transaction.execute("DELETE FROM events WHERE capture=?1", [id])?;
        }
        {
            let mut insert = transaction.prepare_cached(
                "INSERT OR REPLACE INTO events(capture, position, metadata) VALUES (?1, ?2, ?3)",
            )?;
            for (position, entry) in entries.iter().enumerate().skip(first) {
                insert.execute(params![id, position as u32, serde_json::to_string(entry)?])?;
            }
        }
        transaction.execute(
            "INSERT OR REPLACE INTO snapshots(capture, state) VALUES (?1, ?2)",
            params![id, serde_json::to_string(&snapshot)?],
        )?;
        transaction.commit()?;
        Ok(())
    }
}
