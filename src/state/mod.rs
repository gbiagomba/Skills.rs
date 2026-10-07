//! The transactional state store.
//!
//! SQLite is used because every property this store needs is one SQLite already
//! has: a real transaction boundary so a crash cannot leave half a record, a
//! durable schema version to migrate from, and a single file to back up. The
//! `bundled` feature compiles SQLite in, so there is no system library to install
//! and cross-compilation stays clean.
//!
//! The store never holds a credential. Locators are passed through
//! [`crate::pkg::identity::sanitize_locator`] before they arrive here.

pub mod models;
pub mod schema;

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension};

use crate::agent::Scope;
use crate::error::{Error, Result};
use crate::pkg::identity::{InstallName, PackageId, SourceType};
use crate::pkg::tree::{FileEntry, PackageTree};
use crate::safepath::{self, EntryKind};
use models::*;

/// An ISO-8601 UTC timestamp, used for audit fields only.
///
/// Timestamps are never a change signal; digests are. This is recorded so an
/// operator can see when something happened, nothing more.
pub fn now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // A small civil-time conversion, so the crate does not take a date-time
    // dependency purely to format an audit field.
    let days = secs / 86_400;
    let time_of_day = secs % 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        time_of_day / 3600,
        (time_of_day % 3600) / 60,
        time_of_day % 60
    )
}

/// Days since the Unix epoch to a civil date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Handle on the state database.
pub struct Store {
    conn: Connection,
    path: PathBuf,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").field("path", &self.path).finish()
    }
}

