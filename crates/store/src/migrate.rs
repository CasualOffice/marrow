//! Forward-only numbered migrations (Part 6 §107).
//!
//! Three rules, in order of how expensive they are to get wrong:
//!
//! 1. **A `VACUUM INTO` backup is taken before any migration runs.** Invariant
//!    #11 / hard rule 8: derived indexes are rebuildable, corrections are not.
//!    The recovery mechanism is the backup, not a `down` migration — so there
//!    are no `down` migrations.
//! 2. **A failed migration restores the backup and refuses to open for writes**
//!    (NFR-011). Half-migrated is not a state this store will run in.
//! 3. **A database newer than this build is refused, loudly.** §107's
//!    back-compat window is one minor version; opening a future schema and
//!    silently writing the old shape into it corrupts it slowly.

use std::path::{Path, PathBuf};

use marrow_core::{Code, Error, Result, Timestamp};
use rusqlite::{Connection, OptionalExtension};

use crate::schema;
use crate::Location;

/// One numbered, forward-only migration.
#[derive(Clone, Copy, Debug)]
pub struct Migration {
    pub version: i64,
    pub name: &'static str,
    /// SQL applied in a single transaction.
    pub up: &'static str,
}

/// The migration chain. Append only — never edit a shipped entry.
pub const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "m1_core_schema",
    up: schema::SCHEMA_V1,
}];

/// The schema version this build writes.
pub fn target_version() -> i64 {
    MIGRATIONS.last().map(|m| m.version).unwrap_or(0)
}

/// The schema version recorded in `schema_meta`, or 0 for an empty database.
pub fn current_version(conn: &Connection) -> Result<i64> {
    let has_meta: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='schema_meta'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| crate::map_sqlite(e, "Could not read the index database schema table."))?;
    if has_meta == 0 {
        return Ok(0);
    }
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM schema_meta WHERE key = 'schema_version'",
            [],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| crate::map_sqlite(e, "Could not read the index database schema version."))?;
    match raw {
        None => Ok(0),
        Some(s) => s.parse::<i64>().map_err(|_| {
            Error::new(
                Code::DbCorrupt,
                "The index database records an unreadable schema version. Delete the index \
                 directory to rebuild it from your files.",
            )
            .with_context(format!("schema_version = {s:?}"))
        }),
    }
}

/// Filename prefix for this database's backups.
fn backup_prefix(db: &Path) -> String {
    let stem = db
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "marrow.sqlite".to_string());
    format!("{stem}.backup-")
}

/// Every pre-migration backup that exists for `db`, oldest name first.
///
/// Nothing prunes these yet: an M1 database is a few megabytes and a lost
/// backup is unrecoverable, so keeping them all is the cheap side of the trade.
pub fn backups_for(db: &Path) -> Result<Vec<PathBuf>> {
    let dir = match db.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let prefix = backup_prefix(db);
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(Error::from(e).with_context(dir.display().to_string())),
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with(&prefix) {
            out.push(entry.path());
        }
    }
    out.sort();
    Ok(out)
}

/// `VACUUM INTO` a timestamped backup. **Invariant #11.**
fn take_backup(conn: &Connection, db: &Path, from_version: i64) -> Result<PathBuf> {
    let at = Timestamp::now().as_millis();
    let dir = db.parent().unwrap_or(Path::new("."));
    let target = dir.join(format!("{}{at}-v{from_version}.sqlite", backup_prefix(db)));
    let target_str = target.to_str().ok_or_else(|| {
        Error::new(
            Code::FsNotUtf8Path,
            "The index directory has a name this build cannot express as UTF-8, so no \
             pre-migration backup could be written. Move the index to an ASCII path.",
        )
        .with_context(target.display().to_string())
    })?;
    // VACUUM INTO refuses to overwrite, which is why the name carries a
    // millisecond timestamp and the version it is a backup *of*.
    conn.execute("VACUUM INTO ?1", [target_str]).map_err(|e| {
        crate::map_sqlite(
            e,
            "Could not write the pre-migration backup of the index database, so the migration \
             was not attempted. Free disk space in the index directory and retry.",
        )
        .with_context(target.display().to_string())
    })?;
    tracing::info!(backup = %target.display(), from_version, "pre-migration backup written");
    Ok(target)
}

