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

/// Deterministic LCG so the workload is reproducible without test flakiness.
struct Rng(u64);

impl Rng {
    fn next_u32(&mut self) -> u32 {
        // Numerical Recipes constants.
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (self.0 >> 32) as u32
    }

    fn below(&mut self, bound: u32) -> u32 {
        self.next_u32() % bound
    }
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
    let rank = (numerator * n).div_ceil(denominator);
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

#[test]
fn scan_stats_single_item() -> fjall::Result<()> {
    let (_folder, db) = open_db()?;
    let tree = db.keyspace("default", KeyspaceCreateOptions::default)?;

    tree.insert("solo", "abcde")?;

    let stats = tree.scan_stats::<&[u8], _>(..)?;

    assert_eq!(stats.key_count, 1);
    assert_eq!(stats.total_value_bytes, 5);
    assert_eq!(stats.average_value_size, 5);
    assert_eq!(stats.empty_value_count, 0);
    assert_eq!(&*stats.min_key, b"solo");
    assert_eq!(&*stats.max_key, b"solo");
    assert_eq!(stats.quantile_50, 5);
    assert_eq!(stats.quantile_90, 5);
    assert_eq!(stats.quantile_99, 5);

    Ok(())
}

#[test]
fn scan_stats_range_bounds_are_half_open() -> fjall::Result<()> {
    let (_folder, db) = open_db()?;
    let tree = db.keyspace("default", KeyspaceCreateOptions::default)?;

    for k in ["a", "b", "c", "d"] {
        tree.insert(k, "x")?;
    }

    // Lower bound inclusive, upper bound exclusive: b and c only.
    let stats = tree.scan_stats("b".."d")?;
    assert_eq!(stats.key_count, 2);
    assert_eq!(&*stats.min_key, b"b");
    assert_eq!(&*stats.max_key, b"c");

    // Inclusive upper bound expressed as an inclusive range.
    let stats = tree.scan_stats("b"..="c")?;
    assert_eq!(stats.key_count, 2);

    // Unbounded upper side takes everything from b.
    let stats = tree.scan_stats("b"..)?;
    assert_eq!(stats.key_count, 3);

    // Unbounded lower side stops just before d.
    let stats = tree.scan_stats(.. "d")?;
    assert_eq!(stats.key_count, 3);

    Ok(())
}

#[test]
fn scan_stats_works_over_a_prefix_derived_range() -> fjall::Result<()> {
    let (_folder, db) = open_db()?;
    let tree = db.keyspace("default", KeyspaceCreateOptions::default)?;

    for i in 0..5u32 {
        tree.insert(format!("user:{i}"), vec![b'a'; i as usize + 1])?;
    }
    tree.insert("other:1", "zzzz")?;

    // Same range construction the prefix iterator uses internally.
    let range = fjall::util::prefix_to_range(b"user:");
    let stats = tree.scan_stats(range)?;

    assert_eq!(stats.key_count, 5);
    assert_eq!(&*stats.min_key, b"user:0");
    assert_eq!(&*stats.max_key, b"user:4");
    assert_eq!(stats.total_value_bytes, 1 + 2 + 3 + 4 + 5);

    Ok(())
}

#[test]
fn scan_stats_is_read_only() -> fjall::Result<()> {
    let (_folder, db) = open_db()?;
    let tree = db.keyspace("default", KeyspaceCreateOptions::default)?;

    for i in 0..1_000u32 {
        tree.insert(format!("k{i:06}"), vec![b'v'; (i % 7 + 1) as usize])?;
    }

    let sealed_before = tree.sealed_memtable_count();
    let tables_before = tree.table_count();
    let disk_before = tree.disk_space();
    let len_before = tree.len()?;

    // Scan several times; nothing may be flushed or compacted.
    for _ in 0..3 {
        let stats = tree.scan_stats::<&[u8], _>(..)?;
        assert_eq!(stats.key_count, 1_000);
    }

    assert_eq!(tree.sealed_memtable_count(), sealed_before);
    assert_eq!(tree.table_count(), tables_before);
    assert_eq!(tree.disk_space(), disk_before);
    assert_eq!(tree.len()?, len_before);

    Ok(())
}

const LARGE_N: usize = 100_000;

/// Asserts the histogram estimate is within 3% relative error of the exact
/// nearest-rank quantile computed from a full sorted reference multiset.
fn assert_quantiles_within_three_percent(stats: &ScanStats, reference: &[u32]) {
    let n = reference.len() as u64;

    for (name, numerator, estimated) in [
        ("p50", 50u64, stats.quantile_50),
        ("p90", 90u64, stats.quantile_90),
        ("p99", 99u64, stats.quantile_99),
    ] {
        let rank = (numerator * n).div_ceil(100);
        let exact = u64::from(reference[(rank - 1) as usize]);

        let error = estimated.abs_diff(exact);
        // relative error <= 3%; exact values here are always >= 1.
        assert!(
            error * 100 <= 3 * exact,
            "{name}: estimated {estimated} vs exact {exact} ({}% error)",
            error * 100 / exact.max(1),
        );
    }
}

#[test]
fn scan_stats_quantiles_accurate_on_100k_uniform() -> fjall::Result<()> {
    let (_folder, db) = open_db()?;
    let tree = db.keyspace("default", KeyspaceCreateOptions::default)?;

    let mut rng = Rng(0x1234_5678);
    let mut reference = Vec::with_capacity(LARGE_N);

    for i in 0..LARGE_N {
        // Sizes between 1 and 4_096, far more distinct values than buckets.
        let size = rng.below(4_096) + 1;
        reference.push(size);
        tree.insert(format!("k{i:07}"), vec![b'x'; size as usize])?;
    }
    reference.sort_unstable();

    let stats = tree.scan_stats::<&[u8], _>(..)?;

    assert_eq!(stats.key_count as usize, LARGE_N);
    assert_eq!(stats.empty_value_count, 0);
    assert_eq!(
        stats.total_value_bytes,
        reference.iter().copied().map(u64::from).sum::<u64>()
    );
    assert_quantiles_within_three_percent(&stats, &reference);

    // Determinism: a second scan returns byte-identical statistics.
    assert_eq!(tree.scan_stats::<&[u8], _>(..)?, stats);

    Ok(())
}

#[test]
fn scan_stats_quantiles_accurate_on_100k_skewed() -> fjall::Result<()> {
    let (_folder, db) = open_db()?;
    let tree = db.keyspace("default", KeyspaceCreateOptions::default)?;

    let mut rng = Rng(0xdead_beef);
    let mut reference = Vec::with_capacity(LARGE_N);

    for i in 0..LARGE_N {
        // Bounded skew over a single span: many small values with a long
        // shoulder. Distinct sizes (~5k) exceed the fixed bucket count, so
        // the bucket phase is exercised, while the equal-width buckets stay
        // narrow relative to each quantile's true value.
        let r = rng.below(10_000);
        let size = (u64::from(r) * u64::from(r) / 20_000 + 1).min(4_999) as u32;
        reference.push(size);
        tree.insert(format!("k{i:07}"), vec![b'x'; size as usize])?;
    }
    reference.sort_unstable();

    assert!(
        reference.iter().collect::<std::collections::HashSet<_>>().len() > 1024,
        "workload must exceed the fixed bucket count to test estimation",
    );

    let stats = tree.scan_stats::<&[u8], _>(..)?;
    assert_eq!(stats.key_count as usize, LARGE_N);
    assert_quantiles_within_three_percent(&stats, &reference);

    Ok(())
}

#[test]
fn scan_stats_quantiles_accurate_with_empty_values_mixed_in() -> fjall::Result<()> {
    let (_folder, db) = open_db()?;
    let tree = db.keyspace("default", KeyspaceCreateOptions::default)?;

    let mut rng = Rng(0x0bad_c0de);
    let mut reference = Vec::with_capacity(LARGE_N);
    let mut empties = 0u64;

    for i in 0..LARGE_N {
        if rng.below(10) == 0 {
            empties += 1;
            tree.insert(format!("k{i:07}"), [])?;
        } else {
            let size = rng.below(2_048) + 1;
            reference.push(size);
            tree.insert(format!("k{i:07}"), vec![b'x'; size as usize])?;
        }
    }
    reference.sort_unstable();

    let stats = tree.scan_stats::<&[u8], _>(..)?;

    assert_eq!(stats.key_count as usize, LARGE_N);
    assert_eq!(stats.empty_value_count, empties);
    // Quantiles are computed over non-empty values only.
    assert_quantiles_within_three_percent(&stats, &reference);
    assert_eq!(
        stats.total_value_bytes,
        reference.iter().copied().map(u64::from).sum::<u64>()
    );

    Ok(())
}
