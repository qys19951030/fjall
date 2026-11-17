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

#[test]
fn scan_stats_counts_bytes_endpoints_and_empty_values() -> fjall::Result<()> {
    let (_folder, db) = open_db()?;
    let tree = db.keyspace("default", KeyspaceCreateOptions::default)?;

    tree.insert("key_1", "hello")?; // 5 bytes
    tree.insert("key_2", "xy")?; // 2 bytes
    tree.insert("key_3", "")?; // empty
    tree.insert("key_4", "abc")?; // 3 bytes
    tree.insert("other", "ignored")?;

    // Bounded, half-open range: inclusive lower, exclusive upper.
    let stats = tree.scan_stats("key_1".."key_4")?;

    assert_eq!(stats.key_count, 3);
    assert_eq!(stats.total_value_bytes, 7);
    assert_eq!(stats.average_value_size, 2); // floor(7 / 3)
    assert_eq!(stats.empty_value_count, 1);
    assert_eq!(&*stats.min_key, b"key_1");
    assert_eq!(&*stats.max_key, b"key_3");

    Ok(())
}

#[test]
fn scan_stats_empty_range_returns_all_zero() -> fjall::Result<()> {
    let (_folder, db) = open_db()?;
    let tree = db.keyspace("default", KeyspaceCreateOptions::default)?;

    tree.insert("a", "value")?;
    tree.insert("b", "value")?;

    let stats = tree.scan_stats("x".."z")?;
    assert_eq!(stats, ScanStats::zeroed());
    assert!(stats.min_key.is_empty());
    assert!(stats.max_key.is_empty());

    // A reversed / impossible half-open range is empty as well.
    let stats = tree.scan_stats("b".."a")?;
    assert_eq!(stats, ScanStats::zeroed());

    Ok(())
}

/// Nearest-rank reference implementation over an explicit sorted multiset.
fn nearest_rank(sorted: &[u32], numerator: u64, denominator: u64) -> u32 {
    let n = sorted.len() as u64;
    let rank = (numerator * n + denominator - 1) / denominator;
    sorted[(rank - 1) as usize]
}

#[test]
fn scan_stats_quantiles_are_exact_below_bucket_count() -> fjall::Result<()> {
    let (_folder, db) = open_db()?;
    let tree = db.keyspace("default", KeyspaceCreateOptions::default)?;

    // Fewer distinct sizes than the (internal) fixed bucket count, so every
    // size occupies its own slot and quantiles must be exact.
    let sizes = [1u32, 1, 2, 2, 2, 3, 5, 8, 8, 13, 21, 21, 21, 34, 55];
    for (i, size) in sizes.iter().enumerate() {
        tree.insert(format!("k{i:04}"), vec![b'x'; *size as usize])?;
    }

    let stats = tree.scan_stats::<&[u8], _>(..)?;

    let mut sorted = sizes;
    sorted.sort_unstable();

    assert_eq!(stats.quantile_50, u64::from(nearest_rank(&sorted, 50, 100)));
    assert_eq!(stats.quantile_90, u64::from(nearest_rank(&sorted, 90, 100)));
    assert_eq!(stats.quantile_99, u64::from(nearest_rank(&sorted, 99, 100)));

    // Repeated scans over the same snapshot of data must be identical.
    let stats_again = tree.scan_stats::<&[u8], _>(..)?;
    assert_eq!(stats, stats_again);

    Ok(())
}