/// Put `backup` back in place of `db`. The connection must already be closed.
fn restore_backup(backup: &Path, db: &Path) -> Result<()> {
    // The WAL and shared-memory files belong to the database we are replacing;
    // leaving them would graft the failed migration's tail onto the restore.
    for suffix in ["-wal", "-shm"] {
        let mut side = db.as_os_str().to_os_string();
        side.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(side));
    }
    std::fs::copy(backup, db).map_err(|e| {
        Error::from(e).with_context(format!("restore {} -> {}", backup.display(), db.display()))
    })?;
    tracing::warn!(backup = %backup.display(), db = %db.display(), "restored pre-migration backup");
    Ok(())
}

fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO schema_meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [key, value],
    )
    .map_err(|e| crate::map_sqlite(e, "Could not record the index database schema version."))?;
    Ok(())
}

/// Apply every migration newer than `from`. Each runs in its own transaction.
fn apply(conn: &mut Connection, from: i64, migrations: &[Migration]) -> Result<i64> {
    let mut at = from;
    for m in migrations.iter().filter(|m| m.version > from) {
        let span = tracing::info_span!("migration", version = m.version, name = m.name);
        let _e = span.enter();
        let tx = conn
            .transaction()
            .map_err(|e| crate::map_sqlite(e, "Could not begin the schema migration."))?;
        tx.execute_batch(m.up).map_err(|e| {
            crate::map_sqlite(
                e,
                "A schema migration failed and was rolled back; the index database was \
                 restored from its pre-migration backup and is not open for writes. Report \
                 this with the migration name below.",
            )
            .with_context(format!("migration {} ({})", m.version, m.name))
        })?;
        let now = Timestamp::now().as_millis().to_string();
        if m.version == 1 {
            set_meta(&tx, "created_at", &now)?;
            set_meta(&tx, "app_version_at_create", env!("CARGO_PKG_VERSION"))?;
        }
        set_meta(&tx, "schema_version", &m.version.to_string())?;
        set_meta(&tx, "migrated_at", &now)?;
        tx.commit().map_err(|e| {
            crate::map_sqlite(
                e,
                "Could not commit a schema migration to the index database.",
            )
        })?;
        tracing::info!("migration applied");
        at = m.version;
    }
    Ok(at)
}

/// Open the write connection, migrating it up to this build's schema version.
pub(crate) fn open_migrated(loc: &Location) -> Result<(Connection, i64)> {
    open_migrated_with(loc, MIGRATIONS)
}

