//! Log capture.
//!
//! A bounded ring buffer per service, backed by a file on disk.
//!
//! The buffer answers reads; the file is what makes the answer survive a daemon
//! restart. That matters because "why did it die?" is asked *after* the thing
//! died, often after the daemon was restarted too — logs that only live in
//! memory are missing exactly when they are wanted.
//!
//! Reads are cursor-based so an agent can ask "what is new since seq N" instead
//! of re-reading the whole buffer, which is the difference between a useful
//! tool call and a context blowout.

use std::collections::{HashMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::Utc;
use runtime_types::{LogLine, LogStream, Result, RuntimeError, ServiceId};

/// Lines retained in memory per service. Roughly a few hundred KB.
pub const DEFAULT_CAPACITY: usize = 2_000;

/// Hard ceiling on a single read, so a caller asking for everything cannot
/// flood an agent's context.
pub const MAX_READ_LINES: usize = 1_000;

/// A service's log file is rotated once past this size.
///
/// One generation is kept: enough to cover a crash that happened just before a
/// noisy restart, without letting a chatty dev server fill a disk.
pub const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Default)]
struct ServiceLog {
    next_seq: u64,
    lines: VecDeque<LogLine>,
    file: Option<File>,
    bytes: u64,
}

#[derive(Debug)]
pub struct LogStore {
    services: Mutex<HashMap<ServiceId, ServiceLog>>,
    capacity: usize,
    /// Where per-service files live. `None` keeps everything in memory, which
    /// is what tests want.
    directory: Option<PathBuf>,
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
            directory: None,
        }
    }

    /// A store that also writes to `directory`.
    pub fn persistent(capacity: usize, directory: impl AsRef<Path>) -> Result<Self> {
        let directory = directory.as_ref().to_path_buf();
        std::fs::create_dir_all(&directory)?;
        Ok(Self {
            services: Mutex::new(HashMap::new()),
            capacity: capacity.max(1),
            directory: Some(directory),
        })
    }

    pub fn append(
        &self,
        service_id: &ServiceId,
        stream: LogStream,
        message: impl Into<String>,
    ) -> Result<LogLine> {
        let mut guard = self.lock()?;
        let log = match guard.get_mut(service_id) {
            Some(log) => log,
            None => {
                let restored = self.restore(service_id);
                guard.entry(service_id.clone()).or_insert(restored)
            }
        };

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

        if let Some(directory) = &self.directory {
            // A failed write must not stop the service's output being captured
            // in memory, which is what most reads use anyway.
            if let Err(err) = write_line(log, directory, service_id, &line) {
                tracing::warn!(%err, service = %service_id, "could not persist a log line");
            }
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
        let mut guard = self.lock()?;
        // A service the daemon has not touched since restarting still has a
        // file; load it rather than reporting nothing.
        if !guard.contains_key(service_id) {
            let restored = self.restore(service_id);
            if restored.lines.is_empty() && restored.next_seq == 0 {
                return Ok(Vec::new());
            }
            guard.insert(service_id.clone(), restored);
        }
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
        let guard = self.lock()?;
        Ok(guard
            .get(service_id)
            .map(|log| log.next_seq.saturating_sub(1)))
    }

    pub fn clear(&self, service_id: &ServiceId) -> Result<()> {
        let mut guard = self.lock()?;
        guard.remove(service_id);
        if let Some(directory) = &self.directory {
            let _ = std::fs::remove_file(log_path(directory, service_id));
            let _ = std::fs::remove_file(rotated_path(directory, service_id));
        }
        Ok(())
    }

    /// Delete files belonging to services that no longer exist.
    ///
    /// Without this, removing a project leaves its logs behind for good.
    pub fn prune(&self, keep: &[ServiceId]) -> Result<usize> {
        let Some(directory) = &self.directory else {
            return Ok(0);
        };
        let Ok(entries) = std::fs::read_dir(directory) else {
            return Ok(0);
        };

        let mut removed = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let id = name
                .trim_end_matches(".log")
                .trim_end_matches(".1")
                .trim_end_matches(".log");
            if keep.iter().any(|service| service.as_str() == id) {
                continue;
            }
            if std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Rebuild a service's buffer from its file.
    fn restore(&self, service_id: &ServiceId) -> ServiceLog {
        let Some(directory) = &self.directory else {
            return ServiceLog::default();
        };
        let path = log_path(directory, service_id);
        let Ok(file) = File::open(&path) else {
            return ServiceLog::default();
        };

        let mut lines: VecDeque<LogLine> = VecDeque::new();
        for raw in BufReader::new(file).lines().map_while(std::io::Result::ok) {
            if raw.trim().is_empty() {
                continue;
            }
            let Ok(line) = serde_json::from_str::<LogLine>(&raw) else {
                continue; // a torn final line from a crash; skip it
            };
            lines.push_back(line);
            while lines.len() > self.capacity {
                lines.pop_front();
            }
        }

        // Continue the sequence rather than restarting it, so a cursor held
        // across a daemon restart does not replay old lines.
        let next_seq = lines.back().map(|line| line.seq + 1).unwrap_or(0);
        ServiceLog {
            next_seq,
            lines,
            file: None,
            bytes: std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
        }
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HashMap<ServiceId, ServiceLog>>> {
        self.services
            .lock()
            .map_err(|_| RuntimeError::internal("log store lock poisoned"))
    }
}

/// Append one line, rotating first if the file has grown past the cap.
fn write_line(
    log: &mut ServiceLog,
    directory: &Path,
    service_id: &ServiceId,
    line: &LogLine,
) -> Result<()> {
    let path = log_path(directory, service_id);

    if log.bytes >= MAX_FILE_BYTES {
        log.file = None; // close before renaming
        let _ = std::fs::rename(&path, rotated_path(directory, service_id));
        log.bytes = 0;
    }

    if log.file.is_none() {
        log.file = Some(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .map_err(|err| {
                    RuntimeError::io(format!("cannot open {}: {err}", path.display()))
                })?,
        );
    }

    // JSON per line: the stream and the sequence number have to survive, and a
    // torn write during a crash costs one line rather than the whole file.
    let mut encoded = serde_json::to_vec(line)
        .map_err(|err| RuntimeError::internal(format!("cannot encode a log line: {err}")))?;
    encoded.push(b'\n');

    if let Some(file) = log.file.as_mut() {
        file.write_all(&encoded)?;
        log.bytes += encoded.len() as u64;
    }
    Ok(())
}

fn log_path(directory: &Path, service_id: &ServiceId) -> PathBuf {
    directory.join(format!("{}.log", service_id.as_str()))
}

fn rotated_path(directory: &Path, service_id: &ServiceId) -> PathBuf {
    directory.join(format!("{}.log.1", service_id.as_str()))
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

    #[test]
    fn output_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let id = ServiceId::from("svc");

        let store = LogStore::persistent(10, dir.path()).unwrap();
        store.append(&id, LogStream::Stdout, "listening on 3000").unwrap();
        store.append(&id, LogStream::Stderr, "boom").unwrap();
        drop(store);

        // A new store is what the daemon has after being restarted; the answer
        // to "why did it die" must not have gone with the old process.
        let reopened = LogStore::persistent(10, dir.path()).unwrap();
        let lines = reopened.read(&id, 10, None).unwrap();

        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].message, "boom");
        assert_eq!(lines[1].stream, LogStream::Stderr);

        // Sequence numbers continue, so a cursor held across the restart does
        // not replay what the caller already has: holding seq 1 from before the
        // restart returns the new line and nothing else.
        let next = reopened.append(&id, LogStream::Stdout, "restarted").unwrap();
        assert_eq!(next.seq, 2);
        let fresh = reopened.read(&id, 10, Some(1)).unwrap();
        assert_eq!(fresh.len(), 1);
        assert_eq!(fresh[0].message, "restarted");
    }

    #[test]
    fn a_torn_final_line_costs_one_line_not_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let id = ServiceId::from("svc");

        let store = LogStore::persistent(10, dir.path()).unwrap();
        store.append(&id, LogStream::Stdout, "good").unwrap();
        drop(store);

        // A crash mid-write leaves a partial JSON object behind.
        let path = dir.path().join("svc.log");
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"seq\":1,\"mess").unwrap();
        drop(file);

        let reopened = LogStore::persistent(10, dir.path()).unwrap();
        let lines = reopened.read(&id, 10, None).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].message, "good");
    }

    #[test]
    fn pruning_removes_files_for_services_that_are_gone() {
        let dir = tempfile::tempdir().unwrap();
        let kept = ServiceId::from("kept");
        let gone = ServiceId::from("gone");

        let store = LogStore::persistent(10, dir.path()).unwrap();
        store.append(&kept, LogStream::Stdout, "a").unwrap();
        store.append(&gone, LogStream::Stdout, "b").unwrap();

        assert_eq!(store.prune(std::slice::from_ref(&kept)).unwrap(), 1);
        assert!(dir.path().join("kept.log").exists());
        assert!(!dir.path().join("gone.log").exists());
    }
}
