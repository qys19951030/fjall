// Copyright (c) 2024-present, fjall-rs
// This source code is licensed under both the Apache 2.0 and MIT License
// (found in the LICENSE-* files in the repository)

//! Bounded-memory scan statistics over a key range.
//!
//! See [`crate::Keyspace::scan_stats`].

use lsm_tree::{UserKey, UserValue};
use std::collections::HashMap;

/// Number of buckets used by the fixed-size value-size histogram.
///
/// The histogram keeps exactly this many buckets regardless of how many
/// items are scanned, so the memory used by a statistics scan has a hard
/// upper bound.
pub const HISTOGRAM_BUCKET_COUNT: usize = 1024;

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
    #[must_use]
    pub fn zeroed() -> Self {
        Self::empty()
    }

    /// The all-zero result returned for an empty range (or a range whose
    /// values are all empty).
    #[must_use]
    pub fn empty() -> Self {
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

/// Fixed-bucket histogram of non-empty value sizes.
///
/// It runs in two phases:
///
/// 1. **Exact phase**: while at most [`HISTOGRAM_BUCKET_COUNT`] distinct
///    sizes have been seen, every distinct size occupies its own slot, so
///    quantiles are computed from the true multiset.
/// 2. **Bucket phase**: once the distinct-size count would exceed the bucket
///    count, an equal-width histogram spanning the observed min and max is
///    built. If a later value falls outside the current span, the span is
///    widened (the width is multiplied by a power of two) and every existing
///    bucket is re-binned, keeping the bucket count constant.
///
/// The struct therefore never allocates beyond a fixed number of slots:
/// memory has a hard upper bound independent of the number of observations.
#[derive(Debug)]
pub enum ValueSizeHistogram {
    /// Exact phase: one counter per distinct observed size.
    Exact(HashMap<u32, u64>),

    /// Bucket phase: equal-width buckets `[min + i*width, min + (i+1)*width)`,
    /// with the last bucket additionally covering the observed maximum.
    ///
    /// `min` and `max` are the actual smallest/largest observed sizes, so a
    /// value escaping the current span can rebuild the histogram over the
    /// true span without keeping any raw values.
    Bucketed {
        min: u32,
        max: u32,
        width: u64,
        buckets: Box<[u64; HISTOGRAM_BUCKET_COUNT]>,
    },
}

impl ValueSizeHistogram {
    pub fn new() -> Self {
        Self::Exact(HashMap::with_capacity(HISTOGRAM_BUCKET_COUNT))
    }

    /// Records one non-empty value size.
    pub fn observe(&mut self, size: u32) {
        debug_assert!(size > 0, "empty values must not enter the histogram");

        match self {
            Self::Exact(counts) => {
                if let Some(count) = counts.get_mut(&size) {
                    *count += 1;
                    return;
                }

                if counts.len() < HISTOGRAM_BUCKET_COUNT {
                    counts.insert(size, 1);
                } else {
                    // Promote to a fixed-width histogram over the observed span.
                    let mut min = u32::MAX;
                    let mut max = 0u32;
                    for &seen in counts.keys() {
                        min = min.min(seen);
                        max = max.max(seen);
                    }
                    min = min.min(size);
                    max = max.max(size);

                    let width = bucket_width(min, max);
                    let mut buckets = Box::new([0u64; HISTOGRAM_BUCKET_COUNT]);

                    for (&seen, &count) in counts.iter() {
                        increment_bucket(
                            &mut *buckets,
                            bucket_index(u64::from(seen), u64::from(min), width),
                            count,
                        );
                    }
                    increment_bucket(
                        &mut *buckets,
                        bucket_index(u64::from(size), u64::from(min), width),
                        1,
                    );

                    *self = Self::Bucketed {
                        min,
                        max,
                        width,
                        buckets,
                    };
                }
            }

            Self::Bucketed {
                min,
                max,
                width,
                buckets,
            } => {
                let escapes = size < *min || u64::from(size) >= upper_edge(*min, *width);

                if escapes {
                    // The value escapes the current span: rebuild equal-width
                    // buckets over the true new span, re-binning existing
                    // buckets by their lower boundary value.
                    let old_min = *min;
                    let old_width = *width;

                    *min = (*min).min(size);
                    *max = (*max).max(size);
                    *width = bucket_width(*min, *max);

                    let mut re_binned = [0u64; HISTOGRAM_BUCKET_COUNT];
                    for (index, &count) in buckets.iter().enumerate() {
                        if count == 0 {
                            continue;
                        }
                        let boundary = u64::from(old_min) + old_width * index as u64;
                        increment_bucket(
                            &mut re_binned,
                            bucket_index(boundary, u64::from(*min), *width),
                            count,
                        );
                    }
                    **buckets = re_binned;
                } else {
                    *max = (*max).max(size);
                }

                increment_bucket(
                    &mut **buckets,
                    bucket_index(u64::from(size), u64::from(*min), *width),
                    1,
                );
            }
        }
    }

    /// Total number of recorded (non-empty) values.
    pub fn total_count(&self) -> u64 {
        match self {
            Self::Exact(counts) => counts.values().sum(),
            Self::Bucketed { buckets, .. } => buckets.iter().sum(),
        }
    }

    /// Nearest-rank quantile using pure integer arithmetic.
    ///
    /// The quantile is `numerator / denominator` (e.g. `9 / 10` for p90) and
    /// must lie in `(0, 1]`. The nearest rank is
    /// `ceil(numerator * n / denominator)`, computed without floating point.
    ///
    /// Returns `None` when no values were recorded. In bucket phase, every
    /// value in a bucket is represented by that bucket's lower boundary, per
    /// the histogram contract.
    pub fn nearest_rank_quantile(
        &self,
        numerator: u64,
        denominator: u64,
    ) -> Option<u32> {
        debug_assert!(numerator > 0 && numerator <= denominator);

        let total = self.total_count();
        if total == 0 {
            return None;
        }

        // ceil(numerator * total / denominator), 1-based rank.
        // The u128 intermediate cannot overflow for any realistic item count.
        let rank: u64 = (u128::from(numerator) * u128::from(total))
            .div_ceil(u128::from(denominator))
            .try_into()
            .unwrap_or(total);

        match self {
            Self::Exact(counts) => {
                let mut sizes: Vec<u32> = counts.keys().copied().collect();
                sizes.sort_unstable();

                let mut cumulative = 0;
                for size in sizes {
                    // `size` comes straight out of this map, so the lookup is
                    // always present; use get to avoid panicking index syntax.
                    let count = counts.get(&size).copied().unwrap_or(0);
                    cumulative += count;
                    if cumulative >= rank {
                        return Some(size);
                    }
                }
                unreachable!("rank {rank} is within the total count {total}");
            }

            Self::Bucketed { min, width, buckets, .. } => {
                let mut cumulative = 0;
                for (index, &count) in buckets.iter().enumerate() {
                    cumulative += count;
                    if cumulative >= rank {
                        let lower = u64::from(*min) + *width * index as u64;
                        // A bucket lower boundary never exceeds the observed
                        // maximum, which is itself a u32 value size.
                        return Some(u32::try_from(lower).unwrap_or(u32::MAX));
                    }
                }
                unreachable!("rank {rank} is within the total count {total}");
            }
        }
    }
}

/// Equal bucket width for a span, guaranteeing the maximum lands in a valid
/// bucket instead of being pushed past the last one.
fn bucket_width(min: u32, max: u32) -> u64 {
    debug_assert!(max >= min);

    if max == min {
        // All values share one size; any positive width keeps them in bucket 0.
        return 1;
    }

    let span = u64::from(max) - u64::from(min);
    span.div_ceil(HISTOGRAM_BUCKET_COUNT as u64).max(1)
}

/// The first value strictly beyond the histogram's current last bucket.
fn upper_edge(min: u32, width: u64) -> u64 {
    u64::from(min) + width * HISTOGRAM_BUCKET_COUNT as u64
}

/// Maps a value to its bucket index.
///
/// The last bucket additionally swallows any rounding remainder so the
/// observed maximum is always covered.
fn bucket_index(value: u64, min: u64, width: u64) -> usize {
    let offset = value.saturating_sub(min);
    let index = offset / width;
    index
        .try_into()
        .unwrap_or(HISTOGRAM_BUCKET_COUNT - 1)
        .min(HISTOGRAM_BUCKET_COUNT - 1)
}

/// Adds `count` to one bucket.
///
/// [`bucket_index`] clamps into `0..HISTOGRAM_BUCKET_COUNT`, so the lookup
/// is always in bounds; this helper centralizes that invariant instead of
/// using panicking index syntax.
fn increment_bucket(buckets: &mut [u64], index: usize, count: u64) {
    if let Some(slot) = buckets.get_mut(index) {
        *slot += count;
    } else {
        // Unreachable: bucket_index is clamped to the bucket count.
        debug_assert!(false, "bucket index {index} out of range");
    }
}

/// Accumulator fed one item at a time during the single scan pass.
///
/// It holds at most [`HISTOGRAM_BUCKET_COUNT`] histogram buckets (plus a
/// bounded exact-value phase of the same size), never the scanned keys or
/// values themselves.
#[derive(Debug)]
pub struct ScanStatsAccumulator {
    key_count: u64,
    total_value_bytes: u64,
    empty_value_count: u64,
    min_key: Option<UserKey>,
    max_key: Option<UserKey>,

    /// Histogram of non-empty value sizes; empty values never enter it.
    histogram: ValueSizeHistogram,
}

impl ScanStatsAccumulator {
    pub fn new() -> Self {
        Self {
            key_count: 0,
            total_value_bytes: 0,
            empty_value_count: 0,
            min_key: None,
            max_key: None,
            histogram: ValueSizeHistogram::new(),
        }
    }

    /// Feeds a single key-value pair into the accumulator.
    pub fn observe(&mut self, key: &UserKey, value: &UserValue) {
        self.key_count += 1;
        self.total_value_bytes += value.len() as u64;

        if value.is_empty() {
            self.empty_value_count += 1;
        } else {
            // The engine exposes value sizes as u32 (see Guard::size); a
            // value larger than that cannot occur, so saturation is a safe
            // defensive bound rather than a real truncation.
            let size = u32::try_from(value.len()).unwrap_or(u32::MAX);
            self.histogram.observe(size);
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
    pub fn finish(self) -> ScanStats {
        let mut result = ScanStats::empty();

        result.key_count = self.key_count;
        result.total_value_bytes = self.total_value_bytes;
        result.empty_value_count = self.empty_value_count;
        result.min_key = self.min_key.unwrap_or_else(UserKey::empty);
        result.max_key = self.max_key.unwrap_or_else(UserKey::empty);

        // Integer mean, rounded down; an empty range stays at zero.
        if let Some(mean) = self.total_value_bytes.checked_div(self.key_count) {
            result.average_value_size = mean;
        }

        // Quantiles only consider non-empty values. With no non-empty value
        // the three fields stay zero. Once p50 exists, p90 and p99 exist as
        // well, because they share the same underlying histogram.
        if let Some(p50) = self.histogram.nearest_rank_quantile(50, 100) {
            result.quantile_50 = u64::from(p50);

            if let Some(p90) = self.histogram.nearest_rank_quantile(90, 100) {
                result.quantile_90 = u64::from(p90);
            }
            if let Some(p99) = self.histogram.nearest_rank_quantile(99, 100) {
                result.quantile_99 = u64::from(p99);
            }
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // u32 mirror of the bucket count for test ranges, kept in lockstep.
    const BUCKET_COUNT: u32 = 1024;
    const _: () = assert!(BUCKET_COUNT as usize == HISTOGRAM_BUCKET_COUNT);

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

        // Non-empty sizes sorted: 2, 3, 5
        // p50 rank 2 -> 3, p90 rank 3 -> 5, p99 rank 3 -> 5
        assert_eq!(stats.quantile_50, 3);
        assert_eq!(stats.quantile_90, 5);
        assert_eq!(stats.quantile_99, 5);
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

        // No non-empty values: quantiles stay zero.
        assert_eq!(stats.quantile_50, 0);
        assert_eq!(stats.quantile_90, 0);
        assert_eq!(stats.quantile_99, 0);
    }

    #[test]
    fn empty_values_are_excluded_from_quantiles() {
        let mut acc = ScanStatsAccumulator::new();

        // 90 empty values plus 10 values of size 4.
        for i in 0..90 {
            acc.observe(&key(format!("e{i}").as_bytes()), &value(b""));
        }
        for i in 0..10 {
            acc.observe(
                &key(format!("v{i}").as_bytes()),
                &value(b"four"),
            );
        }

        let stats = acc.finish();

        assert_eq!(stats.key_count, 100);
        assert_eq!(stats.empty_value_count, 90);
        assert_eq!(stats.total_value_bytes, 40);
        // Every non-empty value has size 4, so all quantiles are 4 even
        // though 90% of all keys carry empty values.
        assert_eq!(stats.quantile_50, 4);
        assert_eq!(stats.quantile_90, 4);
        assert_eq!(stats.quantile_99, 4);
    }

    #[test]
    fn empty_histogram_has_no_quantiles() {
        let hist = ValueSizeHistogram::new();
        assert_eq!(hist.total_count(), 0);
        assert_eq!(hist.nearest_rank_quantile(1, 2), None);
    }

    #[test]
    fn exact_phase_quantiles_match_sorted_nearest_rank() {
        let mut hist = ValueSizeHistogram::new();

        // Well below the bucket count, every size gets its own slot.
        for (size, count) in [(1u32, 5u64), (3, 2), (10, 4), (17, 1), (42, 3)] {
            for _ in 0..count {
                hist.observe(size);
            }
        }

        assert_eq!(hist.total_count(), 15);
        assert!(matches!(hist, ValueSizeHistogram::Exact(_)));

        // Sorted multiset: 1x5, 3x2, 10x4, 17x1, 42x3
        // p50 rank 8 -> 10; p90 rank 14 -> 42; p99 rank 15 -> 42
        assert_eq!(hist.nearest_rank_quantile(50, 100), Some(10));
        assert_eq!(hist.nearest_rank_quantile(90, 100), Some(42));
        assert_eq!(hist.nearest_rank_quantile(99, 100), Some(42));
        // p100 is always the maximum under nearest rank.
        assert_eq!(hist.nearest_rank_quantile(1, 1), Some(42));
    }

    #[test]
    fn promotion_keeps_bucket_count_fixed_and_total_preserved() {
        let mut hist = ValueSizeHistogram::new();

        // Distinct sizes exceed the bucket count, forcing bucket phase.
        for size in 1..=(BUCKET_COUNT + 50) {
            hist.observe(size);
        }

        match &hist {
            ValueSizeHistogram::Bucketed { buckets, .. } => {
                assert_eq!(buckets.len(), HISTOGRAM_BUCKET_COUNT);
            }
            ValueSizeHistogram::Exact(_) => panic!("should have promoted to bucket phase"),
        }
        assert_eq!(
            hist.total_count(),
            (HISTOGRAM_BUCKET_COUNT as u64) + 50,
            "no observations may be lost during promotion"
        );
    }

    #[test]
    fn observed_maximum_is_covered_when_span_is_evenly_divisible() {
        let mut hist = ValueSizeHistogram::new();

        // Exactly BUCKET_COUNT + 1 distinct sizes spanning exactly
        // BUCKET_COUNT units, so the raw span is evenly divisible by the
        // bucket count. The maximum must still land inside a real bucket
        // instead of being shoved past a hard-coded upper edge.
        let min = 10u32;
        let max = min + BUCKET_COUNT;
        for size in min..=max {
            hist.observe(size);
        }

        match &hist {
            ValueSizeHistogram::Bucketed {
                min: actual_min,
                max: actual_max,
                width,
                buckets,
            } => {
                assert_eq!(*actual_min, min);
                assert_eq!(*actual_max, max);
                assert_eq!(*width, 1);

                let last = HISTOGRAM_BUCKET_COUNT - 1;
                // The final bucket holds the size at its lower boundary plus
                // the observed maximum, proving the maximum is covered rather
                // than pushed beyond a hard-coded upper edge.
                assert_eq!(
                    buckets.get(last).copied(),
                    Some(2),
                    "last bucket must contain the observed maximum",
                );
                assert_eq!(hist.total_count(), u64::from(BUCKET_COUNT) + 1);

                // The maximum itself is represented by its bucket's lower
                // boundary, per the histogram's nearest-rank contract.
                assert_eq!(hist.nearest_rank_quantile(1, 1), Some(max - 1));
            }
            ValueSizeHistogram::Exact(_) => panic!("should have promoted to bucket phase"),
        }
    }

    #[test]
    fn escaping_value_rebuilds_buckets_without_losing_counts() {
        let mut hist = ValueSizeHistogram::new();

        for size in 100..=(100 + BUCKET_COUNT + 10) {
            hist.observe(size);
        }
        let total_before = hist.total_count();

        // First escape far upward, then far downward.
        hist.observe(10_000);
        hist.observe(100_000);
        hist.observe(1);

        let (min, max) = match &hist {
            ValueSizeHistogram::Bucketed { min, max, buckets, .. } => {
                assert_eq!(buckets.len(), HISTOGRAM_BUCKET_COUNT);
                (*min, *max)
            }
            ValueSizeHistogram::Exact(_) => panic!("should stay in bucket phase"),
        };

        assert_eq!(min, 1);
        assert_eq!(max, 100_000);
        assert_eq!(hist.total_count(), total_before + 3);
    }

    #[test]
    fn constant_single_size_never_promotes_and_quantiles_are_exact() {
        let mut hist = ValueSizeHistogram::new();
        for _ in 0..100_000 {
            hist.observe(7);
        }

        assert!(matches!(hist, ValueSizeHistogram::Exact(_)));
        assert_eq!(hist.total_count(), 100_000);
        assert_eq!(hist.nearest_rank_quantile(50, 100), Some(7));
        assert_eq!(hist.nearest_rank_quantile(99, 100), Some(7));
    }

    #[test]
    fn hundred_thousand_observations_keep_fixed_memory_footprint() {
        let mut hist = ValueSizeHistogram::new();

        // Wide, monotonically growing values force promotion early, after
        // which repeated span escapes trigger many full rebuilds.
        for size in 1..=100_000u32 {
            hist.observe(size);
        }

        let (min, max) = match &hist {
            ValueSizeHistogram::Bucketed { buckets, min, max, .. } => {
                assert_eq!(buckets.len(), HISTOGRAM_BUCKET_COUNT);
                (*min, *max)
            }
            ValueSizeHistogram::Exact(_) => panic!("must be in bucket phase"),
        };
        assert_eq!(min, 1);
        assert_eq!(max, 100_000);
        assert_eq!(hist.total_count(), 100_000);
    }
}
