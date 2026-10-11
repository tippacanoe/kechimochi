//! Tracked App Time — schema layer for automatic/manual study-app time tracking.
//!
//! This is a soft-fork feature (see `kechimochi-app-tracking-spec.md`). To keep
//! upstream rebases low-conflict, all DDL, the schema migration, and the schema
//! validation for the two new tables live here in an isolated module. The shared
//! `db.rs` only gains a handful of one-line call sites (the migration-registry
//! entry, `create_tables`, `create_indexes`, and the two schema validators).
//!
//! Two additive tables, both in the `main` (user) database:
//!
//! * `tracked_apps`  — one row per app the user tracks (identity/config record).
//! * `usage_entries` — one row per app, per day, per source/platform. The merged
//!   daily total is a derived sum of the displayed values, never a stored row, so
//!   it can never go stale relative to its inputs.
//!
//! Both tables carry a stable sync `uid` so they ride Kechimochi's existing
//! three-way sync as identified collections (keep-all-contributions for
//! `usage_entries`), matching how `activity_logs`/`milestones` already sync.

use rusqlite::{Connection, Result};

use crate::db::{table_exists, table_has_column};

/// Columns that must be present on `main.tracked_apps` in the latest schema.
pub(crate) const TRACKED_APPS_COLUMNS: &[&str] = &[
    "id",
    "uid",
    "display_name",
    "android_package_name",
    "icon",
    "auto_tracking_enabled",
    "manual_entry_enabled",
    "created_at",
    "archived",
];

/// Columns that must be present on `main.usage_entries` in the latest schema.
pub(crate) const USAGE_ENTRIES_COLUMNS: &[&str] = &[
    "id",
    "uid",
    "tracked_app_uid",
    "date",
    "minutes",
    "source",
    "platform",
    "manual_adjustment_minutes",
    "created_at",
    "updated_at",
    "device_id",
];

fn schema_error(message: impl Into<String>) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(message.into())))
}

/// Create `main.tracked_apps` if it does not already exist.
///
/// `android_package_name`, `icon`, and `device_id` (on usage entries) are
/// nullable: a desktop-first, manual-only tracked app has no Android package or
/// icon, and falls back to a placeholder in the UI. Booleans are stored as
/// `0`/`1` integers, matching the rest of the schema.
pub(crate) fn create_tracked_apps_table(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS main.tracked_apps (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            uid TEXT NOT NULL DEFAULT '',
            display_name TEXT NOT NULL,
            android_package_name TEXT,
            icon TEXT,
            auto_tracking_enabled INTEGER NOT NULL DEFAULT 0 CHECK (auto_tracking_enabled IN (0, 1)),
            manual_entry_enabled INTEGER NOT NULL DEFAULT 1 CHECK (manual_entry_enabled IN (0, 1)),
            created_at TEXT NOT NULL,
            archived INTEGER NOT NULL DEFAULT 0 CHECK (archived IN (0, 1))
        )",
        [],
    )?;
    Ok(())
}

/// Create `main.usage_entries` if it does not already exist.
///
/// One row per app/day/source/platform. `minutes` is the raw (e.g. automatically
/// collected) value; `manual_adjustment_minutes`, when non-null, overrides it for
/// display without destroying the raw value. The displayed value for an entry is
/// therefore `COALESCE(manual_adjustment_minutes, minutes)`.
///
/// `tracked_app_uid` links to `tracked_apps.uid` (the sync-stable identity rather
/// than the local autoincrement id), mirroring how `milestones.media_uid` links
/// to media. No SQL `FOREIGN KEY` is declared, matching the existing synced
/// tables (`activity_logs`, `milestones`), whose rows are reconciled by uid
/// during sync/migration rather than by referential constraints.
pub(crate) fn create_usage_entries_table(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS main.usage_entries (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            uid TEXT NOT NULL DEFAULT '',
            tracked_app_uid TEXT NOT NULL,
            date TEXT NOT NULL,
            minutes INTEGER NOT NULL,
            source TEXT NOT NULL CHECK (source IN ('android_auto', 'manual')),
            platform TEXT NOT NULL CHECK (platform IN ('android', 'desktop', 'web')),
            manual_adjustment_minutes INTEGER,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            device_id TEXT
        )",
        [],
    )?;
    Ok(())
}

/// Create both tracked-app-time tables. Called from `db::create_tables`, so fresh
/// installs and the legacy pre-release upgrader both get them.
pub(crate) fn create_tracked_app_tables(conn: &Connection) -> Result<()> {
    create_tracked_apps_table(conn)?;
    create_usage_entries_table(conn)?;
    Ok(())
}

/// Create the indexes for the tracked-app-time tables. Idempotent
/// (`IF NOT EXISTS`); called from `db::create_indexes`, which runs at the end of
/// every migration path.
pub(crate) fn create_tracked_app_indexes(conn: &Connection) -> Result<()> {
    // Stable sync identity is unique per record, like activity_logs/milestones.
    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS main.idx_tracked_apps_uid
         ON tracked_apps(uid)",
        [],
    )?;
    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS main.idx_usage_entries_uid
         ON usage_entries(uid)",
        [],
    )?;
    // Primary read path: a given app's entries for a given day (drill-down and
    // merged daily total).
    conn.execute(
        "CREATE INDEX IF NOT EXISTS main.idx_usage_entries_app_date
         ON usage_entries(tracked_app_uid, date)",
        [],
    )?;
    // Lookup by Android package when matching a picked app to an existing row.
    conn.execute(
        "CREATE INDEX IF NOT EXISTS main.idx_tracked_apps_package
         ON tracked_apps(android_package_name)",
        [],
    )?;
    Ok(())
}

