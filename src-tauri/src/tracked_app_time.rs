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

use chrono::{NaiveDate, SecondsFormat, Utc};
use rusqlite::{params, Connection, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::{table_exists, table_has_column};

/// Valid `usage_entries.source` values (also enforced by a SQL CHECK).
pub const SOURCE_ANDROID_AUTO: &str = "android_auto";
pub const SOURCE_MANUAL: &str = "manual";
const VALID_SOURCES: &[&str] = &[SOURCE_ANDROID_AUTO, SOURCE_MANUAL];

/// Valid `usage_entries.platform` values (also enforced by a SQL CHECK).
pub const PLATFORM_ANDROID: &str = "android";
pub const PLATFORM_DESKTOP: &str = "desktop";
pub const PLATFORM_WEB: &str = "web";
const VALID_PLATFORMS: &[&str] = &[PLATFORM_ANDROID, PLATFORM_DESKTOP, PLATFORM_WEB];

/// Upper bound on a single usage entry's minutes, mirroring the activity-log cap
/// (`MAX_ACTIVITY_DURATION_MINUTES` in `db.rs`): ~10 years. Guards against
/// overflow and obviously-bad input without constraining real use.
const MAX_USAGE_MINUTES: i64 = 5_256_000;

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

/// Wrap a message as a `rusqlite::Error` so validation failures propagate like
/// the rest of the data layer (`db.rs` uses the same pattern) and convert to a
/// `String` at the Tauri command boundary.
fn err(message: impl Into<String>) -> rusqlite::Error {
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
            return Err(err(format!("Missing required table main.{table}")));
        }
        for column in columns {
            if !table_has_column(conn, "main", table, column)? {
                return Err(err(format!(
                    "Missing required column main.{table}.{column}"
                )));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

/// An app the user has chosen to track. One row per app, not per day.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackedApp {
    /// Local autoincrement id. `None` when creating a new row.
    pub id: Option<i64>,
    /// Stable sync identity. Assigned on insert when absent; immutable after.
    #[serde(default)]
    pub uid: Option<String>,
    /// User-facing name, editable. Defaults to the app's label at add-time.
    pub display_name: String,
    /// e.g. `com.ichi2.anki`. `None` for manual-only (desktop-first) apps.
    #[serde(default)]
    pub android_package_name: Option<String>,
    /// Cached icon (base64/data URI) from Android `PackageManager`. `None` on
    /// desktop; the UI falls back to a placeholder.
    #[serde(default)]
    pub icon: Option<String>,
    /// Whether Android automatic collection is active for this app.
    #[serde(default)]
    pub auto_tracking_enabled: bool,
    /// Whether this app appears in the manual-entry form. Defaults to true so a
    /// PC entry can be logged even for an auto-tracked app.
    #[serde(default = "default_true")]
    pub manual_entry_enabled: bool,
    /// RFC3339 creation timestamp. Set on insert when absent.
    #[serde(default)]
    pub created_at: Option<String>,
    /// Soft-delete flag; rows are archived, never hard-deleted, so historical
    /// usage entries keep a valid link.
    #[serde(default)]
    pub archived: bool,
}

fn default_true() -> bool {
    true
}

/// One usage contribution for an app on a day from one source/platform. The raw
/// value is `minutes`; `manual_adjustment_minutes`, when set, overrides it for
/// display without discarding the raw value (see spec §3.5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageEntry {
    pub id: Option<i64>,
    #[serde(default)]
    pub uid: Option<String>,
    /// Links to `tracked_apps.uid` (sync-stable identity, not the local id).
    pub tracked_app_uid: String,
    /// Local calendar day, `YYYY-MM-DD` (no timezone), matching activity logs.
    pub date: String,
    pub minutes: i64,
    pub source: String,
    pub platform: String,
    #[serde(default)]
    pub manual_adjustment_minutes: Option<i64>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    /// Originating device, for debugging sync/merge. Not shown in the UI.
    #[serde(default)]
    pub device_id: Option<String>,
}

/// A per-app merged daily total: the sum of displayed values across every
/// contributing `UsageEntry` for one app on one day. Derived, never stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppDailyTotal {
    pub tracked_app_uid: String,
    pub display_name: String,
    pub date: String,
    pub total_minutes: i64,
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Trim and collapse an optional string to `None` when blank.
fn normalize_opt(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|trimmed| !trimmed.is_empty())
        .map(ToString::to_string)
}

