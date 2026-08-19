//! In-memory log capture.
//!
//! A bounded ring buffer per service. Reads are cursor-based so an agent can
//! ask for "what is new since seq N" instead of re-reading the whole buffer,
//! which is the difference between a useful tool call and a context blowout.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use chrono::Utc;
use runtime_types::{LogLine, LogStream, Result, RuntimeError, ServiceId};

/// Lines retained per service. Roughly a few hundred KB at typical line lengths.
pub const DEFAULT_CAPACITY: usize = 2_000;

/// Hard ceiling on a single read, so a caller asking for everything cannot
/// flood an agent's context.
pub const MAX_READ_LINES: usize = 1_000;

#[derive(Debug, Default)]
struct ServiceLog {
    next_seq: u64,
    lines: VecDeque<LogLine>,
}

#[derive(Debug)]
pub struct LogStore {
    services: Mutex<HashMap<ServiceId, ServiceLog>>,
    capacity: usize,
}

impl Default for LogStore {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

impl LogStore {
    pub fn new(capacity: usize) -> Self {
        Self {
            services: Mutex::new(HashMap::new()),
            capacity: capacity.max(1),
        }
    }

    pub fn append(
        &self,
        service_id: &ServiceId,
        stream: LogStream,
        message: impl Into<String>,
    ) -> Result<LogLine> {
        let mut guard = self
            .services
            .lock()
            .map_err(|_| RuntimeError::internal("log store lock poisoned"))?;
        let log = guard.entry(service_id.clone()).or_default();

        let line = LogLine {
            seq: log.next_seq,
            service_id: service_id.clone(),
            stream,
            timestamp: Utc::now(),
            message: message.into(),
        };
        log.next_seq += 1;
        log.lines.push_back(line.clone());
        while log.lines.len() > self.capacity {
            log.lines.pop_front();
        }
        Ok(line)
    }

    /// The most recent lines, oldest first.
    ///
    /// `since_seq` returns only lines strictly newer than that cursor;
    /// `max_lines` is clamped to [`MAX_READ_LINES`].
    pub fn read(
        &self,
        service_id: &ServiceId,
        max_lines: usize,
        since_seq: Option<u64>,
    ) -> Result<Vec<LogLine>> {
        let guard = self
            .services
            .lock()
            .map_err(|_| RuntimeError::internal("log store lock poisoned"))?;
        let Some(log) = guard.get(service_id) else {
            return Ok(Vec::new());
        };

        let limit = max_lines.clamp(1, MAX_READ_LINES);
        let matching = log
            .lines
            .iter()
            .filter(|line| since_seq.is_none_or(|cursor| line.seq > cursor));

        let total = matching.clone().count();
        Ok(matching.skip(total.saturating_sub(limit)).cloned().collect())
    }

    /// Cursor to pass as `since_seq` to receive only future lines.
    pub fn cursor(&self, service_id: &ServiceId) -> Result<Option<u64>> {
        let guard = self
            .services
            .lock()
            .map_err(|_| RuntimeError::internal("log store lock poisoned"))?;
        Ok(guard
            .get(service_id)
            .map(|log| log.next_seq.saturating_sub(1)))
    }

    pub fn clear(&self, service_id: &ServiceId) -> Result<()> {
        let mut guard = self
            .services
            .lock()
            .map_err(|_| RuntimeError::internal("log store lock poisoned"))?;
        guard.remove(service_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_oldest_lines_past_capacity() {
        let store = LogStore::new(3);
        let id = ServiceId::from("svc");
        for i in 0..5 {
            store.append(&id, LogStream::Stdout, format!("line {i}")).unwrap();
        }

        let lines = store.read(&id, 10, None).unwrap();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].message, "line 2");
        // Sequence numbers survive eviction, so cursors stay valid.
        assert_eq!(lines[0].seq, 2);
    }

    #[test]
    fn since_seq_returns_only_newer_lines() {
        let store = LogStore::new(10);
        let id = ServiceId::from("svc");
        for i in 0..4 {
            store.append(&id, LogStream::Stdout, format!("line {i}")).unwrap();
        }

        let lines = store.read(&id, 10, Some(1)).unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].message, "line 2");
    }

    #[test]
    fn read_returns_the_tail_when_limited() {
        let store = LogStore::new(10);
        let id = ServiceId::from("svc");
        for i in 0..6 {
            store.append(&id, LogStream::Stdout, format!("line {i}")).unwrap();
        }

        let lines = store.read(&id, 2, None).unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].message, "line 4");
        assert_eq!(lines[1].message, "line 5");
    }
}
