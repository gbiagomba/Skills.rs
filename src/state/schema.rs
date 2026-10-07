//! Schema definition and forward migrations.
//!
//! The schema version is stored in the database and checked on every open. A
//! database written by a newer `skill` is refused rather than guessed at, because
//! writing to a schema we do not understand is how state gets corrupted.
//!
//! Migrations are append-only: add a new entry to [`MIGRATIONS`], never edit an
//! existing one, so an older database upgrades through the same steps that were
//! tested.

use rusqlite::Connection;

use crate::error::{Error, Result};

/// The schema version this binary writes and understands.
pub const SUPPORTED_VERSION: u32 = 1;

/// Each migration is the SQL that moves the schema to `version`.
pub const MIGRATIONS: &[(u32, &str)] = &[(1, V1)];

const V1: &str = r#"
CREATE TABLE package (
    id                TEXT PRIMARY KEY NOT NULL,
    install_name      TEXT NOT NULL,
    install_name_folded TEXT NOT NULL,
    display_name      TEXT,
    source_type       TEXT NOT NULL,
    locator           TEXT NOT NULL,
    selector          TEXT NOT NULL DEFAULT '',
    requested_ref     TEXT,
    resolved_revision TEXT,
    pin_policy        TEXT NOT NULL DEFAULT 'tracking',
    trusted_origin    INTEGER NOT NULL DEFAULT 0,
    acquired_at       TEXT NOT NULL,
    last_checked_at   TEXT
);

-- Two packages may not claim the same install name, and the comparison is
-- case-folded because macOS and Windows filesystems are.
CREATE UNIQUE INDEX package_install_name_unique ON package (install_name_folded);

CREATE TABLE snapshot (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    package_id  TEXT NOT NULL REFERENCES package(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL,
    digest      TEXT NOT NULL,
    total_bytes INTEGER NOT NULL DEFAULT 0,
    recorded_at TEXT NOT NULL
);

CREATE INDEX snapshot_by_package ON snapshot (package_id, kind);

CREATE TABLE snapshot_file (
    snapshot_id INTEGER NOT NULL REFERENCES snapshot(id) ON DELETE CASCADE,
    path        TEXT NOT NULL,
    kind        TEXT NOT NULL,
    mode        INTEGER,
    size        INTEGER NOT NULL DEFAULT 0,
    digest      TEXT NOT NULL,
    link_target TEXT,
    PRIMARY KEY (snapshot_id, path)
) WITHOUT ROWID;

CREATE TABLE deployment (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    package_id           TEXT NOT NULL REFERENCES package(id) ON DELETE CASCADE,
    agent                TEXT NOT NULL,
    scope                TEXT NOT NULL,
    path                 TEXT NOT NULL,
    mode                 TEXT NOT NULL,
    baseline_snapshot_id INTEGER REFERENCES snapshot(id) ON DELETE SET NULL,
    deployed_at          TEXT NOT NULL,
    updated_at           TEXT NOT NULL
);

-- One deployment per physical path. Two agents sharing a directory is one row,
-- which is what stops a shared target being written or deleted twice.
CREATE UNIQUE INDEX deployment_path_unique ON deployment (path);
CREATE INDEX deployment_by_package ON deployment (package_id);

CREATE TABLE txn (
    id          TEXT PRIMARY KEY NOT NULL,
    command     TEXT NOT NULL,
    status      TEXT NOT NULL,
    started_at  TEXT NOT NULL,
    finished_at TEXT,
    journal     TEXT
);

CREATE INDEX txn_by_status ON txn (status);

-- Two digests, because rollback needs both. `digest_before` is the content we
-- saved, which is what a restore puts back. `digest_after` is what the
-- transaction then wrote, which is what the destination should still look like
-- if nobody has touched it since. Comparing the destination against
-- `digest_after` is how rollback tells "untouched" from "edited afterwards";
-- comparing against `digest_before` would always differ after a successful
-- write and would refuse every legitimate rollback.
CREATE TABLE backup (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    txn_id        TEXT NOT NULL REFERENCES txn(id) ON DELETE CASCADE,
    original_path TEXT NOT NULL,
    backup_path   TEXT NOT NULL,
    digest_before TEXT NOT NULL,
    digest_after  TEXT,
    created_at    TEXT NOT NULL
);

CREATE INDEX backup_by_txn ON backup (txn_id);
"#;

/// Read the stored schema version, or `0` for a fresh database.
pub fn read_version(conn: &Connection) -> Result<u32> {
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|err| Error::State {
            path: std::path::PathBuf::from("<open connection>"),
            reason: format!("could not read the schema version: {err}"),
        })?;
    Ok(version.max(0) as u32)
}