impl Store {
    /// Open or create the database at `path`, migrating it to the current schema.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| Error::io("creating the state directory", parent, err))?;
        }

        let existed = path.exists();
        let mut conn = Connection::open(path).map_err(|err| Error::State {
            path: path.to_path_buf(),
            reason: format!("could not open the database: {err}"),
        })?;

        // WAL survives a crash mid-write; foreign keys make the cascades real.
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|err| Error::State {
                path: path.to_path_buf(),
                reason: format!("could not enable write-ahead logging: {err}"),
            })?;
        conn.pragma_update(None, "foreign_keys", true)
            .map_err(|err| Error::State {
                path: path.to_path_buf(),
                reason: format!("could not enable foreign keys: {err}"),
            })?;
        conn.pragma_update(None, "synchronous", "FULL")
            .map_err(|err| Error::State {
                path: path.to_path_buf(),
                reason: format!("could not set the durability level: {err}"),
            })?;

        schema::migrate(&mut conn, path)?;

        if !existed {
            // The database records source locators and package inventories, so it
            // is not world readable.
            safepath::restrict_file(path)?;
        }

        Ok(Self {
            conn,
            path: path.to_path_buf(),
        })
    }

    /// Open an in-memory store, for tests.
    pub fn in_memory() -> Result<Self> {
        let mut conn = Connection::open_in_memory().map_err(|err| Error::State {
            path: PathBuf::from(":memory:"),
            reason: format!("could not open an in-memory database: {err}"),
        })?;
        conn.pragma_update(None, "foreign_keys", true).ok();
        schema::migrate(&mut conn, Path::new(":memory:"))?;
        Ok(Self {
            conn,
            path: PathBuf::from(":memory:"),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn state_err(&self, reason: impl std::fmt::Display) -> Error {
        Error::State {
            path: self.path.clone(),
            reason: reason.to_string(),
        }
    }

    // ---- packages --------------------------------------------------------

    /// Insert or replace a package record.
    ///
    /// Refuses when a *different* package already holds the install name, which
    /// is the same-name-different-origin case: two origins may not silently
    /// occupy one directory.
    pub fn upsert_package(&self, record: &PackageRecord) -> Result<()> {
        if let Some(existing) = self.package_by_install_name(record.install_name.as_str())? {
            if existing.id != record.id {
                return Err(Error::NameCollision {
                    name: record.install_name.as_str().to_string(),
                    existing: format!("{} {}", existing.source_type, existing.locator),
                });
            }
        }

        self.conn
            .execute(
                "INSERT INTO package (id, install_name, install_name_folded, display_name, \
                 source_type, locator, selector, requested_ref, resolved_revision, pin_policy, \
                 trusted_origin, acquired_at, last_checked_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13) \
                 ON CONFLICT(id) DO UPDATE SET \
                 install_name=excluded.install_name, \
                 install_name_folded=excluded.install_name_folded, \
                 display_name=excluded.display_name, \
                 locator=excluded.locator, selector=excluded.selector, \
                 requested_ref=excluded.requested_ref, \
                 resolved_revision=excluded.resolved_revision, \
                 pin_policy=excluded.pin_policy, trusted_origin=excluded.trusted_origin, \
                 last_checked_at=excluded.last_checked_at",
                rusqlite::params![
                    record.id.as_str(),
                    record.install_name.as_str(),
                    record.install_name.folded(),
                    record.display_name,
                    record.source_type.slug(),
                    record.locator,
                    record.selector,
                    record.requested_ref,
                    record.resolved_revision,
                    record.pin_policy.slug(),
                    record.trusted_origin,
                    record.acquired_at,
                    record.last_checked_at,
                ],
            )
            .map_err(|err| self.state_err(format!("could not record the package: {err}")))?;
        Ok(())
    }

    /// Every package, ordered by install name.
    pub fn packages(&self) -> Result<Vec<PackageRecord>> {
        let mut stmt = self
            .conn
            .prepare(&format!("{PACKAGE_SELECT} ORDER BY install_name"))
            .map_err(|err| self.state_err(err))?;
        let rows = stmt
            .query_map([], package_from_row)
            .map_err(|err| self.state_err(err))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|err| self.state_err(err))
    }

    /// Look a package up by its stable id.
    pub fn package(&self, id: &PackageId) -> Result<Option<PackageRecord>> {
        self.conn
            .query_row(
                &format!("{PACKAGE_SELECT} WHERE id = ?1"),
                [id.as_str()],
                package_from_row,
            )
            .optional()
            .map_err(|err| self.state_err(err))
    }

    /// Look a package up by install name, case-insensitively.
    pub fn package_by_install_name(&self, name: &str) -> Result<Option<PackageRecord>> {
        self.conn
            .query_row(
                &format!("{PACKAGE_SELECT} WHERE install_name_folded = ?1"),
                [name.to_lowercase()],
                package_from_row,
            )
            .optional()
            .map_err(|err| self.state_err(err))
    }

    /// Resolve a user-supplied selector to exactly one package.
    pub fn require_package(&self, name: &str) -> Result<PackageRecord> {
        if let Some(found) = self.package_by_install_name(name)? {
            return Ok(found);
        }
        if let Some(found) = self.package(&PackageId::from_stored(name))? {
            return Ok(found);
        }

        let mut known: Vec<String> = self
            .packages()?
            .into_iter()
            .map(|p| p.install_name.as_str().to_string())
            .collect();
        known.sort_unstable();
        Err(Error::Usage(format!(
            "no managed skill named {name:?}\nhint: {}",
            if known.is_empty() {
                "nothing is managed yet; install something with `skill copy <source> <agent>`"
                    .to_string()
            } else {
                format!("managed skills are {}", known.join(", "))
            }
        )))
    }

    /// Record the time of an upstream check without touching anything else.
    pub fn mark_checked(&self, id: &PackageId, at: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE package SET last_checked_at = ?2 WHERE id = ?1",
                rusqlite::params![id.as_str(), at],
            )
            .map_err(|err| self.state_err(err))?;
        Ok(())
    }

    /// Remove a package and, by cascade, its snapshots and deployments.
    ///
    /// Only ever called for an explicit removal decision made further up.
    pub fn delete_package(&self, id: &PackageId) -> Result<()> {
        self.conn
            .execute("DELETE FROM package WHERE id = ?1", [id.as_str()])
            .map_err(|err| self.state_err(err))?;
        Ok(())
    }

    // ---- snapshots -------------------------------------------------------

    /// Record a snapshot and its complete file list in one transaction.
    pub fn record_snapshot(
        &mut self,
        package_id: &PackageId,
        kind: SnapshotKind,
        tree: &PackageTree,
    ) -> Result<i64> {
        let recorded_at = now();
        let tx = self.conn.transaction().map_err(|err| Error::State {
            path: self.path.clone(),
            reason: format!("could not begin a snapshot transaction: {err}"),
        })?;

        // One current snapshot per kind: superseded ones are replaced, so the
        // store does not grow without bound. Deployment baselines are keyed by
        // the deployment row instead and are cleaned up with it.
        if kind != SnapshotKind::DeploymentBaseline {
            tx.execute(
                "DELETE FROM snapshot WHERE package_id = ?1 AND kind = ?2",
                rusqlite::params![package_id.as_str(), kind.slug()],
            )
            .map_err(|err| Error::State {
                path: self.path.clone(),
                reason: format!("could not replace the previous snapshot: {err}"),
            })?;
        }

        tx.execute(
            "INSERT INTO snapshot (package_id, kind, digest, total_bytes, recorded_at) \
             VALUES (?1,?2,?3,?4,?5)",
            rusqlite::params![
                package_id.as_str(),
                kind.slug(),
                tree.digest,
                tree.total_bytes as i64,
                recorded_at
            ],
        )
        .map_err(|err| Error::State {
            path: self.path.clone(),
            reason: format!("could not record the snapshot: {err}"),
        })?;
        let snapshot_id = tx.last_insert_rowid();

        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO snapshot_file (snapshot_id, path, kind, mode, size, digest, \
                     link_target) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                )
                .map_err(|err| Error::State {
                    path: self.path.clone(),
                    reason: format!("could not prepare the file insert: {err}"),
                })?;
            for entry in &tree.entries {
                stmt.execute(rusqlite::params![
                    snapshot_id,
                    entry.path,
                    entry_kind_slug(entry.kind),
                    entry.mode,
                    entry.size as i64,
                    entry.digest,
                    entry.link_target,
                ])
                .map_err(|err| Error::State {
                    path: self.path.clone(),
                    reason: format!("could not record file {}: {err}", entry.path),
                })?;
            }
        }

        tx.commit().map_err(|err| Error::State {
            path: self.path.clone(),
            reason: format!("could not commit the snapshot: {err}"),
        })?;

        Ok(snapshot_id)
    }

    /// Load a snapshot by id, including its file list.
    pub fn snapshot(&self, id: i64) -> Result<Option<SnapshotRecord>> {
        let header = self
            .conn
            .query_row(
                "SELECT id, package_id, kind, digest, total_bytes, recorded_at FROM snapshot \
                 WHERE id = ?1",
                [id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()
            .map_err(|err| self.state_err(err))?;

        let Some((id, package_id, kind, digest, total_bytes, recorded_at)) = header else {
            return Ok(None);
        };
        let kind = SnapshotKind::from_slug(&kind)
            .ok_or_else(|| self.state_err(format!("unknown snapshot kind {kind:?}")))?;

        Ok(Some(SnapshotRecord {
            id,
            package_id: PackageId::from_stored(package_id),
            kind,
            digest: digest.clone(),
            recorded_at,
            tree: PackageTree {
                entries: self.snapshot_files(id)?,
                digest,
                total_bytes: total_bytes.max(0) as u64,
            },
        }))
    }

    /// The most recent snapshot of a given kind for a package.
    pub fn latest_snapshot(
        &self,
        package_id: &PackageId,
        kind: SnapshotKind,
    ) -> Result<Option<SnapshotRecord>> {
        let id: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM snapshot WHERE package_id = ?1 AND kind = ?2 \
                 ORDER BY id DESC LIMIT 1",
                rusqlite::params![package_id.as_str(), kind.slug()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|err| self.state_err(err))?;

        match id {
            Some(id) => self.snapshot(id),
            None => Ok(None),
        }
    }

    fn snapshot_files(&self, snapshot_id: i64) -> Result<Vec<FileEntry>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT path, kind, mode, size, digest, link_target FROM snapshot_file \
                 WHERE snapshot_id = ?1 ORDER BY path",
            )
            .map_err(|err| self.state_err(err))?;
        let rows = stmt
            .query_map([snapshot_id], |row| {
                let kind: String = row.get(1)?;
                Ok(FileEntry {
                    path: row.get(0)?,
                    kind: entry_kind_from_slug(&kind),
                    mode: row.get(2)?,
                    size: row.get::<_, i64>(3)?.max(0) as u64,
                    digest: row.get(4)?,
                    link_target: row.get(5)?,
                })
            })
            .map_err(|err| self.state_err(err))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|err| self.state_err(err))
    }

    // ---- deployments -----------------------------------------------------

    /// Insert or update a deployment, keyed by its physical path.
    pub fn upsert_deployment(
        &self,
        package_id: &PackageId,
        agent: &str,
        scope: Scope,
        path: &Path,
        mode: DeployMode,
        baseline_snapshot_id: Option<i64>,
    ) -> Result<i64> {
        let at = now();
        let key = path.to_string_lossy().to_string();
        self.conn
            .execute(
                "INSERT INTO deployment (package_id, agent, scope, path, mode, \
                 baseline_snapshot_id, deployed_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?7) \
                 ON CONFLICT(path) DO UPDATE SET \
                 package_id=excluded.package_id, agent=excluded.agent, scope=excluded.scope, \
                 mode=excluded.mode, baseline_snapshot_id=excluded.baseline_snapshot_id, \
                 updated_at=excluded.updated_at",
                rusqlite::params![
                    package_id.as_str(),
                    agent,
                    scope.slug(),
                    key,
                    mode.slug(),
                    baseline_snapshot_id,
                    at
                ],
            )
            .map_err(|err| self.state_err(format!("could not record the deployment: {err}")))?;

        self.conn
            .query_row("SELECT id FROM deployment WHERE path = ?1", [key], |row| {
                row.get(0)
            })
            .map_err(|err| self.state_err(err))
    }

    /// Every deployment of one package.
    pub fn deployments(&self, package_id: &PackageId) -> Result<Vec<DeploymentRecord>> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "{DEPLOYMENT_SELECT} WHERE package_id = ?1 ORDER BY agent, path"
            ))
            .map_err(|err| self.state_err(err))?;
        let rows = stmt
            .query_map([package_id.as_str()], deployment_from_row)
            .map_err(|err| self.state_err(err))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|err| self.state_err(err))
    }

    /// Every deployment we track, across all packages.
    pub fn all_deployments(&self) -> Result<Vec<DeploymentRecord>> {
        let mut stmt = self
            .conn
            .prepare(&format!("{DEPLOYMENT_SELECT} ORDER BY agent, path"))
            .map_err(|err| self.state_err(err))?;
        let rows = stmt
            .query_map([], deployment_from_row)
            .map_err(|err| self.state_err(err))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|err| self.state_err(err))
    }

    /// The deployment at a given path, if we manage it.
    pub fn deployment_at(&self, path: &Path) -> Result<Option<DeploymentRecord>> {
        self.conn
            .query_row(
                &format!("{DEPLOYMENT_SELECT} WHERE path = ?1"),
                [path.to_string_lossy().to_string()],
                deployment_from_row,
            )
            .optional()
            .map_err(|err| self.state_err(err))
    }

    /// Forget a deployment. Used by `migrate` after the source is removed.
    pub fn delete_deployment(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM deployment WHERE id = ?1", [id])
            .map_err(|err| self.state_err(err))?;
        Ok(())
    }

    // ---- transactions ----------------------------------------------------

    /// Open a transaction record.
    pub fn begin_txn(&self, id: &str, command: &str, journal: Option<&Path>) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO txn (id, command, status, started_at, journal) \
                 VALUES (?1,?2,'open',?3,?4)",
                rusqlite::params![
                    id,
                    command,
                    now(),
                    journal.map(|p| p.to_string_lossy().to_string())
                ],
            )
            .map_err(|err| self.state_err(format!("could not open the transaction: {err}")))?;
        Ok(())
    }

    /// Record a transaction's outcome.
    pub fn finish_txn(&self, id: &str, status: TxnStatus) -> Result<()> {
        self.conn
            .execute(
                "UPDATE txn SET status = ?2, finished_at = ?3 WHERE id = ?1",
                rusqlite::params![id, status.slug(), now()],
            )
            .map_err(|err| self.state_err(err))?;
        Ok(())
    }

    /// Transactions that need operator attention, newest first.
    ///
    /// Any row still `open` means a previous run died mid-change, which every
    /// mutating command checks for before it starts.
    pub fn unfinished_txns(&self) -> Result<Vec<TxnRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, command, status, started_at, finished_at, journal FROM txn \
                 WHERE status IN ('open','failed') ORDER BY started_at DESC",
            )
            .map_err(|err| self.state_err(err))?;
        let rows = stmt
            .query_map([], txn_from_row)
            .map_err(|err| self.state_err(err))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|err| self.state_err(err))
    }

    /// One transaction by id.
    pub fn txn(&self, id: &str) -> Result<Option<TxnRecord>> {
        self.conn
            .query_row(
                "SELECT id, command, status, started_at, finished_at, journal FROM txn \
                 WHERE id = ?1",
                [id],
                txn_from_row,
            )
            .optional()
            .map_err(|err| self.state_err(err))
    }

    /// Record a backup taken before a destructive step.
    ///
    /// `digest_before` is the content being saved. The digest that was then
    /// written is recorded separately by [`Self::record_backup_result`], once the
    /// write has actually completed and been verified.
    pub fn record_backup(
        &self,
        txn_id: &str,
        original: &Path,
        backup: &Path,
        digest_before: &str,
    ) -> Result<i64> {
        self.conn
            .execute(
                "INSERT INTO backup (txn_id, original_path, backup_path, digest_before, \
                 created_at) VALUES (?1,?2,?3,?4,?5)",
                rusqlite::params![
                    txn_id,
                    original.to_string_lossy().to_string(),
                    backup.to_string_lossy().to_string(),
                    digest_before,
                    now()
                ],
            )
            .map_err(|err| self.state_err(err))?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Record what the transaction actually wrote over a backed-up destination.
    ///
    /// Left unset when the write did not complete, which is how a half-written
    /// target stays distinguishable from a finished one.
    pub fn record_backup_result(&self, backup_id: i64, digest_after: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE backup SET digest_after = ?2 WHERE id = ?1",
                rusqlite::params![backup_id, digest_after],
            )
            .map_err(|err| self.state_err(err))?;
        Ok(())
    }

    /// Backups belonging to one transaction, in the order they were taken.
    pub fn backups(&self, txn_id: &str) -> Result<Vec<BackupRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, txn_id, original_path, backup_path, digest_before, digest_after, \
                 created_at FROM backup WHERE txn_id = ?1 ORDER BY id",
            )
            .map_err(|err| self.state_err(err))?;
        let rows = stmt
            .query_map([txn_id], |row| {
                Ok(BackupRecord {
                    id: row.get(0)?,
                    txn_id: row.get(1)?,
                    original_path: PathBuf::from(row.get::<_, String>(2)?),
                    backup_path: PathBuf::from(row.get::<_, String>(3)?),
                    digest_before: row.get(4)?,
                    digest_after: row.get(5)?,
                    created_at: row.get(6)?,
                })
            })
            .map_err(|err| self.state_err(err))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|err| self.state_err(err))
    }

    /// Run a closure inside a database transaction, rolling back on error.
    pub fn with_transaction<T, F>(&mut self, f: F) -> Result<T>
    where
        F: FnOnce(&rusqlite::Transaction<'_>) -> Result<T>,
    {
        let tx = self.conn.transaction().map_err(|err| Error::State {
            path: self.path.clone(),
            reason: format!("could not begin a transaction: {err}"),
        })?;
        let out = f(&tx)?;
        tx.commit().map_err(|err| Error::State {
            path: self.path.clone(),
            reason: format!("could not commit: {err}"),
        })?;
        Ok(out)
    }
}

