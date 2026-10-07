//! The append-only operation journal.
//!
//! A journal exists because a filesystem change and a database change cannot be
//! committed together. Multiple destinations may also sit on multiple
//! filesystems, so there is no global atomic apply available at any price. What
//! *is* achievable is a durable record of intent written before each mutation and
//! a durable record of outcome written after, so a crash leaves enough
//! information to finish or undo the work.
//!
//! The format is one JSON object per line, flushed and synced after every entry.
//! Append-only and line-oriented means a truncated final line is recoverable: the
//! reader stops at the last complete entry rather than failing the whole file.

use std::io::{BufRead, BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::error::{Error, IoContext, Result};

/// Journal schema version, so a future format change is detectable.
pub const JOURNAL_SCHEMA: u32 = 1;

/// One journal entry.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Entry {
    /// Opens the journal and names the operation.
    Begin {
        schema: u32,
        txn: String,
        command: String,
        at: String,
    },
    /// The complete plan, recorded before anything is touched.
    Plan { targets: Vec<String> },
    /// Written immediately before a target is mutated.
    ///
    /// `fingerprint` is the digest of the target as it was *just* re-verified, so
    /// recovery can tell whether the mutation had started.
    BeforeMutate {
        target: String,
        fingerprint: Option<String>,
        backup: Option<String>,
        at: String,
    },
    /// Written immediately after a target is mutated successfully.
    AfterMutate {
        target: String,
        fingerprint: Option<String>,
        at: String,
    },
    /// A target was deliberately skipped, with the reason.
    Skipped { target: String, reason: String },
    /// A target failed.
    Failed { target: String, reason: String },
    /// The operation finished.
    End { status: String, at: String },
}

/// A journal open for appending.
#[derive(Debug)]
pub struct Journal {
    path: PathBuf,
    writer: Option<BufWriter<std::fs::File>>,
}

impl Journal {
    /// Create a new journal for a transaction.
    pub fn create(dir: &Path, txn_id: &str, command: &str) -> Result<Self> {
        std::fs::create_dir_all(dir).ctx("creating the journal directory", dir)?;
        let path = dir.join(format!("{txn_id}.jsonl"));
        let file = std::fs::OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&path)
            .ctx("creating the journal", &path)?;
        // A journal records source locators and package paths, so it is private.
        crate::safepath::restrict_file(&path)?;

        let mut journal = Self {
            path,
            writer: Some(BufWriter::new(file)),
        };
        journal.append(&Entry::Begin {
            schema: JOURNAL_SCHEMA,
            txn: txn_id.to_string(),
            command: command.to_string(),
            at: crate::state::now(),
        })?;
        Ok(journal)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append an entry and make it durable before returning.
    ///
    /// The `sync_all` is the point of the exercise: an entry still sitting in the
    /// page cache when the machine loses power would leave no record of a
    /// mutation that did reach the disk.
    pub fn append(&mut self, entry: &Entry) -> Result<()> {
        let Some(writer) = self.writer.as_mut() else {
            return Err(Error::Internal("journal is already closed".into()));
        };
        let line = serde_json::to_string(entry).map_err(|err| {
            Error::Internal(format!("could not serialise a journal entry: {err}"))
        })?;
        writer
            .write_all(line.as_bytes())
            .ctx("writing to the journal", &self.path)?;
        writer
            .write_all(b"\n")
            .ctx("writing to the journal", &self.path)?;
        writer.flush().ctx("flushing the journal", &self.path)?;
        writer
            .get_ref()
            .sync_all()
            .ctx("syncing the journal", &self.path)?;
        Ok(())
    }

    /// Write the closing entry and close the file.
    pub fn finish(mut self, status: &str) -> Result<PathBuf> {
        self.append(&Entry::End {
            status: status.to_string(),
            at: crate::state::now(),
        })?;
        self.writer = None;
        Ok(self.path)
    }
}

/// Read a journal back, stopping at the last complete line.
///
/// A partial trailing line means the process died mid-write, which is expected
/// rather than exceptional, so it is dropped instead of failing the read.
pub fn read(path: &Path) -> Result<Vec<Entry>> {
    let file = std::fs::File::open(path).ctx("opening the journal", path)?;
    let reader = std::io::BufReader::new(file);
    let mut entries = Vec::new();

    for line in reader.lines() {
        let line = line.ctx("reading the journal", path)?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Entry>(&line) {
            Ok(entry) => entries.push(entry),
            // A trailing partial line is the crash signature. Stop cleanly.
            Err(_) => break,
        }
    }

    Ok(entries)
}

/// What a journal says about an interrupted run.
#[derive(Debug, Clone, PartialEq)]
pub struct Recovery {
    pub txn_id: String,
    pub command: String,
    /// Targets that were planned.
    pub planned: Vec<String>,
    /// Targets whose mutation both started and completed.
    pub completed: Vec<String>,
    /// Targets whose mutation started but never completed.
    ///
    /// These are the dangerous ones: each may be half-written, so each needs its
    /// backup restored.
    pub in_flight: Vec<(String, Option<String>)>,
    /// True when an `End` entry is present.
    pub finished: bool,
}

impl Recovery {
    /// True when the run needs attention.
    pub fn needs_recovery(&self) -> bool {
        !self.finished || !self.in_flight.is_empty()
    }
}

