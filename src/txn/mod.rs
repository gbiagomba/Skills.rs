//! The transaction pipeline every mutating command shares.
//!
//! `plan -> acquire -> validate -> compare -> stage -> backup -> apply -> verify
//! -> commit`
//!
//! Three properties are worth stating plainly, because each costs something and
//! each is there for a reason:
//!
//! * **The lock is process-wide.** A second `skill` run fails with
//!   [`crate::ExitCode::Locked`] rather than racing. Two processes reconciling the
//!   same store would each plan against state the other is changing.
//! * **Fingerprints are re-verified immediately before each mutation**, not only
//!   at planning time. If a target changed in between, the plan is stale and the
//!   apply aborts rather than overwriting work that appeared after the plan was
//!   made.
//! * **Partial failure is reported as partial.** Several destinations may live on
//!   several filesystems, so there is no global atomic apply. What we guarantee is
//!   that each target is replaced atomically, that a backup exists before any
//!   target is touched, and that a partial result is never reported as success.

pub mod journal;

use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::error::{Error, IoContext, Result};
use crate::pkg::tree::{self, PackageTree};
use crate::safepath::{self, Limits};
use crate::state::models::{DeployMode, TxnStatus};
use crate::state::Store;

/// An advisory lock over the store and state directory.
///
/// Released on drop, including on an early return or a panic, so a failed run
/// does not leave the lock held.
#[derive(Debug)]
pub struct Lock {
    file: std::fs::File,
    path: PathBuf,
}