const PACKAGE_SELECT: &str = "SELECT id, install_name, display_name, source_type, locator, \
                              selector, requested_ref, resolved_revision, pin_policy, \
                              trusted_origin, acquired_at, last_checked_at FROM package";

const DEPLOYMENT_SELECT: &str = "SELECT id, package_id, agent, scope, path, mode, \
                                 baseline_snapshot_id, deployed_at, updated_at FROM deployment";

fn package_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PackageRecord> {
    let install_raw: String = row.get(1)?;
    let source_raw: String = row.get(3)?;
    Ok(PackageRecord {
        id: PackageId::from_stored(row.get::<_, String>(0)?),
        // The name was validated before it was written; if the database has been
        // edited by hand we fall back rather than panic.
        install_name: InstallName::parse(&install_raw)
            .unwrap_or_else(|_| InstallName::sanitize("unreadable-name").expect("static name")),
        display_name: row.get(2)?,
        source_type: source_type_from_slug(&source_raw),
        locator: row.get(4)?,
        selector: row.get(5)?,
        requested_ref: row.get(6)?,
        resolved_revision: row.get(7)?,
        pin_policy: PinPolicy::from_slug(&row.get::<_, String>(8)?),
        trusted_origin: row.get(9)?,
        acquired_at: row.get(10)?,
        last_checked_at: row.get(11)?,
    })
}

