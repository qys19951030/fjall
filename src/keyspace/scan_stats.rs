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
    pub(crate) fn observe(&mut self, _key: &UserKey, _value: &UserValue) {
        // Filled in during the next implementation step.
    }

    /// Produces the immutable [`ScanStats`] snapshot.
    pub(crate) fn finish(self) -> ScanStats {
        // Skeleton: always return the all-zero result.
        let mut result = ScanStats::empty();
        result.min_key = self.min_key.unwrap_or_else(UserKey::empty);
        result.max_key = self.max_key.unwrap_or_else(UserKey::empty);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skeleton_empty_stats_are_all_zero() {
        let stats = ScanStatsAccumulator::new().finish();
        assert_eq!(stats, ScanStats::empty());
        assert!(stats.min_key.is_empty());
        assert!(stats.max_key.is_empty());
    }
}
