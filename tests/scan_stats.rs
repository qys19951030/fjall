// Copyright (c) 2024-present, fjall-rs
// This source code is licensed under both the Apache 2.0 and MIT License
// (found in the LICENSE-* files in the repository)

use fjall::{Database, KeyspaceCreateOptions, ScanStats};
use test_log::test;

fn open_db() -> fjall::Result<(tempfile::TempDir, Database)> {
    let folder = tempfile::tempdir()?;
    let db = Database::builder(&folder).open()?;
    Ok((folder, db))
}

#[test]
fn scan_stats_skeleton_compiles() -> fjall::Result<()> {
    let (_folder, db) = open_db()?;
    let tree = db.keyspace("default", KeyspaceCreateOptions::default)?;

    let stats: ScanStats = tree.scan_stats::<&[u8], _>(..)?;
    assert_eq!(stats, ScanStats::zeroed());

    Ok(())
}