/// Bring the database up to [`SUPPORTED_VERSION`], or refuse a newer one.
///
/// Each step runs in its own transaction together with the version bump, so an
/// interrupted migration leaves the database at the last fully applied version
/// rather than halfway through one.
pub fn migrate(conn: &mut Connection, db_path: &std::path::Path) -> Result<u32> {
    let current = read_version(conn)?;

    if current > SUPPORTED_VERSION {
        return Err(Error::SchemaTooNew {
            found: current,
            supported: SUPPORTED_VERSION,
        });
    }

    for (version, sql) in MIGRATIONS {
        if *version <= current {
            continue;
        }
        let tx = conn.transaction().map_err(|err| Error::State {
            path: db_path.to_path_buf(),
            reason: format!("could not begin the migration to v{version}: {err}"),
        })?;
        tx.execute_batch(sql).map_err(|err| Error::State {
            path: db_path.to_path_buf(),
            reason: format!("migration to v{version} failed: {err}"),
        })?;
        tx.pragma_update(None, "user_version", *version as i64)
            .map_err(|err| Error::State {
                path: db_path.to_path_buf(),
                reason: format!("could not record schema v{version}: {err}"),
            })?;
        tx.commit().map_err(|err| Error::State {
            path: db_path.to_path_buf(),
            reason: format!("could not commit the migration to v{version}: {err}"),
        })?;
    }

    read_version(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_are_ordered_and_contiguous() {
        // An out-of-order or duplicated migration would apply unpredictably.
        for (index, (version, _)) in MIGRATIONS.iter().enumerate() {
            assert_eq!(
                *version,
                index as u32 + 1,
                "migration {index} should declare version {}",
                index + 1
            );
        }
        assert_eq!(
            MIGRATIONS.last().map(|(v, _)| *v),
            Some(SUPPORTED_VERSION),
            "SUPPORTED_VERSION must match the last migration"
        );
    }

    #[test]
    fn migrates_a_fresh_database_then_is_idempotent() {
        let mut conn = Connection::open_in_memory().unwrap();
        let path = std::path::Path::new(":memory:");
        assert_eq!(read_version(&conn).unwrap(), 0);

        assert_eq!(migrate(&mut conn, path).unwrap(), SUPPORTED_VERSION);
        // Running again must be a no-op, not an error.
        assert_eq!(migrate(&mut conn, path).unwrap(), SUPPORTED_VERSION);
    }

    #[test]
    fn refuses_a_newer_schema_instead_of_guessing() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", 999_i64).unwrap();
        let err = migrate(&mut conn, std::path::Path::new(":memory:")).unwrap_err();
        assert!(
            matches!(err, Error::SchemaTooNew { found: 999, .. }),
            "{err:?}"
        );
        assert_eq!(err.exit_code(), crate::ExitCode::State);
        assert!(err.to_string().contains("upgrade skill"));
    }

    #[test]
    fn install_names_are_unique_case_insensitively() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn, std::path::Path::new(":memory:")).unwrap();

        let insert = "INSERT INTO package (id, install_name, install_name_folded, source_type, \
                      locator, acquired_at) VALUES (?, ?, ?, 'filesystem', '/x', '2026-01-01')";
        conn.execute(insert, rusqlite::params!["a", "My-Skill", "my-skill"])
            .unwrap();
        let clash = conn.execute(insert, rusqlite::params!["b", "my-skill", "my-skill"]);
        assert!(
            clash.is_err(),
            "a case-folded duplicate must be rejected by the schema"
        );
    }

    #[test]
    fn one_deployment_per_physical_path() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn, std::path::Path::new(":memory:")).unwrap();
        conn.execute(
            "INSERT INTO package (id, install_name, install_name_folded, source_type, locator, \
             acquired_at) VALUES ('p', 'n', 'n', 'filesystem', '/x', '2026-01-01')",
            [],
        )
        .unwrap();

        let insert = "INSERT INTO deployment (package_id, agent, scope, path, mode, deployed_at, \
                      updated_at) VALUES ('p', ?, 'user', '/shared/n', 'copy', 't', 't')";
        conn.execute(insert, rusqlite::params!["codex"]).unwrap();
        assert!(
            conn.execute(insert, rusqlite::params!["gemini"]).is_err(),
            "a shared physical target must be a single deployment row"
        );
    }
}
