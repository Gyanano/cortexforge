//! Event bus I/O — NDJSON append-write and read operations for eventbus.log (§4.5).
//!
//! Orchestrator is the sole writer. Events are appended line-by-line as NDJSON.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use crate::error::ForgeResult;
use crate::event::EventEntry;

/// The project-level event bus (eventbus.log).
pub struct EventBus {
    path: std::path::PathBuf,
}

impl EventBus {
    /// Open the event bus at the given path. Creates the file if it doesn't exist.
    pub fn open(path: impl Into<std::path::PathBuf>) -> Self {
        let path = path.into();
        // Ensure parent exists
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        Self { path }
    }

    /// Append a single event as one NDJSON line.
    ///
    /// Uses O_APPEND for safe concurrent appends (within OS limits for small writes).
    pub fn append(&self, entry: &EventEntry) -> ForgeResult<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let line = serde_json::to_string(entry)
            .map_err(|e| crate::error::ForgeError::Other(format!("json serialize: {e}")))?;
        writeln!(file, "{line}")?;
        file.flush()?;
        Ok(())
    }

    /// Read all events from the log.
    pub fn read_all(&self) -> ForgeResult<Vec<EventEntry>> {
        if !self.path.exists() {
            return Ok(vec![]);
        }
        let file = File::open(&self.path)?;
        let reader = BufReader::new(file);
        let mut entries = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<EventEntry>(&line) {
                Ok(entry) => entries.push(entry),
                Err(e) => {
                    tracing::warn!(line = %line, error = %e, "failed to parse eventbus line, skipping");
                }
            }
        }
        Ok(entries)
    }

    /// Read events filtered by node name.
    pub fn read_by_node(&self, node: &str) -> ForgeResult<Vec<EventEntry>> {
        Ok(self
            .read_all()?
            .into_iter()
            .filter(|e| e.node == node)
            .collect())
    }

    /// Read events filtered by event type name.
    pub fn read_by_event(&self, event_name: &str) -> ForgeResult<Vec<EventEntry>> {
        Ok(self
            .read_all()?
            .into_iter()
            .filter(|e| e.event.name() == event_name)
            .collect())
    }

    /// Read events since a given timestamp (RFC 3339).
    pub fn read_since(&self, since: &str) -> ForgeResult<Vec<EventEntry>> {
        Ok(self
            .read_all()?
            .into_iter()
            .filter(|e| e.ts.to_rfc3339().as_str() >= since)
            .collect())
    }

    /// Replay the full lifecycle of a node from the event log.
    ///
    /// Returns events sorted chronologically (guaranteed by append-only writes).
    pub fn replay_node(&self, node: &str) -> ForgeResult<Vec<EventEntry>> {
        let mut events = self.read_by_node(node)?;
        events.sort_by_key(|e| e.ts);
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventType;

    fn test_entry(node: &str, ev: EventType) -> EventEntry {
        EventEntry {
            ts: chrono::Utc::now().into(),
            node: node.into(),
            event: ev,
        }
    }

    #[test]
    fn test_append_and_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("eventbus.log");
        let bus = EventBus::open(&path);

        bus.append(&test_entry(
            "node-a",
            EventType::State {
                from: "idle".into(),
                to: "assigned".into(),
                seq: 1,
                depth: 2,
            },
        ))
        .unwrap();

        bus.append(&test_entry(
            "node-b",
            EventType::Spawn {
                child: "mod-c".into(),
                pid: 1234,
                depth: 3,
                wake_up: false,
            },
        ))
        .unwrap();

        let all = bus.read_all().unwrap();
        assert_eq!(all.len(), 2);

        let a_events = bus.read_by_node("node-a").unwrap();
        assert_eq!(a_events.len(), 1);
        assert_eq!(a_events[0].event.name(), "state");

        let replayed = bus.replay_node("node-b").unwrap();
        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0].event.name(), "spawn");
    }

    #[test]
    fn test_read_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.log");
        let bus = EventBus::open(&path);
        let all = bus.read_all().unwrap();
        assert!(all.is_empty());
    }

    #[test]
    fn test_read_filter_by_event() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("eventbus.log");
        let bus = EventBus::open(&path);

        bus.append(&test_entry(
            "n1",
            EventType::Deadlock {
                cycle: vec!["a".into(), "b".into()],
            },
        ))
        .unwrap();

        bus.append(&test_entry(
            "n2",
            EventType::SpawnWakeFailed {
                provider: "p".into(),
                key: "k".into(),
            },
        ))
        .unwrap();

        let deadlocks = bus.read_by_event("deadlock").unwrap();
        assert_eq!(deadlocks.len(), 1);
    }
}