fn validate_local_day(date: &str) -> Result<()> {
    NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d")
        .map(|_| ())
        .map_err(|_| err(format!("Usage entry date '{date}' must be YYYY-MM-DD")))
}

fn tracked_app_exists(conn: &Connection, uid: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM main.tracked_apps WHERE uid = ?1",
        params![uid],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

const TRACKED_APP_SELECT: &str = "SELECT id, uid, display_name, android_package_name, icon, \
     auto_tracking_enabled, manual_entry_enabled, created_at, archived \
     FROM main.tracked_apps";

fn map_tracked_app(row: &rusqlite::Row) -> rusqlite::Result<TrackedApp> {
    Ok(TrackedApp {
        id: Some(row.get(0)?),
        uid: Some(row.get(1)?),
        display_name: row.get(2)?,
        android_package_name: row.get(3)?,
        icon: row.get(4)?,
        auto_tracking_enabled: row.get::<_, i64>(5)? != 0,
        manual_entry_enabled: row.get::<_, i64>(6)? != 0,
        created_at: Some(row.get(7)?),
        archived: row.get::<_, i64>(8)? != 0,
    })
}

const USAGE_ENTRY_SELECT: &str = "SELECT id, uid, tracked_app_uid, date, minutes, source, platform, \
     manual_adjustment_minutes, created_at, updated_at, device_id \
     FROM main.usage_entries";

fn map_usage_entry(row: &rusqlite::Row) -> rusqlite::Result<UsageEntry> {
    Ok(UsageEntry {
        id: Some(row.get(0)?),
        uid: Some(row.get(1)?),
        tracked_app_uid: row.get(2)?,
        date: row.get(3)?,
        minutes: row.get(4)?,
        source: row.get(5)?,
        platform: row.get(6)?,
        manual_adjustment_minutes: row.get(7)?,
        created_at: Some(row.get(8)?),
        updated_at: Some(row.get(9)?),
        device_id: row.get(10)?,
    })
}

// ---------------------------------------------------------------------------
// Tracked apps
// ---------------------------------------------------------------------------

/// Insert a tracked app, assigning a fresh sync uid. Returns the new row id.
pub fn add_tracked_app(conn: &Connection, app: &TrackedApp) -> Result<i64> {
    add_tracked_app_with_uid(conn, app, &Uuid::new_v4().to_string())
}

pub(crate) fn add_tracked_app_with_uid(
    conn: &Connection,
    app: &TrackedApp,
    uid: &str,
) -> Result<i64> {
    let display_name = app.display_name.trim();
    if display_name.is_empty() {
        return Err(err("Tracked app display name cannot be blank"));
    }
    if uid.trim().is_empty() {
        return Err(err("Tracked app sync UID cannot be blank"));
    }
    let created_at = app.created_at.clone().unwrap_or_else(now_rfc3339);
    conn.execute(
        "INSERT INTO main.tracked_apps
             (uid, display_name, android_package_name, icon,
              auto_tracking_enabled, manual_entry_enabled, created_at, archived)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            uid,
            display_name,
            normalize_opt(app.android_package_name.as_deref()),
            normalize_opt(app.icon.as_deref()),
            app.auto_tracking_enabled as i64,
            app.manual_entry_enabled as i64,
            created_at,
            app.archived as i64,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Update a tracked app's editable fields. The sync uid and created_at are
/// immutable; `archived` is toggled here too (soft delete/restore).
pub fn update_tracked_app(conn: &Connection, app: &TrackedApp) -> Result<()> {
    let id = app
        .id
        .ok_or_else(|| err("Tracked app update requires an id"))?;
    let display_name = app.display_name.trim();
    if display_name.is_empty() {
        return Err(err("Tracked app display name cannot be blank"));
    }
    let changed = conn.execute(
        "UPDATE main.tracked_apps
         SET display_name = ?1, android_package_name = ?2, icon = ?3,
             auto_tracking_enabled = ?4, manual_entry_enabled = ?5, archived = ?6
         WHERE id = ?7",
        params![
            display_name,
            normalize_opt(app.android_package_name.as_deref()),
            normalize_opt(app.icon.as_deref()),
            app.auto_tracking_enabled as i64,
            app.manual_entry_enabled as i64,
            app.archived as i64,
            id,
        ],
    )?;
    if changed == 0 {
        Err(err(format!("Tracked app {id} not found")))
    } else {
        Ok(())
    }
}

/// Archive or restore a tracked app (soft delete). Historical usage entries are
/// kept, so the link stays valid.
pub fn set_tracked_app_archived(conn: &Connection, id: i64, archived: bool) -> Result<()> {
    let changed = conn.execute(
        "UPDATE main.tracked_apps SET archived = ?1 WHERE id = ?2",
        params![archived as i64, id],
    )?;
    if changed == 0 {
        Err(err(format!("Tracked app {id} not found")))
    } else {
        Ok(())
    }
}

/// List tracked apps, ordered by display name. Archived rows are included only
/// when requested.
pub fn get_tracked_apps(conn: &Connection, include_archived: bool) -> Result<Vec<TrackedApp>> {
    let filter = if include_archived {
        ""
    } else {
        " WHERE archived = 0"
    };
    let sql = format!("{TRACKED_APP_SELECT}{filter} ORDER BY display_name COLLATE NOCASE ASC, id ASC");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], map_tracked_app)?;
    rows.collect()
}

// ---------------------------------------------------------------------------
// Usage entries
// ---------------------------------------------------------------------------

fn validate_usage_entry(conn: &Connection, entry: &UsageEntry) -> Result<()> {
    let app_uid = entry.tracked_app_uid.trim();
    if app_uid.is_empty() {
        return Err(err("Usage entry requires a tracked app"));
    }
    if !tracked_app_exists(conn, app_uid)? {
        return Err(err(format!(
            "Usage entry references unknown tracked app '{app_uid}'"
        )));
    }
    validate_local_day(&entry.date)?;
    if !(0..=MAX_USAGE_MINUTES).contains(&entry.minutes) {
        return Err(err(format!(
            "Usage entry minutes must be between 0 and {MAX_USAGE_MINUTES}"
        )));
    }
    if let Some(adjustment) = entry.manual_adjustment_minutes {
        if !(0..=MAX_USAGE_MINUTES).contains(&adjustment) {
            return Err(err(format!(
                "Usage entry adjustment must be between 0 and {MAX_USAGE_MINUTES}"
            )));
        }
    }
    if !VALID_SOURCES.contains(&entry.source.as_str()) {
        return Err(err(format!("Unknown usage source '{}'", entry.source)));
    }
    if !VALID_PLATFORMS.contains(&entry.platform.as_str()) {
        return Err(err(format!("Unknown usage platform '{}'", entry.platform)));
    }
    Ok(())
}

/// Insert a usage entry, assigning a fresh sync uid. Returns the new row id.
///
/// This is the manual-entry and (later) automatic-collection write path. Each
/// call is a distinct contribution; entries are never merged into a single
/// overwritten row — the displayed daily total is summed on read.
pub fn add_usage_entry(conn: &Connection, entry: &UsageEntry) -> Result<i64> {
    add_usage_entry_with_uid(conn, entry, &Uuid::new_v4().to_string())
}

pub(crate) fn add_usage_entry_with_uid(
    conn: &Connection,
    entry: &UsageEntry,
    uid: &str,
) -> Result<i64> {
    validate_usage_entry(conn, entry)?;
    if uid.trim().is_empty() {
        return Err(err("Usage entry sync UID cannot be blank"));
    }
    let now = now_rfc3339();
    let created_at = entry.created_at.clone().unwrap_or_else(|| now.clone());
    let updated_at = entry.updated_at.clone().unwrap_or(now);
    conn.execute(
        "INSERT INTO main.usage_entries
             (uid, tracked_app_uid, date, minutes, source, platform,
              manual_adjustment_minutes, created_at, updated_at, device_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            uid,
            entry.tracked_app_uid.trim(),
            entry.date.trim(),
            entry.minutes,
            entry.source,
            entry.platform,
            entry.manual_adjustment_minutes,
            created_at,
            updated_at,
            normalize_opt(entry.device_id.as_deref()),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Set or clear the manual adjustment on a single entry (spec §3.5), leaving the
/// raw `minutes` untouched. `None` clears the adjustment.
pub fn set_usage_entry_adjustment(
    conn: &Connection,
    id: i64,
    adjustment: Option<i64>,
) -> Result<()> {
    if let Some(value) = adjustment {
        if !(0..=MAX_USAGE_MINUTES).contains(&value) {
            return Err(err(format!(
                "Usage entry adjustment must be between 0 and {MAX_USAGE_MINUTES}"
            )));
        }
    }
    let changed = conn.execute(
        "UPDATE main.usage_entries SET manual_adjustment_minutes = ?1, updated_at = ?2 WHERE id = ?3",
        params![adjustment, now_rfc3339(), id],
    )?;
    if changed == 0 {
        Err(err(format!("Usage entry {id} not found")))
    } else {
        Ok(())
    }
}

/// Delete a single usage entry.
pub fn delete_usage_entry(conn: &Connection, id: i64) -> Result<()> {
    let changed = conn.execute("DELETE FROM main.usage_entries WHERE id = ?1", params![id])?;
    if changed == 0 {
        Err(err(format!("Usage entry {id} not found")))
    } else {
        Ok(())
    }
}

/// Drill-down: every contributing entry for one app on one day, ordered by
/// source then platform for a stable display.
pub fn get_usage_entries_for_day(
    conn: &Connection,
    tracked_app_uid: &str,
    date: &str,
) -> Result<Vec<UsageEntry>> {
    let sql = format!(
        "{USAGE_ENTRY_SELECT} WHERE tracked_app_uid = ?1 AND date = ?2 \
         ORDER BY source ASC, platform ASC, id ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![tracked_app_uid, date], map_usage_entry)?;
    rows.collect()
}

/// Per-app merged daily totals for one day: the sum of displayed values
/// (`COALESCE(manual_adjustment_minutes, minutes)`) across all sources and
/// platforms. Non-archived apps are listed even when zero, so the dashboard can
/// show them; ordered by total descending then name.
pub fn get_tracked_app_daily_totals(conn: &Connection, date: &str) -> Result<Vec<AppDailyTotal>> {
    let mut stmt = conn.prepare(
        "SELECT a.uid, a.display_name,
                COALESCE(SUM(COALESCE(u.manual_adjustment_minutes, u.minutes)), 0) AS total
         FROM main.tracked_apps a
         LEFT JOIN main.usage_entries u
             ON u.tracked_app_uid = a.uid AND u.date = ?1
         WHERE a.archived = 0
         GROUP BY a.uid, a.display_name
         ORDER BY total DESC, a.display_name COLLATE NOCASE ASC",
    )?;
    let rows = stmt.query_map(params![date], |row| {
        Ok(AppDailyTotal {
            tracked_app_uid: row.get(0)?,
            display_name: row.get(1)?,
            date: date.to_string(),
            total_minutes: row.get(2)?,
        })
    })?;
    rows.collect()
}

/// Total tracked-app minutes across every app on one day — the input to the
/// combined "Japanese time today" metric (spec §5), summed from displayed
/// values. Archived apps still count: their past time was really spent.
pub fn get_tracked_app_total_minutes(conn: &Connection, date: &str) -> Result<i64> {
    conn.query_row(
        "SELECT COALESCE(SUM(COALESCE(manual_adjustment_minutes, minutes)), 0)
         FROM main.usage_entries WHERE date = ?1",
        params![date],
        |row| row.get(0),
    )
}

// ---------------------------------------------------------------------------
// Sync support (used by sync_snapshot build/apply)
// ---------------------------------------------------------------------------

/// Every usage entry across all apps and days, ordered deterministically. Used
/// when building a sync snapshot.
pub(crate) fn all_usage_entries(conn: &Connection) -> Result<Vec<UsageEntry>> {
    let sql = format!(
        "{USAGE_ENTRY_SELECT} \
         ORDER BY tracked_app_uid ASC, date ASC, source ASC, platform ASC, uid ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], map_usage_entry)?;
    rows.collect()
}

/// Delete all tracked-app-time rows, in preparation for re-applying a synced
/// snapshot. Usage entries are cleared before tracked apps so the logical
/// parent is removed last.
pub(crate) fn clear_for_sync_apply(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM main.usage_entries", [])?;
    conn.execute("DELETE FROM main.tracked_apps", [])?;
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

    // --- Data-access layer (Phase 3: manual-entry path) ---------------------

    fn ready_bundle() -> Connection {
        let conn = in_memory_bundle();
        create_tracked_app_tables(&conn).unwrap();
        create_tracked_app_indexes(&conn).unwrap();
        conn
    }

    fn sample_app(name: &str) -> TrackedApp {
        TrackedApp {
            id: None,
            uid: None,
            display_name: name.to_string(),
            android_package_name: None,
            icon: None,
            auto_tracking_enabled: false,
            manual_entry_enabled: true,
            created_at: None,
            archived: false,
        }
    }

    fn manual_entry(app_uid: &str, date: &str, minutes: i64) -> UsageEntry {
        UsageEntry {
            id: None,
            uid: None,
            tracked_app_uid: app_uid.to_string(),
            date: date.to_string(),
            minutes,
            source: SOURCE_MANUAL.to_string(),
            platform: PLATFORM_DESKTOP.to_string(),
            manual_adjustment_minutes: None,
            created_at: None,
            updated_at: None,
            device_id: None,
        }
    }

    fn uid_of(conn: &Connection, id: i64) -> String {
        conn.query_row(
            "SELECT uid FROM main.tracked_apps WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn add_and_list_tracked_apps_respects_archive_filter() {
        let conn = ready_bundle();
        let anki_id = add_tracked_app(&conn, &sample_app("AnkiDroid")).unwrap();
        let old_id = add_tracked_app(&conn, &sample_app("Old App")).unwrap();
        set_tracked_app_archived(&conn, old_id, true).unwrap();

        let active = get_tracked_apps(&conn, false).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].display_name, "AnkiDroid");
        assert_eq!(active[0].id, Some(anki_id));
        assert!(active[0].uid.as_deref().is_some_and(|u| !u.is_empty()));
        assert!(active[0].manual_entry_enabled);

        let all = get_tracked_apps(&conn, true).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn blank_display_name_is_rejected() {
        let conn = ready_bundle();
        assert!(add_tracked_app(&conn, &sample_app("   ")).is_err());
    }

    #[test]
    fn manual_and_auto_entries_sum_for_the_same_app_day() {
        let conn = ready_bundle();
        let app_id = add_tracked_app(&conn, &sample_app("Renshuu")).unwrap();
        let app_uid = uid_of(&conn, app_id);

        add_usage_entry(&conn, &manual_entry(&app_uid, "2026-10-11", 30)).unwrap();
        let mut auto = manual_entry(&app_uid, "2026-10-11", 90);
        auto.source = SOURCE_ANDROID_AUTO.to_string();
        auto.platform = PLATFORM_ANDROID.to_string();
        add_usage_entry(&conn, &auto).unwrap();

        let totals = get_tracked_app_daily_totals(&conn, "2026-10-11").unwrap();
        assert_eq!(totals.len(), 1);
        assert_eq!(totals[0].total_minutes, 120);
        assert_eq!(
            get_tracked_app_total_minutes(&conn, "2026-10-11").unwrap(),
            120
        );

        // Both contributions are kept as distinct rows (drill-down), never merged.
        let drill = get_usage_entries_for_day(&conn, &app_uid, "2026-10-11").unwrap();
        assert_eq!(drill.len(), 2);
    }

    #[test]
    fn adjustment_overrides_only_the_adjusted_entry() {
        let conn = ready_bundle();
        let app_id = add_tracked_app(&conn, &sample_app("AnkiDroid")).unwrap();
        let app_uid = uid_of(&conn, app_id);

        let mut auto = manual_entry(&app_uid, "2026-10-11", 90);
        auto.source = SOURCE_ANDROID_AUTO.to_string();
        auto.platform = PLATFORM_ANDROID.to_string();
        let auto_id = add_usage_entry(&conn, &auto).unwrap();
        add_usage_entry(&conn, &manual_entry(&app_uid, "2026-10-11", 30)).unwrap();

        // App was open in the background; correct the auto entry down to 15.
        set_usage_entry_adjustment(&conn, auto_id, Some(15)).unwrap();
        assert_eq!(
            get_tracked_app_total_minutes(&conn, "2026-10-11").unwrap(),
            45
        );

        // Raw value is preserved; clearing the adjustment restores the original.
        let adjusted = get_usage_entries_for_day(&conn, &app_uid, "2026-10-11")
            .unwrap()
            .into_iter()
            .find(|entry| entry.id == Some(auto_id))
            .unwrap();
        assert_eq!(adjusted.minutes, 90);
        assert_eq!(adjusted.manual_adjustment_minutes, Some(15));

        set_usage_entry_adjustment(&conn, auto_id, None).unwrap();
        assert_eq!(
            get_tracked_app_total_minutes(&conn, "2026-10-11").unwrap(),
            120
        );
    }

    #[test]
    fn usage_entry_validation_rejects_bad_input() {
        let conn = ready_bundle();
        let app_id = add_tracked_app(&conn, &sample_app("AnkiDroid")).unwrap();
        let app_uid = uid_of(&conn, app_id);

        // Unknown tracked app.
        assert!(add_usage_entry(&conn, &manual_entry("ghost-uid", "2026-10-11", 10)).is_err());
        // Bad date.
        assert!(add_usage_entry(&conn, &manual_entry(&app_uid, "2026/10/11", 10)).is_err());
        // Negative minutes.
        assert!(add_usage_entry(&conn, &manual_entry(&app_uid, "2026-10-11", -1)).is_err());
        // Unknown source.
        let mut bad_source = manual_entry(&app_uid, "2026-10-11", 10);
        bad_source.source = "guess".to_string();
        assert!(add_usage_entry(&conn, &bad_source).is_err());
    }

    #[test]
    fn delete_usage_entry_removes_the_contribution() {
        let conn = ready_bundle();
        let app_id = add_tracked_app(&conn, &sample_app("AnkiDroid")).unwrap();
        let app_uid = uid_of(&conn, app_id);
        let entry_id = add_usage_entry(&conn, &manual_entry(&app_uid, "2026-10-11", 30)).unwrap();

        delete_usage_entry(&conn, entry_id).unwrap();
        assert_eq!(
            get_tracked_app_total_minutes(&conn, "2026-10-11").unwrap(),
            0
        );
        assert!(delete_usage_entry(&conn, entry_id).is_err());
    }
}
