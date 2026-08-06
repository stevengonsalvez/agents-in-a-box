//! Pre-migration auto-backup: the file-restore rollback the plan documents is
//! only real if the file exists, so the daemon copies `hangar.db` aside BEFORE
//! it walks through the forward-only migration door.
//!
//! Migrations here have no down files, so a daemon downgraded below a newly
//! applied migration does not start at all. The backup is the ONLY rollback.

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

use ainb_hangar_store::Store;

/// Apply every migration EXCEPT the newest, reproducing the schema an install
/// runs the moment before it upgrades. Returns the highest applied version.
async fn seed_one_migration_behind(dir: &std::path::Path) -> i64 {
    let mut migrator = sqlx::migrate::Migrator::new(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"),
    )
    .await
    .expect("load migrations directory");
    let newest = migrator.migrations.iter().map(|m| m.version).max().expect("migrations exist");
    migrator.migrations.to_mut().retain(|m| m.version < newest);
    let applied = migrator.migrations.iter().map(|m| m.version).max().expect("a prior migration");

    let opts = SqliteConnectOptions::new()
        .filename(dir.join("hangar.db"))
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal);
    let pool = SqlitePoolOptions::new().connect_with(opts).await.expect("open pool");
    migrator.run(&pool).await.expect("prior migrations apply");
    pool.close().await;
    applied
}

fn backups(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("read hangar home")
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str().map(ToString::to_string))
        .filter(|name| name.starts_with("hangar.db.pre-") && name.ends_with(".bak"))
        .collect();
    names.sort();
    names
}

/// A boot with a PENDING migration backs the database up first; the next boot,
/// with nothing pending, does not (a backup per boot would be pure churn).
#[tokio::test]
async fn pending_migration_backs_up_and_a_clean_boot_does_not() {
    let dir = tempfile::tempdir().expect("temp dir");
    let applied = seed_one_migration_behind(dir.path()).await;

    let store = Store::open_in(dir.path()).await.expect("upgrade boot");
    drop(store);
    assert_eq!(
        backups(dir.path()),
        vec![format!("hangar.db.pre-{applied}.bak")],
        "the upgrading boot must leave a restorable copy of the PRE-upgrade file"
    );

    let store = Store::open_in(dir.path()).await.expect("steady-state boot");
    drop(store);
    assert_eq!(
        backups(dir.path()),
        vec![format!("hangar.db.pre-{applied}.bak")],
        "a boot with nothing pending must not write another backup"
    );
}

/// A fresh home has no prior state to preserve, so it writes no backup.
#[tokio::test]
async fn fresh_database_writes_no_backup() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = Store::open_in(dir.path()).await.expect("fresh boot");
    drop(store);
    assert!(
        backups(dir.path()).is_empty(),
        "a fresh database has nothing to back up"
    );
}

/// Only the newest two backups survive: the last-known-good plus the one
/// before it. Older copies are pruned so upgrades cannot eat the disk.
#[tokio::test]
async fn only_the_newest_two_backups_are_kept() {
    let dir = tempfile::tempdir().expect("temp dir");
    let applied = seed_one_migration_behind(dir.path()).await;
    for old in [1, 2] {
        std::fs::write(dir.path().join(format!("hangar.db.pre-{old}.bak")), b"old")
            .expect("plant an older backup");
    }

    let store = Store::open_in(dir.path()).await.expect("upgrade boot");
    drop(store);

    assert_eq!(
        backups(dir.path()),
        vec![
            "hangar.db.pre-2.bak".to_string(),
            format!("hangar.db.pre-{applied}.bak"),
        ],
        "the oldest backup is pruned once a third one lands"
    );
}

/// The backup is a COMPLETE database, not a torso whose tail is still in the
/// WAL: it opens on its own and answers at the pre-upgrade schema version.
#[tokio::test]
async fn the_backup_opens_as_a_database_at_the_pre_upgrade_version() {
    let dir = tempfile::tempdir().expect("temp dir");
    let applied = seed_one_migration_behind(dir.path()).await;
    let store = Store::open_in(dir.path()).await.expect("upgrade boot");
    drop(store);

    assert_eq!(
        backup_version(&dir.path().join(format!("hangar.db.pre-{applied}.bak"))).await,
        applied,
        "the backup holds the schema as it was BEFORE the pending migration"
    );
}

/// Every opener races here: `Store::open_in` is the daemon boot path AND the
/// path every `ainb hangar *` invocation takes, so on the upgrade boot they all
/// read the same pre-upgrade version and all decide to back up.
///
/// Whatever survives must be a complete database at the PRE-upgrade version. A
/// plain `fs::copy` into the shared name fails this two ways: concurrent copies
/// interleave into one file, and a late opener that read the old version but
/// only copied after someone else migrated files a POST-upgrade database under
/// the pre-upgrade name, which restores as a silent no-op.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_openers_leave_only_complete_pre_upgrade_backups() {
    let dir = tempfile::tempdir().expect("temp dir");
    let applied = seed_one_migration_behind(dir.path()).await;

    let mut opens = Vec::new();
    for _ in 0..6 {
        let path = dir.path().to_path_buf();
        opens.push(tokio::spawn(async move {
            Store::open_in(&path).await.map(|_| ())
        }));
    }
    // `sqlx::migrate!().run()` takes no cross-process lock on SQLite, so a
    // loser of the migrate race can still report "table already exists". That
    // is pre-existing behaviour and orthogonal to the backup; what this test
    // pins is that the ARTIFACT every racer wrote is trustworthy.
    let mut booted = 0;
    for open in opens {
        if open.await.expect("join").is_ok() {
            booted += 1;
        }
    }
    assert!(
        booted >= 1,
        "at least one concurrent opener must complete the upgrade"
    );

    let names = backups(dir.path());
    assert_eq!(
        names,
        vec![format!("hangar.db.pre-{applied}.bak")],
        "one visible backup, named for the version it actually holds"
    );
    for name in names {
        assert_eq!(
            backup_version(&dir.path().join(&name)).await,
            applied,
            "{name} must be a complete PRE-upgrade database, not a post-upgrade or torn copy"
        );
    }
    let leftovers: Vec<String> = std::fs::read_dir(dir.path())
        .expect("read hangar home")
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str().map(ToString::to_string))
        .filter(|name| name.ends_with(".tmp"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "staging snapshots must be renamed or removed, not left behind: {leftovers:?}"
    );
}

/// Open a backup on its own and read the schema version it holds.
async fn backup_version(path: &std::path::Path) -> i64 {
    let opts = SqliteConnectOptions::new().filename(path).create_if_missing(false);
    let pool = SqlitePoolOptions::new().connect_with(opts).await.expect("open the backup");
    let version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .expect("read the backup's schema version");
    pool.close().await;
    version
}
