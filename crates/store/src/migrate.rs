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
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "m1_core_schema",
        up: schema::SCHEMA_V1,
    },
    // **Version 3, not 2.** The numbered chain is global across crates: the
    // text index inserts itself at 2 (`marrow_index::fts5::MIGRATION`), and a
    // database in the field already has it there. Taking 2 here would mean
    // re-running a migration that has already been applied, on every existing
    // index, with the table already present.
    Migration {
        version: 3,
        name: "m3_self_written",
        up: schema::SCHEMA_V3,
    },
    // **Five, not four.** `marrow-index` took 4 for the vector table, by the
    // same rule that gave it 2. The composed chain is what a real database is
    // at, and `Store::compose` refuses a chain with a hole in it — so the next
    // free number is the one after the highest either crate has claimed, not
    // the one after this list's own last entry.
    Migration {
        version: 5,
        name: "m5_conversations",
        up: schema::SCHEMA_V5,
    },
    // Six is simply the next number: the composed chain reached five with this
    // crate's own migration 5, so for once there is no extension in the way.
    // The rule is unchanged — take the number after the highest either crate
    // has claimed, not the one after this list's own last entry — and
    // `marrow_index::SCHEMA_VERSION` moves with it, because the version a
    // binary declares must be the version it writes (D57).
    Migration {
        version: 6,
        name: "m6_table_ir",
        up: schema::SCHEMA_V6,
    },
    // Seven, by the same rule: neither crate has claimed it.
    Migration {
        version: 7,
        name: "m7_chunk_source_span",
        up: schema::SCHEMA_V7,
    },
];

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

/// How many pre-migration backups to keep.
///
/// The comment this replaces said "nothing prunes these yet: an M1 database is
/// a few megabytes and a lost backup is unrecoverable, so keeping them all is
/// the cheap side of the trade." That was true when it was written and stopped
/// being true without anyone noticing: on a real corpus the database reached
/// 4.3 GB and its four kept backups came to 4.2 GB, which is most of what
/// filled a disk — and a full disk stops SQLite writing at all, so the
/// mechanism protecting the index was the thing taking it down.
///
/// Two, not one. One backup means the *current* migration is recoverable and
/// nothing before it, and a schema fault is often noticed a migration late.
/// Two costs one more copy and covers the case where the last upgrade was the
/// one that did the damage.
pub const KEEP_BACKUPS: usize = 2;

/// Every pre-migration backup that exists for `db`, oldest name first.
///
/// Name order is time order: the filename carries an epoch-millisecond stamp,
/// which is why pruning can trust a lexical sort rather than asking the
/// filesystem for mtimes it may not have preserved through a copy.
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