fn deployment_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DeploymentRecord> {
    let scope_raw: String = row.get(3)?;
    let mode_raw: String = row.get(5)?;
    Ok(DeploymentRecord {
        id: row.get(0)?,
        package_id: PackageId::from_stored(row.get::<_, String>(1)?),
        agent: row.get(2)?,
        scope: if scope_raw == "project" {
            Scope::Project
        } else {
            Scope::User
        },
        path: PathBuf::from(row.get::<_, String>(4)?),
        mode: DeployMode::from_slug(&mode_raw).unwrap_or(DeployMode::Copy),
        baseline_snapshot_id: row.get(6)?,
        deployed_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn txn_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TxnRecord> {
    Ok(TxnRecord {
        id: row.get(0)?,
        command: row.get(1)?,
        status: TxnStatus::from_slug(&row.get::<_, String>(2)?),
        started_at: row.get(3)?,
        finished_at: row.get(4)?,
        journal: row.get::<_, Option<String>>(5)?.map(PathBuf::from),
    })
}

fn source_type_from_slug(raw: &str) -> SourceType {
    match raw {
        "git" => SourceType::Git,
        "http" => SourceType::Http,
        "smb" => SourceType::Smb,
        "bundle" => SourceType::Bundle,
        "adopted" => SourceType::Adopted,
        _ => SourceType::Filesystem,
    }
}

fn entry_kind_slug(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::File => "file",
        EntryKind::Dir => "dir",
        EntryKind::InternalSymlink => "link",
    }
}

fn entry_kind_from_slug(raw: &str) -> EntryKind {
    match raw {
        "dir" => EntryKind::Dir,
        "link" => EntryKind::InternalSymlink,
        _ => EntryKind::File,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(name: &str, locator: &str) -> PackageRecord {
        PackageRecord {
            id: PackageId::derive(SourceType::Git, locator, ""),
            install_name: InstallName::parse(name).unwrap(),
            display_name: Some(name.to_string()),
            source_type: SourceType::Git,
            locator: locator.to_string(),
            selector: String::new(),
            requested_ref: Some("main".into()),
            resolved_revision: Some("abc123".into()),
            pin_policy: PinPolicy::Tracking,
            trusted_origin: true,
            acquired_at: now(),
            last_checked_at: None,
        }
    }

    fn tree() -> PackageTree {
        let entries = vec![FileEntry {
            path: "SKILL.md".into(),
            kind: EntryKind::File,
            mode: Some(0o644),
            size: 10,
            digest: "deadbeef".into(),
            link_target: None,
        }];
        PackageTree {
            digest: crate::pkg::tree::tree_digest(&entries),
            entries,
            total_bytes: 10,
        }
    }

    #[test]
    fn round_trips_a_package() {
        let store = Store::in_memory().unwrap();
        let record = pkg("my-skill", "https://example.com/r.git");
        store.upsert_package(&record).unwrap();

        let loaded = store.package(&record.id).unwrap().unwrap();
        assert_eq!(loaded, record);
        assert_eq!(store.packages().unwrap().len(), 1);
        assert_eq!(
            store
                .package_by_install_name("MY-SKILL")
                .unwrap()
                .unwrap()
                .id,
            record.id,
            "install-name lookup must be case-insensitive"
        );
    }

    #[test]
    fn refuses_the_same_name_from_a_different_origin() {
        let store = Store::in_memory().unwrap();
        store
            .upsert_package(&pkg("shared", "https://a/one.git"))
            .unwrap();

        let err = store
            .upsert_package(&pkg("shared", "https://b/two.git"))
            .unwrap_err();
        assert!(matches!(err, Error::NameCollision { .. }), "{err:?}");
        let text = err.to_string();
        assert!(text.contains("--as"), "must suggest an alias: {text}");
    }

    #[test]
    fn upserting_the_same_package_is_idempotent() {
        let store = Store::in_memory().unwrap();
        let record = pkg("my-skill", "https://example.com/r.git");
        store.upsert_package(&record).unwrap();
        store.upsert_package(&record).unwrap();
        assert_eq!(store.packages().unwrap().len(), 1);
    }

    #[test]
    fn keeps_the_three_baselines_apart() {
        let mut store = Store::in_memory().unwrap();
        let record = pkg("my-skill", "https://example.com/r.git");
        store.upsert_package(&record).unwrap();

        let upstream = store
            .record_snapshot(&record.id, SnapshotKind::UpstreamPristine, &tree())
            .unwrap();
        let canonical = store
            .record_snapshot(&record.id, SnapshotKind::Canonical, &tree())
            .unwrap();
        assert_ne!(upstream, canonical);

        let loaded = store
            .latest_snapshot(&record.id, SnapshotKind::UpstreamPristine)
            .unwrap()
            .unwrap();
        assert_eq!(loaded.kind, SnapshotKind::UpstreamPristine);
        assert_eq!(loaded.tree.entries.len(), 1);
        assert_eq!(loaded.tree.digest, tree().digest);
    }

    #[test]
    fn a_new_canonical_snapshot_replaces_the_previous_one() {
        let mut store = Store::in_memory().unwrap();
        let record = pkg("my-skill", "https://example.com/r.git");
        store.upsert_package(&record).unwrap();

        store
            .record_snapshot(&record.id, SnapshotKind::Canonical, &tree())
            .unwrap();
        let mut changed = tree();
        changed.entries[0].digest = "cafe".into();
        changed.digest = crate::pkg::tree::tree_digest(&changed.entries);
        let second = store
            .record_snapshot(&record.id, SnapshotKind::Canonical, &changed)
            .unwrap();

        let latest = store
            .latest_snapshot(&record.id, SnapshotKind::Canonical)
            .unwrap()
            .unwrap();
        assert_eq!(latest.id, second);
        assert_eq!(latest.tree.entries[0].digest, "cafe");
    }

    #[test]
    fn deployment_baselines_are_kept_per_destination() {
        let mut store = Store::in_memory().unwrap();
        let record = pkg("my-skill", "https://example.com/r.git");
        store.upsert_package(&record).unwrap();

        let a = store
            .record_snapshot(&record.id, SnapshotKind::DeploymentBaseline, &tree())
            .unwrap();
        let b = store
            .record_snapshot(&record.id, SnapshotKind::DeploymentBaseline, &tree())
            .unwrap();
        assert_ne!(a, b, "each destination keeps its own baseline");

        store
            .upsert_deployment(
                &record.id,
                "claude",
                Scope::User,
                Path::new("/h/.claude/skills/my-skill"),
                DeployMode::Copy,
                Some(a),
            )
            .unwrap();
        store
            .upsert_deployment(
                &record.id,
                "gemini",
                Scope::User,
                Path::new("/h/.gemini/skills/my-skill"),
                DeployMode::Link,
                Some(b),
            )
            .unwrap();

        let deployments = store.deployments(&record.id).unwrap();
        assert_eq!(deployments.len(), 2);
        assert_eq!(deployments[0].agent, "claude");
        assert_eq!(deployments[0].mode, DeployMode::Copy);
        assert_eq!(deployments[1].mode, DeployMode::Link);
    }

    #[test]
    fn one_physical_path_is_one_deployment_row() {
        let mut store = Store::in_memory().unwrap();
        let record = pkg("my-skill", "https://example.com/r.git");
        store.upsert_package(&record).unwrap();
        let snap = store
            .record_snapshot(&record.id, SnapshotKind::DeploymentBaseline, &tree())
            .unwrap();
        let shared = Path::new("/h/.agents/skills/my-skill");

        let first = store
            .upsert_deployment(
                &record.id,
                "codex",
                Scope::User,
                shared,
                DeployMode::Copy,
                Some(snap),
            )
            .unwrap();
        let second = store
            .upsert_deployment(
                &record.id,
                "gemini",
                Scope::User,
                shared,
                DeployMode::Copy,
                Some(snap),
            )
            .unwrap();

        assert_eq!(first, second, "a shared target must not create two rows");
        assert_eq!(store.deployments(&record.id).unwrap().len(), 1);
    }

    #[test]
    fn deleting_a_package_cascades_to_its_records() {
        let mut store = Store::in_memory().unwrap();
        let record = pkg("my-skill", "https://example.com/r.git");
        store.upsert_package(&record).unwrap();
        let snap = store
            .record_snapshot(&record.id, SnapshotKind::Canonical, &tree())
            .unwrap();
        store
            .upsert_deployment(
                &record.id,
                "claude",
                Scope::User,
                Path::new("/h/x"),
                DeployMode::Copy,
                Some(snap),
            )
            .unwrap();

        store.delete_package(&record.id).unwrap();
        assert!(store.packages().unwrap().is_empty());
        assert!(store.all_deployments().unwrap().is_empty());
        assert!(store.snapshot(snap).unwrap().is_none());
    }

    #[test]
    fn an_open_transaction_is_reported_as_needing_recovery() {
        let store = Store::in_memory().unwrap();
        store.begin_txn("txn-1", "copy", None).unwrap();

        let unfinished = store.unfinished_txns().unwrap();
        assert_eq!(unfinished.len(), 1);
        assert_eq!(unfinished[0].status, TxnStatus::Open);
        assert!(unfinished[0].status.needs_recovery());

        store.finish_txn("txn-1", TxnStatus::Committed).unwrap();
        assert!(store.unfinished_txns().unwrap().is_empty());
        assert_eq!(
            store.txn("txn-1").unwrap().unwrap().status,
            TxnStatus::Committed
        );
    }

    #[test]
    fn backups_record_the_digest_at_backup_time() {
        // rollback needs this to tell "unchanged since" from "edited since".
        let store = Store::in_memory().unwrap();
        store.begin_txn("txn-1", "copy", None).unwrap();
        store
            .record_backup(
                "txn-1",
                Path::new("/dest/skill"),
                Path::new("/backups/txn-1/skill"),
                "digest-at-backup",
            )
            .unwrap();

        let backups = store.backups("txn-1").unwrap();
        assert_eq!(backups.len(), 1);
        assert_eq!(backups[0].digest_before, "digest-at-backup");
        assert_eq!(backups[0].original_path, Path::new("/dest/skill"));
        assert_eq!(
            backups[0].digest_after, None,
            "until the write completes there is no post-mutation digest"
        );

        store
            .record_backup_result(backups[0].id, "digest-written")
            .unwrap();
        let reloaded = store.backups("txn-1").unwrap();
        assert_eq!(reloaded[0].digest_after.as_deref(), Some("digest-written"));
    }

    #[test]
    fn require_package_lists_what_is_available() {
        let store = Store::in_memory().unwrap();
        store
            .upsert_package(&pkg("alpha", "https://a.git"))
            .unwrap();

        assert!(store.require_package("alpha").is_ok());
        let err = store.require_package("missing").unwrap_err();
        assert!(err.to_string().contains("alpha"), "{err}");
    }

    #[test]
    fn opening_a_file_backed_store_creates_private_state() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested/state.sqlite3");
        let store = Store::open(&path).unwrap();
        assert!(path.is_file());
        assert_eq!(store.packages().unwrap().len(), 0);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "the database must not be world readable");
        }

        // Re-opening must migrate cleanly and keep the data.
        drop(store);
        let reopened = Store::open(&path).unwrap();
        assert_eq!(reopened.packages().unwrap().len(), 0);
    }

    #[test]
    fn timestamps_are_iso_8601_utc() {
        let stamp = now();
        assert_eq!(stamp.len(), 20, "{stamp}");
        assert!(stamp.ends_with('Z'));
        assert_eq!(&stamp[4..5], "-");
        assert_eq!(&stamp[10..11], "T");
        // Sanity: the year should be plausible, which catches an epoch mistake.
        let year: i32 = stamp[..4].parse().unwrap();
        assert!((2026..2100).contains(&year), "implausible year in {stamp}");
    }
}