/// Schema migration `v8 -> v9`: add the tracked-app-time tables and indexes.
///
/// Additive only — it creates two new tables and never alters an existing table,
/// so no historical row changes meaning. Registered in
/// `db::VERSIONED_MIGRATIONS`.
pub(crate) fn migrate_v8_to_v9_add_tracked_app_time(conn: &Connection) -> Result<()> {
    create_tracked_app_tables(conn)?;
    create_tracked_app_indexes(conn)?;
    Ok(())
}

fn table_has_all_columns(conn: &Connection, table: &str, columns: &[&str]) -> Result<bool> {
    if !table_exists(conn, "main", table)? {
        return Ok(false);
    }
    for column in columns {
        if !table_has_column(conn, "main", table, column)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Whether both tracked-app-time tables are present with all their latest
/// columns. Folded into `db::latest_schema_is_present`.
pub(crate) fn tracked_app_tables_present(conn: &Connection) -> Result<bool> {
    Ok(table_has_all_columns(conn, "tracked_apps", TRACKED_APPS_COLUMNS)?
        && table_has_all_columns(conn, "usage_entries", USAGE_ENTRIES_COLUMNS)?)
}

/// Assert both tracked-app-time tables exist with all their latest columns,
/// erroring loudly otherwise. Folded into `db::validate_latest_schema`.
pub(crate) fn validate_tracked_app_tables(conn: &Connection) -> Result<()> {
    for (table, columns) in [
        ("tracked_apps", TRACKED_APPS_COLUMNS),
        ("usage_entries", USAGE_ENTRIES_COLUMNS),
    ] {
        if !table_exists(conn, "main", table)? {
            return Err(schema_error(format!("Missing required table main.{table}")));
        }
        for column in columns {
            if !table_has_column(conn, "main", table, column)? {
                return Err(schema_error(format!(
                    "Missing required column main.{table}.{column}"
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bare in-memory bundle: a `main` database with `shared` attached, and no
    /// tables yet. Enough to exercise the tracked-app-time DDL in isolation; the
    /// full `migrate_schema` v8 -> v9 chain is covered in `db.rs`'s test module,
    /// where the private bundle helpers live.
    fn in_memory_bundle() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("ATTACH DATABASE ':memory:' AS shared", [])
            .unwrap();
        conn
    }

    #[test]
    fn fresh_table_creation_has_all_latest_columns() {
        let conn = in_memory_bundle();
        create_tracked_app_tables(&conn).unwrap();
        create_tracked_app_indexes(&conn).unwrap();

        assert!(tracked_app_tables_present(&conn).unwrap());
        validate_tracked_app_tables(&conn).unwrap();
    }

    #[test]
    fn migration_is_additive_and_idempotent() {
        let conn = in_memory_bundle();
        assert!(!tracked_app_tables_present(&conn).unwrap());
        assert!(validate_tracked_app_tables(&conn).is_err());

        migrate_v8_to_v9_add_tracked_app_time(&conn).unwrap();
        assert!(tracked_app_tables_present(&conn).unwrap());

        // Running it again must not fail (IF NOT EXISTS throughout).
        migrate_v8_to_v9_add_tracked_app_time(&conn).unwrap();
        assert!(tracked_app_tables_present(&conn).unwrap());
    }

    #[test]
    fn check_constraints_reject_bad_source_and_platform() {
        let conn = in_memory_bundle();
        create_tracked_app_tables(&conn).unwrap();

        let bad_source = conn.execute(
            "INSERT INTO main.usage_entries
                 (uid, tracked_app_uid, date, minutes, source, platform, created_at, updated_at)
             VALUES ('u1', 'app1', '2026-10-11', 30, 'bogus', 'android', 't', 't')",
            [],
        );
        assert!(bad_source.is_err());

        let bad_platform = conn.execute(
            "INSERT INTO main.usage_entries
                 (uid, tracked_app_uid, date, minutes, source, platform, created_at, updated_at)
             VALUES ('u1', 'app1', '2026-10-11', 30, 'manual', 'bogus', 't', 't')",
            [],
        );
        assert!(bad_platform.is_err());
    }

    #[test]
    fn manual_adjustment_overrides_raw_minutes_for_display() {
        let conn = in_memory_bundle();
        create_tracked_app_tables(&conn).unwrap();

        conn.execute(
            "INSERT INTO main.usage_entries
                 (uid, tracked_app_uid, date, minutes, source, platform,
                  manual_adjustment_minutes, created_at, updated_at)
             VALUES
                 ('u1', 'app1', '2026-10-11', 90, 'android_auto', 'android', 15, 't', 't'),
                 ('u2', 'app1', '2026-10-11', 30, 'manual', 'desktop', NULL, 't', 't')",
            [],
        )
        .unwrap();

        // Merged daily total is the sum of displayed values, not a stored row.
        let displayed_total: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(COALESCE(manual_adjustment_minutes, minutes)), 0)
                 FROM main.usage_entries
                 WHERE tracked_app_uid = 'app1' AND date = '2026-10-11'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(displayed_total, 45);
    }
}