/// As [`open_migrated`], with an injectable chain so the failure and
/// restore paths are testable without shipping a broken migration.
pub(crate) fn open_migrated_with(
    loc: &Location,
    migrations: &[Migration],
) -> Result<(Connection, i64)> {
    let mut conn = loc.open()?;
    schema::apply_writer_pragmas(&conn)?;

    let current = current_version(&conn)?;
    let target = migrations.last().map(|m| m.version).unwrap_or(0);

    if current > target {
        // §108 has no DB_SCHEMA_TOO_NEW. CFG_UNSUPPORTED_VERSION is the closest
        // existing code and says the right thing: this build does not support
        // that version. Adding a code to `marrow_core` is out of scope here.
        return Err(Error::new(
            Code::CfgUnsupportedVersion,
            "This index was written by a newer version of Marrow and will not be opened, to \
             avoid corrupting it. Upgrade Marrow, or point it at a different index directory.",
        )
        .with_context(format!(
            "index schema v{current}, this build writes v{target}"
        )));
    }
    if current == target {
        tracing::debug!(version = current, "schema up to date");
        return Ok((conn, current));
    }

    // Invariant #11: back up first, unconditionally — including the very first
    // migration. An unconditional rule is one that cannot be reasoned wrong at
    // 1am; an empty database costs a few kilobytes to copy.
    let backup = match loc.file_path() {
        Some(p) => Some((take_backup(&conn, p, current)?, p.to_path_buf())),
        // An in-memory database has nothing to protect and nowhere to put it.
        None => None,
    };

    match apply(&mut conn, current, migrations) {
        Ok(v) => Ok((conn, v)),
        Err(e) => {
            drop(conn); // release the file before overwriting it
            if let Some((backup, db)) = &backup {
                if let Err(restore_err) = restore_backup(backup, db) {
                    tracing::error!(error = %restore_err, "backup restore failed");
                    return Err(Error::new(
                        Code::DbMigrationFailed,
                        "A schema migration failed and the pre-migration backup could not be \
                         restored. The index is not open for writes. The backup file named \
                         below is intact — copy it over the index database by hand.",
                    )
                    .with_context(format!("backup at {}", backup.display())));
                }
            }
            Err(Error::new(
                Code::DbMigrationFailed,
                "A schema migration failed. The index database was restored from its \
                 pre-migration backup and is not open for writes. Re-run after upgrading; \
                 if it persists, delete the index directory to rebuild from your files.",
            )
            .with_context(e.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn migration_chain_matches_core_schema_version() {
        assert_eq!(
            target_version(),
            marrow_core::SCHEMA_VERSION,
            "MIGRATIONS and marrow_core::SCHEMA_VERSION must agree"
        );
    }

    #[test]
    fn migration_versions_are_dense_and_ascending() {
        for (i, m) in MIGRATIONS.iter().enumerate() {
            assert_eq!(m.version, i as i64 + 1, "migrations are numbered from 1");
        }
    }

    #[test]
    fn fresh_database_migrates_to_target() {
        let dir = tmp();
        let loc = Location::File(dir.path().join("marrow.sqlite"));
        let (conn, v) = open_migrated(&loc).unwrap();
        assert_eq!(v, target_version());
        assert_eq!(current_version(&conn).unwrap(), target_version());
        let created: String = conn
            .query_row(
                "SELECT value FROM schema_meta WHERE key='created_at'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(created.parse::<i64>().unwrap() > 1_700_000_000_000);
    }

    #[test]
    fn reopening_an_up_to_date_database_takes_no_backup() {
        let dir = tmp();
        let db = dir.path().join("marrow.sqlite");
        let loc = Location::File(db.clone());
        drop(open_migrated(&loc).unwrap());
        let after_first = backups_for(&db).unwrap().len();
        drop(open_migrated(&loc).unwrap());
        assert_eq!(
            backups_for(&db).unwrap().len(),
            after_first,
            "a no-op open must not churn backups"
        );
    }

    #[test]
    fn refuses_a_database_newer_than_this_build() {
        let dir = tmp();
        let db = dir.path().join("marrow.sqlite");
        let loc = Location::File(db.clone());
        {
            let (conn, _) = open_migrated(&loc).unwrap();
            set_meta(&conn, "schema_version", "9999").unwrap();
        }
        let err = open_migrated(&loc).unwrap_err();
        assert_eq!(err.code(), Code::CfgUnsupportedVersion);
        assert!(err.message().contains("newer version"));
    }

    #[test]
    fn restores_the_backup_when_a_migration_fails() {
        let dir = tmp();
        let db = dir.path().join("marrow.sqlite");
        let loc = Location::File(db.clone());
        {
            let (conn, _) = open_migrated(&loc).unwrap();
            conn.execute(
                "INSERT INTO devices (device_id, platform, first_seen_at, last_seen_at)
                 VALUES ('dev-1', 'macos', 1, 1)",
                [],
            )
            .unwrap();
        }

        // v2 commits a destructive change, v3 then fails. Only a real restore
        // brings `devices` back.
        let bad = [
            MIGRATIONS[0],
            Migration {
                version: 2,
                name: "drops_devices",
                up: "DROP TABLE devices;",
            },
            Migration {
                version: 3,
                name: "explodes",
                up: "CREATE TABLE oops (x INTEGER REFERENCES nowhere(y)); INSERT INTO oops VALUES (1);",
            },
        ];
        let err = open_migrated_with(&loc, &bad).unwrap_err();
        assert_eq!(err.code(), Code::DbMigrationFailed);

        let (conn, v) = open_migrated(&loc).unwrap();
        assert_eq!(
            v, 1,
            "restored database is back at the pre-migration version"
        );
        let n: i64 = conn
            .query_row("SELECT count(*) FROM devices", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "pre-migration row survived the restore");
    }

    #[test]
    fn in_memory_databases_migrate_without_a_backup() {
        let loc = Location::memory();
        let (conn, v) = open_migrated(&loc).unwrap();
        assert_eq!(v, target_version());
        assert_eq!(current_version(&conn).unwrap(), target_version());
    }
}