impl Lock {
    /// Take the exclusive lock, or fail immediately if another process holds it.
    ///
    /// Deliberately non-blocking: waiting silently would make a stuck run look
    /// like a slow one.
    pub fn acquire(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ctx("creating the lock directory", parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .ctx("opening the lock file", path)?;
        safepath::restrict_file(path)?;

        // Fully qualified so this resolves to fs4 rather than the inherent
        // `File::try_lock` that newer toolchains also provide, which would
        // otherwise make behaviour depend on the compiler version.
        match fs4::FileExt::try_lock(&file) {
            Ok(()) => Ok(Self {
                file,
                path: path.to_path_buf(),
            }),
            Err(fs4::TryLockError::WouldBlock) => Err(Error::Locked {
                path: path.to_path_buf(),
            }),
            Err(fs4::TryLockError::Error(err)) => {
                Err(Error::io("locking the state directory", path, err))
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        // Best effort: the OS also releases the lock when the handle closes.
        let _ = fs4::FileExt::unlock(&self.file);
    }
}

/// Generate a sortable, collision-resistant transaction id.
///
/// Time-prefixed so journals sort chronologically, with a random suffix so two
/// runs in the same second cannot collide.
pub fn new_txn_id() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{secs:010}-{nanos:09}")
}

/// One target a transaction will write.
#[derive(Debug, Clone)]
pub struct Target {
    /// Where the content goes.
    pub path: PathBuf,
    /// Copy an independent tree, or create a per-skill symlink.
    pub mode: DeployMode,
    /// The canonical directory this target is derived from.
    pub source: PathBuf,
    /// Containment root the target must stay inside.
    pub containment_root: PathBuf,
    /// Digest the target is expected to have right now, `None` when absent.
    ///
    /// Re-checked immediately before mutation to detect concurrent change.
    pub expected_fingerprint: Option<String>,
}

/// What happened to one target.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Outcome {
    pub path: PathBuf,
    pub applied: bool,
    /// Digest after the write, for the deployment baseline.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// A transaction in progress.
#[derive(Debug)]
pub struct Transaction {
    id: String,
    command: String,
    journal: Option<journal::Journal>,
    backup_root: PathBuf,
    limits: Limits,
    dry_run: bool,
    outcomes: Vec<Outcome>,
}

impl Transaction {
    /// Open a transaction, writing its opening journal entry.
    ///
    /// A dry run writes no journal and takes no backups, which is what makes
    /// `--dry-run` genuinely free of side effects.
    pub fn begin(config: &Config, store: &Store, command: &str) -> Result<Self> {
        let id = new_txn_id();

        // Refuse to start on top of an unfinished run. Proceeding would plan
        // against a store that is already mid-change.
        if !config.dry_run {
            if let Some(unfinished) = store.unfinished_txns()?.first() {
                return Err(Error::IncompleteTransaction {
                    id: unfinished.id.clone(),
                });
            }
        }

        let journal = if config.dry_run {
            None
        } else {
            let journal = journal::Journal::create(&config.paths.journal_dir(), &id, command)?;
            store.begin_txn(&id, command, Some(journal.path()))?;
            Some(journal)
        };

        Ok(Self {
            backup_root: config.paths.backup_dir().join(&id),
            id,
            command: command.to_string(),
            journal,
            limits: config.limits,
            dry_run: config.dry_run,
            outcomes: Vec::new(),
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn command(&self) -> &str {
        &self.command
    }

    pub fn outcomes(&self) -> &[Outcome] {
        &self.outcomes
    }

    fn log(&mut self, entry: &journal::Entry) -> Result<()> {
        match self.journal.as_mut() {
            Some(journal) => journal.append(entry),
            None => Ok(()),
        }
    }

    /// Record the plan before touching anything.
    pub fn record_plan(&mut self, targets: &[Target]) -> Result<()> {
        let paths = targets
            .iter()
            .map(|t| t.path.to_string_lossy().to_string())
            .collect();
        self.log(&journal::Entry::Plan { targets: paths })
    }

    /// Apply every target, stopping at the first failure.
    ///
    /// On failure, targets already written are left in place and reported: they
    /// are consistent individually, and silently reverting them could destroy
    /// work. Recovery is an explicit `skill rollback`.
    pub fn apply(&mut self, store: &Store, targets: &[Target]) -> Result<()> {
        self.record_plan(targets)?;

        for (index, target) in targets.iter().enumerate() {
            match self.apply_one(store, target) {
                Ok(outcome) => self.outcomes.push(outcome),
                Err(err) => {
                    self.log(&journal::Entry::Failed {
                        target: target.path.to_string_lossy().to_string(),
                        reason: err.to_string(),
                    })?;

                    if index == 0 {
                        // Nothing was changed, so the original error is the whole
                        // story and a partial-failure wrapper would be misleading.
                        return Err(err);
                    }
                    return Err(Error::PartialFailure {
                        applied: index,
                        planned: targets.len(),
                        reason: err.to_string(),
                        hint: format!(
                            "the {index} target(s) already written are intact; undo them with \
                             `skill rollback {}`",
                            self.id
                        ),
                    });
                }
            }
        }

        Ok(())
    }

    /// Stage, back up, replace, and verify one target.
    fn apply_one(&mut self, store: &Store, target: &Target) -> Result<Outcome> {
        let target_path = &target.path;

        // Re-verify the fingerprint now. The plan may be seconds or minutes old.
        let current = current_digest(target_path, &self.limits)?;
        if current != target.expected_fingerprint {
            return Err(Error::ConcurrentModification {
                path: target_path.clone(),
            });
        }

        if self.dry_run {
            return Ok(Outcome {
                path: target_path.clone(),
                applied: false,
                digest: None,
                backup: None,
                note: Some("dry run: nothing was written".to_string()),
            });
        }

        if let Some(parent) = target_path.parent() {
            std::fs::create_dir_all(parent).ctx("creating the destination directory", parent)?;
        }
        // Containment is checked here, immediately before the write, so an
        // ancestor swapped in since planning is caught.
        safepath::verify_containment(&target.containment_root, target_path)?;

        let backup = if current.is_some() {
            Some(self.backup_path(target_path))
        } else {
            None
        };

        self.log(&journal::Entry::BeforeMutate {
            target: target_path.to_string_lossy().to_string(),
            fingerprint: current.clone(),
            backup: backup.as_ref().map(|p| p.to_string_lossy().to_string()),
            at: crate::state::now(),
        })?;

        let backup_id = match (&backup, &current) {
            (Some(backup_path), Some(digest)) => {
                Some(store.record_backup(&self.id, target_path, backup_path, digest)?)
            }
            _ => None,
        };

        match target.mode {
            DeployMode::Copy => {
                // Stage beside the destination so the final step is a rename on
                // the same filesystem rather than a copy that could half-finish.
                let staging = staging_path(target_path);
                if staging.exists() {
                    let _ = std::fs::remove_dir_all(&staging);
                }
                crate::source::copy_tree(&target.source, &staging, &self.limits)?;
                safepath::replace_dir(&staging, target_path, backup.as_deref())?;
            }
            DeployMode::Link => {
                if let Some(backup_path) = &backup {
                    if let Some(parent) = backup_path.parent() {
                        std::fs::create_dir_all(parent)
                            .ctx("creating the backup directory", parent)?;
                    }
                    std::fs::rename(target_path, backup_path)
                        .ctx("moving the existing deployment aside", target_path)?;
                }
                safepath::symlink_dir(&target.source, target_path)?;
            }
        }

        // Verify by reading back what is actually there, rather than assuming the
        // write produced what we intended.
        let written = current_digest(target_path, &self.limits)?;

        // Record what we wrote, so a later rollback can tell an untouched
        // destination from one edited after this transaction.
        if let (Some(backup_id), Some(written)) = (backup_id, written.as_deref()) {
            store.record_backup_result(backup_id, written)?;
        }

        self.log(&journal::Entry::AfterMutate {
            target: target_path.to_string_lossy().to_string(),
            fingerprint: written.clone(),
            at: crate::state::now(),
        })?;

        Ok(Outcome {
            path: target_path.clone(),
            applied: true,
            digest: written,
            backup,
            note: None,
        })
    }

    /// Note that a target was intentionally skipped.
    pub fn skip(&mut self, path: &Path, reason: &str) -> Result<()> {
        self.log(&journal::Entry::Skipped {
            target: path.to_string_lossy().to_string(),
            reason: reason.to_string(),
        })?;
        self.outcomes.push(Outcome {
            path: path.to_path_buf(),
            applied: false,
            digest: None,
            backup: None,
            note: Some(reason.to_string()),
        });
        Ok(())
    }

    /// Where this transaction's backup of `target` lives.
    fn backup_path(&self, target: &Path) -> PathBuf {
        let name = target
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "target".to_string());
        // Include a hash of the full path so two deployments with the same
        // directory name do not overwrite each other's backup.
        let key = &tree::hash_bytes(target.to_string_lossy().as_bytes())[..12];
        self.backup_root.join(format!("{key}-{name}"))
    }

    /// Commit the transaction.
    pub fn commit(mut self, store: &Store) -> Result<()> {
        if let Some(journal) = self.journal.take() {
            journal.finish(TxnStatus::Committed.slug())?;
            store.finish_txn(&self.id, TxnStatus::Committed)?;
        }
        Ok(())
    }

    /// Mark the transaction failed, leaving its backups in place for recovery.
    pub fn fail(mut self, store: &Store) -> Result<()> {
        if let Some(journal) = self.journal.take() {
            journal.finish(TxnStatus::Failed.slug())?;
            store.finish_txn(&self.id, TxnStatus::Failed)?;
        }
        Ok(())
    }
}

/// Digest the current contents of a deployment path, or `None` if absent.
///
/// A symlink deployment is digested by its target string, so repointing a link
/// counts as a change.
pub fn current_digest(path: &Path, limits: &Limits) -> Result<Option<String>> {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Ok(None);
    };

    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(path).ctx("reading the deployment link", path)?;
        return Ok(Some(tree::hash_bytes(target.to_string_lossy().as_bytes())));
    }
    if meta.file_type().is_dir() {
        return Ok(Some(tree::build(path, limits)?.digest));
    }
    Err(Error::UnsafeEntry {
        entry: path.display().to_string(),
        reason: "deployment path is neither a directory nor a link".into(),
    })
}

/// Read a deployment's package tree, when it is a real directory.
pub fn deployed_tree(path: &Path, limits: &Limits) -> Result<Option<PackageTree>> {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Ok(None);
    };
    if meta.file_type().is_dir() && !meta.file_type().is_symlink() {
        return Ok(Some(tree::build(path, limits)?));
    }
    Ok(None)
}

/// A staging directory beside `target`, on the same filesystem.
fn staging_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "pkg".to_string());
    let parent = target.parent().unwrap_or(Path::new("."));
    parent.join(format!(".skill-staging-{name}"))
}