/// Analyse a journal to work out what an interrupted run left behind.
pub fn analyse(entries: &[Entry]) -> Option<Recovery> {
    let mut txn_id = String::new();
    let mut command = String::new();
    let mut planned = Vec::new();
    let mut started: Vec<(String, Option<String>)> = Vec::new();
    let mut completed = Vec::new();
    let mut finished = false;

    for entry in entries {
        match entry {
            Entry::Begin {
                txn, command: c, ..
            } => {
                txn_id = txn.clone();
                command = c.clone();
            }
            Entry::Plan { targets } => planned = targets.clone(),
            Entry::BeforeMutate { target, backup, .. } => {
                started.push((target.clone(), backup.clone()));
            }
            Entry::AfterMutate { target, .. } => completed.push(target.clone()),
            Entry::End { .. } => finished = true,
            Entry::Skipped { .. } | Entry::Failed { .. } => {}
        }
    }

    if txn_id.is_empty() {
        return None;
    }

    let in_flight: Vec<(String, Option<String>)> = started
        .into_iter()
        .filter(|(target, _)| !completed.contains(target))
        .collect();

    Some(Recovery {
        txn_id,
        command,
        planned,
        completed,
        in_flight,
        finished,
    })
}

/// Every journal in `dir`, oldest first.
pub fn list(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .ctx("listing journals", dir)?
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("jsonl"))
        .collect();
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_reads_back_a_complete_run() {
        let tmp = tempfile::tempdir().unwrap();
        let mut journal = Journal::create(tmp.path(), "txn-1", "copy").unwrap();
        journal
            .append(&Entry::Plan {
                targets: vec!["/dest/a".into()],
            })
            .unwrap();
        journal
            .append(&Entry::BeforeMutate {
                target: "/dest/a".into(),
                fingerprint: Some("old".into()),
                backup: Some("/backups/a".into()),
                at: crate::state::now(),
            })
            .unwrap();
        journal
            .append(&Entry::AfterMutate {
                target: "/dest/a".into(),
                fingerprint: Some("new".into()),
                at: crate::state::now(),
            })
            .unwrap();
        let path = journal.finish("committed").unwrap();

        let entries = read(&path).unwrap();
        assert_eq!(entries.len(), 5);
        let recovery = analyse(&entries).unwrap();
        assert!(recovery.finished);
        assert!(recovery.in_flight.is_empty());
        assert_eq!(recovery.completed, vec!["/dest/a"]);
        assert!(!recovery.needs_recovery());
    }

    #[test]
    fn an_interrupted_mutation_is_identified_with_its_backup() {
        // BeforeMutate without AfterMutate is the crash case that matters.
        let tmp = tempfile::tempdir().unwrap();
        let mut journal = Journal::create(tmp.path(), "txn-2", "migrate").unwrap();
        journal
            .append(&Entry::Plan {
                targets: vec!["/dest/a".into(), "/dest/b".into()],
            })
            .unwrap();
        journal
            .append(&Entry::BeforeMutate {
                target: "/dest/a".into(),
                fingerprint: Some("a-old".into()),
                backup: Some("/backups/txn-2/a".into()),
                at: crate::state::now(),
            })
            .unwrap();
        journal
            .append(&Entry::AfterMutate {
                target: "/dest/a".into(),
                fingerprint: Some("a-new".into()),
                at: crate::state::now(),
            })
            .unwrap();
        journal
            .append(&Entry::BeforeMutate {
                target: "/dest/b".into(),
                fingerprint: Some("b-old".into()),
                backup: Some("/backups/txn-2/b".into()),
                at: crate::state::now(),
            })
            .unwrap();
        // Process dies here: no AfterMutate, no End.
        let path = journal.path().to_path_buf();
        drop(journal);

        let recovery = analyse(&read(&path).unwrap()).unwrap();
        assert!(!recovery.finished);
        assert!(recovery.needs_recovery());
        assert_eq!(recovery.completed, vec!["/dest/a"]);
        assert_eq!(
            recovery.in_flight,
            vec![("/dest/b".to_string(), Some("/backups/txn-2/b".to_string()))],
            "the half-written target and its backup must both be identified"
        );
    }

    #[test]
    fn a_truncated_final_line_is_recoverable() {
        let tmp = tempfile::tempdir().unwrap();
        let mut journal = Journal::create(tmp.path(), "txn-3", "copy").unwrap();
        journal
            .append(&Entry::Plan {
                targets: vec!["/dest/a".into()],
            })
            .unwrap();
        let path = journal.path().to_path_buf();
        drop(journal);

        // Simulate a power loss mid-write.
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("{\"event\":\"before_mut");
        std::fs::write(&path, text).unwrap();

        let entries = read(&path).unwrap();
        assert_eq!(entries.len(), 2, "complete entries must still be readable");
        let recovery = analyse(&entries).unwrap();
        assert!(!recovery.finished);
    }

    #[test]
    fn a_journal_cannot_be_created_twice() {
        // Reusing a transaction id would interleave two runs in one file.
        let tmp = tempfile::tempdir().unwrap();
        let _first = Journal::create(tmp.path(), "txn-4", "copy").unwrap();
        assert!(Journal::create(tmp.path(), "txn-4", "copy").is_err());
    }

    #[test]
    fn journals_are_listed_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        for id in ["txn-a", "txn-b"] {
            Journal::create(tmp.path(), id, "copy")
                .unwrap()
                .finish("committed")
                .unwrap();
        }
        let found = list(tmp.path()).unwrap();
        assert_eq!(found.len(), 2);
        assert!(found[0].ends_with("txn-a.jsonl"));
        assert!(list(&tmp.path().join("absent")).unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_journal_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let journal = Journal::create(tmp.path(), "txn-5", "copy").unwrap();
        let mode = std::fs::metadata(journal.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}