/// `VACUUM INTO` a timestamped backup. **Back up before any migration.**
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
/// Open a database, applying `migrations` in order.
///
/// The composition root reaches this through [`crate::Store::open_with_migrations`],
/// which is the public seam; this stays crate-private so `Location` does not
/// leak. Also how the failure and restore paths are tested without shipping a
/// broken migration.
/// The default chain, for tests that do not care about composition.
#[cfg(test)]
fn open_migrated(loc: &Location) -> Result<(Connection, i64)> {
    open_migrated_with(loc, MIGRATIONS)
}

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

    // Back up before any migration: unconditionally first, unconditionally — including the very first
    // migration. An unconditional rule is one that cannot be reasoned wrong at
    // 1am; an empty database costs a few kilobytes to copy.
    let backup = match loc.file_path() {
        Some(p) => Some((take_backup(&conn, p, current)?, p.to_path_buf())),
        // An in-memory database has nothing to protect and nowhere to put it.
        None => None,
    };

    match apply(&mut conn, current, migrations) {
        Ok(v) => {
            // **After success, never before.** The whole point of the backup is
            // the window between "about to change the schema" and "the changed
            // schema is known good"; pruning inside that window would delete
            // the thing being relied on. By here the migration has applied and
            // the backup just taken is the newest, so it is one of the ones
            // kept.
            if let Some((_, db)) = &backup {
                prune_backups(db);
            }
            Ok((conn, v))
        }
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

/// Delete all but the newest [`KEEP_BACKUPS`] backups of `db`.
///
/// **Never fails the migration.** A backup that cannot be listed or removed is
/// wasted disk, and wasted disk is not a reason to refuse an index that has
/// just migrated successfully. Every failure here is logged and swallowed.
fn prune_backups(db: &Path) {
    let all = match backups_for(db) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!(error = %e, "could not list old backups to prune");
            return;
        }
    };
    let Some(surplus) = all.len().checked_sub(KEEP_BACKUPS) else {
        return;
    };
    for old in all.iter().take(surplus) {
        match std::fs::remove_file(old) {
            Ok(()) => tracing::info!(backup = %old.display(), "pruned an old pre-migration backup"),
            Err(e) => {
                tracing::warn!(error = %e, backup = %old.display(), "could not prune a backup")
            }
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
    fn migration_versions_ascend_and_never_repeat() {
        // Ascending and unique, not *dense*. The chain is numbered across
        // crates — `marrow_index::fts5::MIGRATION` holds 2 and is composed in
        // at open time — so this crate's own list legitimately has holes in it.
        // `marrow-index` asserts the composed chain is contiguous, which is
        // the property that actually matters.
        let mut last = 0;
        for m in MIGRATIONS {
            assert!(
                m.version > last,
                "migration {} ({}) does not come after {last}",
                m.version,
                m.name
            );
            last = m.version;
        }
        assert_eq!(MIGRATIONS[0].version, 1, "the chain starts at 1");
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
    fn backups_are_pruned_to_a_bound_but_never_below_it() {
        // The comment that used to sit on `backups_for` said keeping every
        // backup was "the cheap side of the trade", and it was — for a database
        // of a few megabytes. On a real corpus the index reached 4.3 GB and its
        // four kept backups came to 4.2 GB, which is most of what filled a
        // disk. A full disk stops SQLite writing, so the mechanism that exists
        // to protect the index was the thing taking it down.
        let dir = tmp();
        let db = dir.path().join("marrow.sqlite");

        // Older backups than any migration would leave behind, named so that
        // the lexical sort the pruner relies on puts them oldest-first.
        for stamp in ["1000000000000-v0", "1000000000001-v1", "1000000000002-v2"] {
            std::fs::write(
                dir.path()
                    .join(format!("marrow.sqlite.backup-{stamp}.sqlite")),
                b"old",
            )
            .unwrap();
        }
        assert_eq!(backups_for(&db).unwrap().len(), 3);

        // A real migration: takes one more, then prunes.
        drop(open_migrated(&Location::File(db.clone())).unwrap());

        let left = backups_for(&db).unwrap();
        assert_eq!(
            left.len(),
            KEEP_BACKUPS,
            "pruning did not bound the backups: {left:?}"
        );
        // And it kept the newest, not whichever the filesystem listed first.
        // The one this migration just took is the newest of all, so it must
        // survive its own pruning.
        assert!(
            left.iter()
                .all(|p| !p.to_string_lossy().contains("1000000000000")),
            "the oldest backup outlived a newer one: {left:?}"
        );
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
        let before = {
            let (conn, v) = open_migrated(&loc).unwrap();
            conn.execute(
                "INSERT INTO devices (device_id, platform, first_seen_at, last_seen_at)
                 VALUES ('dev-1', 'macos', 1, 1)",
                [],
            )
            .unwrap();
            v
        };

        // The shipped chain, then a destructive step that commits, then one
        // that fails. Only a real restore brings `devices` back.
        //
        // Built on top of `MIGRATIONS` rather than replacing it, so appending
        // a real migration cannot silently turn this into a different test.
        let mut chain: Vec<Migration> = MIGRATIONS.to_vec();
        chain.push(Migration {
            version: before + 1,
            name: "drops_devices",
            up: "DROP TABLE devices;",
        });
        chain.push(Migration {
            version: before + 2,
            name: "explodes",
            up: "CREATE TABLE oops (x INTEGER REFERENCES nowhere(y)); INSERT INTO oops VALUES (1);",
        });
        let bad = chain;
        let err = open_migrated_with(&loc, &bad).unwrap_err();
        assert_eq!(err.code(), Code::DbMigrationFailed);

        let (conn, v) = open_migrated(&loc).unwrap();
        assert_eq!(
            v, before,
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