/// Restore the targets recorded for a transaction.
///
/// Refuses to overwrite a destination that has been edited since the backup was
/// taken, because that edit is work the operator did after the transaction and
/// rollback must not destroy it. The refusal names the path and says how to
/// proceed deliberately.
pub fn rollback(config: &Config, store: &Store, txn_id: &str) -> Result<Vec<Outcome>> {
    let record = store.txn(txn_id)?.ok_or_else(|| {
        Error::Usage(format!(
            "no transaction {txn_id:?} is recorded\nhint: run `skill doctor` to list recent \
             transactions"
        ))
    })?;

    let backups = store.backups(txn_id)?;
    if backups.is_empty() {
        return Err(Error::Usage(format!(
            "transaction {txn_id} has no recorded backups, so there is nothing to restore\n\
             hint: it either made no destructive change or was a dry run"
        )));
    }

    let mut outcomes = Vec::new();
    let mut refused = Vec::new();

    for backup in &backups {
        let current = current_digest(&backup.original_path, &config.limits)?;

        // Compare the destination against what this transaction wrote. Equal
        // means nobody has touched it since, so restoring is safe. Different
        // means there are later edits, and restoring would destroy them.
        //
        // Comparing against `digest_before` instead would be wrong: after a
        // successful write the destination never matches the saved content, so
        // every legitimate rollback would be refused.
        if let Some(current) = &current {
            match backup.digest_after.as_deref() {
                Some(written) if current == written => {}
                Some(_) => {
                    let after_txn = record
                        .finished_at
                        .as_deref()
                        .unwrap_or(record.started_at.as_str());
                    refused.push(format!(
                        "{} was edited after the transaction completed at {after_txn}; restoring \
                         the backup would discard those edits",
                        backup.original_path.display()
                    ));
                    continue;
                }
                None => {
                    // The write never completed, so the destination may be
                    // half-written and we cannot attribute its current state.
                    // Restoring the backup is the right move here.
                    if current == &backup.digest_before {
                        // Nothing was actually replaced; leave it alone.
                        outcomes.push(Outcome {
                            path: backup.original_path.clone(),
                            applied: false,
                            digest: current.clone().into(),
                            backup: Some(backup.backup_path.clone()),
                            note: Some(
                                "already holds the pre-transaction content; nothing to restore"
                                    .to_string(),
                            ),
                        });
                        continue;
                    }
                }
            }
        }

        if config.dry_run {
            outcomes.push(Outcome {
                path: backup.original_path.clone(),
                applied: false,
                digest: None,
                backup: Some(backup.backup_path.clone()),
                note: Some("dry run: would restore this backup".to_string()),
            });
            continue;
        }

        if !backup.backup_path.exists() {
            refused.push(format!(
                "the backup for {} is missing from {}",
                backup.original_path.display(),
                backup.backup_path.display()
            ));
            continue;
        }

        if current.is_some() {
            safepath::remove_deployment(&backup.original_path)?;
        }
        if let Some(parent) = backup.original_path.parent() {
            std::fs::create_dir_all(parent).ctx("recreating the destination parent", parent)?;
        }
        std::fs::rename(&backup.backup_path, &backup.original_path)
            .ctx("restoring the backup", &backup.backup_path)?;

        outcomes.push(Outcome {
            path: backup.original_path.clone(),
            applied: true,
            digest: current_digest(&backup.original_path, &config.limits)?,
            backup: Some(backup.backup_path.clone()),
            note: Some("restored from backup".to_string()),
        });
    }

    if !refused.is_empty() {
        return Err(Error::Conflict {
            count: refused.len(),
            hint: format!(
                "{}\nreview each path, then move or delete it yourself before retrying the \
                 rollback",
                refused.join("\n")
            ),
        });
    }

    if !config.dry_run {
        store.finish_txn(txn_id, TxnStatus::RolledBack)?;
    }
    Ok(outcomes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkg::identity::{InstallName, PackageId, SourceType};
    use crate::state::models::{PackageRecord, PinPolicy};

    fn fixture_package(dir: &Path) {
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            "---\nname: t\ndescription: d\n---\nbody\n",
        )
        .unwrap();
        std::fs::write(dir.join("scripts/run.sh"), "#!/bin/sh\n").unwrap();
    }

    fn setup() -> (tempfile::TempDir, Config, Store) {
        let tmp = tempfile::tempdir().unwrap();
        let config = Config::for_test(tmp.path());
        config.paths.ensure().unwrap();
        let store = Store::open(&config.paths.database()).unwrap();
        (tmp, config, store)
    }

    fn record(store: &Store) -> PackageRecord {
        let rec = PackageRecord {
            id: PackageId::derive(SourceType::Filesystem, "/src", ""),
            install_name: InstallName::parse("my-skill").unwrap(),
            display_name: None,
            source_type: SourceType::Filesystem,
            locator: "/src".into(),
            selector: String::new(),
            requested_ref: None,
            resolved_revision: None,
            pin_policy: PinPolicy::Tracking,
            trusted_origin: false,
            acquired_at: crate::state::now(),
            last_checked_at: None,
        };
        store.upsert_package(&rec).unwrap();
        rec
    }

    #[test]
    fn the_lock_is_exclusive_across_handles() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("skill.lock");

        let first = Lock::acquire(&path).unwrap();
        let err = Lock::acquire(&path).unwrap_err();
        assert!(matches!(err, Error::Locked { .. }), "{err:?}");
        assert_eq!(err.exit_code(), crate::ExitCode::Locked);

        // Released on drop, so a later run succeeds.
        drop(first);
        assert!(Lock::acquire(&path).is_ok());
    }

    #[test]
    fn copies_a_package_and_records_a_verified_digest() {
        let (tmp, config, store) = setup();
        let source = tmp.path().join("canonical/my-skill");
        fixture_package(&source);
        let dest_root = tmp.path().join("agent/skills");
        std::fs::create_dir_all(&dest_root).unwrap();
        let dest = dest_root.join("my-skill");

        let mut txn = Transaction::begin(&config, &store, "copy").unwrap();
        txn.apply(
            &store,
            &[Target {
                path: dest.clone(),
                mode: DeployMode::Copy,
                source: source.clone(),
                containment_root: dest_root.clone(),
                expected_fingerprint: None,
            }],
        )
        .unwrap();

        let expected = tree::build(&source, &config.limits).unwrap().digest;
        assert_eq!(txn.outcomes()[0].digest.as_deref(), Some(expected.as_str()));
        assert!(dest.join("scripts/run.sh").is_file());
        txn.commit(&store).unwrap();

        assert!(store.unfinished_txns().unwrap().is_empty());
    }

    #[test]
    fn links_a_package_without_copying_it() {
        let (tmp, config, store) = setup();
        let source = tmp.path().join("canonical/my-skill");
        fixture_package(&source);
        let dest_root = tmp.path().join("agent/skills");
        std::fs::create_dir_all(&dest_root).unwrap();
        let dest = dest_root.join("my-skill");

        let mut txn = Transaction::begin(&config, &store, "link").unwrap();
        txn.apply(
            &store,
            &[Target {
                path: dest.clone(),
                mode: DeployMode::Link,
                source: source.clone(),
                containment_root: dest_root,
                expected_fingerprint: None,
            }],
        )
        .unwrap();
        txn.commit(&store).unwrap();

        let meta = std::fs::symlink_metadata(&dest).unwrap();
        assert!(meta.file_type().is_symlink());
        // Reading through the link sees canonical content.
        assert!(dest.join("SKILL.md").is_file());
    }

    #[test]
    fn a_target_changed_since_planning_aborts_instead_of_overwriting() {
        let (tmp, config, store) = setup();
        let source = tmp.path().join("canonical/my-skill");
        fixture_package(&source);
        let dest_root = tmp.path().join("agent/skills");
        let dest = dest_root.join("my-skill");
        fixture_package(&dest);

        // Plan says the destination is empty, but it is not.
        let mut txn = Transaction::begin(&config, &store, "copy").unwrap();
        let err = txn
            .apply(
                &store,
                &[Target {
                    path: dest.clone(),
                    mode: DeployMode::Copy,
                    source,
                    containment_root: dest_root,
                    expected_fingerprint: None,
                }],
            )
            .unwrap_err();

        assert!(
            matches!(err, Error::ConcurrentModification { .. }),
            "{err:?}"
        );
        assert_eq!(err.exit_code(), crate::ExitCode::Partial);
        assert!(err.to_string().contains("re-run the command"));
    }

    #[test]
    fn a_dry_run_writes_nothing_at_all() {
        let (tmp, mut config, store) = setup();
        config.dry_run = true;
        let source = tmp.path().join("canonical/my-skill");
        fixture_package(&source);
        let dest_root = tmp.path().join("agent/skills");
        std::fs::create_dir_all(&dest_root).unwrap();
        let dest = dest_root.join("my-skill");

        let mut txn = Transaction::begin(&config, &store, "copy").unwrap();
        txn.apply(
            &store,
            &[Target {
                path: dest.clone(),
                mode: DeployMode::Copy,
                source,
                containment_root: dest_root,
                expected_fingerprint: None,
            }],
        )
        .unwrap();

        assert!(!dest.exists(), "a dry run must not create the destination");
        assert!(!txn.outcomes()[0].applied);
        // No journal, no transaction row, no backup directory.
        assert!(journal::list(&config.paths.journal_dir())
            .unwrap()
            .is_empty());
        assert!(store.txn(txn.id()).unwrap().is_none());
    }

    #[test]
    fn refuses_to_start_on_top_of_an_unfinished_transaction() {
        let (_tmp, config, store) = setup();
        store.begin_txn("stale-txn", "copy", None).unwrap();

        let err = Transaction::begin(&config, &store, "copy").unwrap_err();
        assert!(
            matches!(err, Error::IncompleteTransaction { .. }),
            "{err:?}"
        );
        let text = err.to_string();
        assert!(text.contains("skill doctor"), "{text}");
        assert!(text.contains("skill rollback"), "{text}");
    }

    #[test]
    fn a_dry_run_is_allowed_while_a_transaction_is_unfinished() {
        // Inspecting a damaged store must still be possible.
        let (_tmp, mut config, store) = setup();
        config.dry_run = true;
        store.begin_txn("stale-txn", "copy", None).unwrap();
        assert!(Transaction::begin(&config, &store, "copy").is_ok());
    }

    #[test]
    fn rollback_restores_the_previous_contents() {
        let (tmp, config, store) = setup();
        let rec = record(&store);
        let source = tmp.path().join("canonical/my-skill");
        fixture_package(&source);

        let dest_root = tmp.path().join("agent/skills");
        let dest = dest_root.join("my-skill");
        fixture_package(&dest);
        std::fs::write(dest.join("SKILL.md"), "ORIGINAL\n").unwrap();
        let original = tree::build(&dest, &config.limits).unwrap();

        let mut txn = Transaction::begin(&config, &store, "copy").unwrap();
        let txn_id = txn.id().to_string();
        txn.apply(
            &store,
            &[Target {
                path: dest.clone(),
                mode: DeployMode::Copy,
                source,
                containment_root: dest_root,
                expected_fingerprint: Some(original.digest.clone()),
            }],
        )
        .unwrap();
        txn.commit(&store).unwrap();

        assert_ne!(
            tree::build(&dest, &config.limits).unwrap().digest,
            original.digest
        );

        let outcomes = rollback(&config, &store, &txn_id).unwrap();
        assert_eq!(outcomes.len(), 1);
        assert!(outcomes[0].applied);
        assert_eq!(
            tree::build(&dest, &config.limits).unwrap().digest,
            original.digest,
            "rollback must restore the exact previous contents"
        );
        let _ = rec;
    }

    #[test]
    fn rollback_refuses_to_discard_edits_made_after_the_transaction() {
        let (tmp, config, store) = setup();
        let source = tmp.path().join("canonical/my-skill");
        fixture_package(&source);
        let dest_root = tmp.path().join("agent/skills");
        let dest = dest_root.join("my-skill");
        fixture_package(&dest);
        std::fs::write(dest.join("SKILL.md"), "ORIGINAL\n").unwrap();
        let original = tree::build(&dest, &config.limits).unwrap();

        let mut txn = Transaction::begin(&config, &store, "copy").unwrap();
        let txn_id = txn.id().to_string();
        txn.apply(
            &store,
            &[Target {
                path: dest.clone(),
                mode: DeployMode::Copy,
                source,
                containment_root: dest_root,
                expected_fingerprint: Some(original.digest),
            }],
        )
        .unwrap();
        txn.commit(&store).unwrap();

        // The operator edits the destination after the transaction.
        std::fs::write(dest.join("SKILL.md"), "EDITED AFTERWARDS\n").unwrap();

        let err = rollback(&config, &store, &txn_id).unwrap_err();
        assert_eq!(err.exit_code(), crate::ExitCode::Conflict);
        let text = err.to_string();
        assert!(
            text.contains("edited after the transaction"),
            "the refusal must say why: {text}"
        );
        assert_eq!(
            std::fs::read_to_string(dest.join("SKILL.md")).unwrap(),
            "EDITED AFTERWARDS\n",
            "the later edit must survive the refused rollback"
        );
    }

    #[test]
    fn rollback_of_an_unknown_transaction_is_actionable() {
        let (_tmp, config, store) = setup();
        let err = rollback(&config, &store, "no-such-txn").unwrap_err();
        assert!(err.to_string().contains("skill doctor"), "{err}");
    }

    #[test]
    fn current_digest_distinguishes_absent_copy_and_link() {
        let tmp = tempfile::tempdir().unwrap();
        let limits = Limits::default();
        let absent = tmp.path().join("absent");
        assert_eq!(current_digest(&absent, &limits).unwrap(), None);

        let dir = tmp.path().join("dir");
        fixture_package(&dir);
        let dir_digest = current_digest(&dir, &limits).unwrap().unwrap();

        let link = tmp.path().join("link");
        safepath::symlink_dir(&dir, &link).unwrap();
        let link_digest = current_digest(&link, &limits).unwrap().unwrap();
        assert_ne!(
            dir_digest, link_digest,
            "a link must not be mistaken for a copy of its target"
        );
    }

    #[test]
    fn backup_paths_do_not_collide_for_same_named_deployments() {
        let (_tmp, config, store) = setup();
        let txn = Transaction::begin(&config, &store, "copy").unwrap();
        let a = txn.backup_path(Path::new("/one/.claude/skills/my-skill"));
        let b = txn.backup_path(Path::new("/two/.gemini/skills/my-skill"));
        assert_ne!(a, b, "same leaf name from different roots must not collide");
    }
}
