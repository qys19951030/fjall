// Copyright (c) 2024-present, fjall-rs
// This source code is licensed under both the Apache 2.0 and MIT License
// (found in the LICENSE-* files in the repository)

//! Bounded-memory scan statistics over a key range.
//!
//! See [`crate::Keyspace::scan_stats`].

use lsm_tree::{UserKey, UserValue};

/// Number of buckets used by the fixed-size value-size histogram.
///
/// The histogram keeps exactly this many buckets regardless of how many
/// items are scanned, so the memory used by a statistics scan has a hard
/// upper bound.
pub(crate) const HISTOGRAM_BUCKET_COUNT: usize = 1024;

/// Statistics gathered by scanning a key range.
///
/// All counters are exact; the three quantile values are estimated with a
/// fixed-bucket histogram once the number of distinct value sizes exceeds
/// [`HISTOGRAM_BUCKET_COUNT`], and are exact below that threshold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanStats {
    /// Number of keys in the scanned range.
    pub key_count: u64,

    /// Sum of all value sizes in bytes (empty values contribute `0`).
    pub total_value_bytes: u64,

    /// Mean value size in bytes, rounded down (`floor(total / count)`).
    ///
    /// `0` for an empty range.
    pub average_value_size: u64,

    /// Smallest key in the range, by byte order.
    ///
    /// Empty (`len() == 0`) for an empty range.
    pub min_key: UserKey,

    /// Largest key in the range, by byte order.
    ///
    /// Empty (`len() == 0`) for an empty range.
    pub max_key: UserKey,

    /// Number of keys whose value is empty (`len() == 0`).
    ///
    /// Empty values count toward [`key_count`](Self::key_count) and
    /// [`total_value_bytes`](Self::total_value_bytes) but are excluded from
    /// quantile estimation.
    pub empty_value_count: u64,

    /// Nearest-rank median (p50) of value sizes.
    ///
    /// `0` when the range contains no non-empty values.
    pub quantile_50: u64,

    /// Nearest-rank 90th percentile (p90) of value sizes.
    ///
    /// `0` when the range contains no non-empty values.
    pub quantile_90: u64,

    /// Nearest-rank 99th percentile (p99) of value sizes.
    ///
    /// `0` when the range contains no non-empty values.
    pub quantile_99: u64,
}

impl ScanStats {
    /// The all-zero result returned for an empty range (or a range whose
    /// values are all empty).
    pub fn zeroed() -> Self {
        Self::empty()
    }

    /// The all-zero result returned for an empty range (or a range whose
    /// values are all empty).
    pub(crate) fn empty() -> Self {
        Self {
            key_count: 0,
            total_value_bytes: 0,
            average_value_size: 0,
            min_key: UserKey::empty(),
            max_key: UserKey::empty(),
            empty_value_count: 0,
            quantile_50: 0,
            quantile_90: 0,
            quantile_99: 0,
        }
    }
}

/// Accumulator fed one item at a time during the single scan pass.
///
/// It holds at most [`HISTOGRAM_BUCKET_COUNT`] histogram buckets (plus a
/// bounded exact-value phase of the same size), never the scanned keys or
/// values themselves.
#[derive(Debug)]
pub(crate) struct ScanStatsAccumulator {
    key_count: u64,
    total_value_bytes: u64,
    empty_value_count: u64,
    min_key: Option<UserKey>,
    max_key: Option<UserKey>,
}

impl ScanStatsAccumulator {
    pub(crate) fn new() -> Self {
        Self {
            key_count: 0,
            total_value_bytes: 0,
            empty_value_count: 0,
            min_key: None,
            max_key: None,
        }
    }

    /// Feeds a single key-value pair into the accumulator.
    pub(crate) fn observe(&mut self, key: &UserKey, value: &UserValue) {
        self.key_count += 1;
        self.total_value_bytes += value.len() as u64;

        if value.is_empty() {
            self.empty_value_count += 1;
        }

        // The iterator yields keys in byte order, but track the endpoints
        // explicitly so the contract does not rely on iterator ordering.
        match &self.min_key {
            Some(current) if current.as_ref() <= key.as_ref() => {}
            _ => self.min_key = Some(key.clone()),
        }
        match &self.max_key {
            Some(current) if current.as_ref() >= key.as_ref() => {}
            _ => self.max_key = Some(key.clone()),
        }
    }

    /// Produces the immutable [`ScanStats`] snapshot.
    pub(crate) fn finish(self) -> ScanStats {
        let mut result = ScanStats::empty();

        result.key_count = self.key_count;
        result.total_value_bytes = self.total_value_bytes;
        result.empty_value_count = self.empty_value_count;
        result.min_key = self.min_key.unwrap_or_else(UserKey::empty);
        result.max_key = self.max_key.unwrap_or_else(UserKey::empty);

        // Integer mean, rounded down; an empty range stays at zero.
        if self.key_count > 0 {
            result.average_value_size = self.total_value_bytes / self.key_count;
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(bytes: &[u8]) -> UserKey {
        UserKey::new(bytes)
    }

    fn value(bytes: &[u8]) -> UserValue {
        UserValue::new(bytes)
    }

    #[test]
    fn skeleton_empty_stats_are_all_zero() {
        let stats = ScanStatsAccumulator::new().finish();
        assert_eq!(stats, ScanStats::empty());
        assert!(stats.min_key.is_empty());
        assert!(stats.max_key.is_empty());
    }

    #[test]
    fn counters_and_endpoints_are_tracked_in_one_pass() {
        let mut acc = ScanStatsAccumulator::new();

        // Deliberately out of order; endpoints must still be byte-ordered.
        acc.observe(&key(b"m"), &value(b"hello"));
        acc.observe(&key(b"a"), &value(b"xy"));
        acc.observe(&key(b"z"), &value(b""));
        acc.observe(&key(b"b"), &value(b"abc"));

        let stats = acc.finish();

        assert_eq!(stats.key_count, 4);
        assert_eq!(stats.total_value_bytes, 10);
        assert_eq!(stats.average_value_size, 2); // floor(10 / 4)
        assert_eq!(stats.empty_value_count, 1);
        assert_eq!(&*stats.min_key, b"a");
        assert_eq!(&*stats.max_key, b"z");
    }

    #[test]
    fn single_item_is_both_endpoints_and_mean_is_exact() {
        let mut acc = ScanStatsAccumulator::new();
        acc.observe(&key(b"only"), &value(b"seven77"));

        let stats = acc.finish();

        assert_eq!(stats.key_count, 1);
        assert_eq!(stats.total_value_bytes, 7);
        assert_eq!(stats.average_value_size, 7);
        assert_eq!(stats.empty_value_count, 0);
        assert_eq!(&*stats.min_key, b"only");
        assert_eq!(&*stats.max_key, b"only");
    }

    #[test]
    fn all_empty_values_count_but_contribute_no_bytes() {
        let mut acc = ScanStatsAccumulator::new();
        acc.observe(&key(b"a"), &value(b""));
        acc.observe(&key(b"b"), &value(b""));

        let stats = acc.finish();

        assert_eq!(stats.key_count, 2);
        assert_eq!(stats.total_value_bytes, 0);
        assert_eq!(stats.average_value_size, 0);
        assert_eq!(stats.empty_value_count, 2);
        assert_eq!(&*stats.min_key, b"a");
        assert_eq!(&*stats.max_key, b"b");
    }
}
