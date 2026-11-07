//! What Loog keeps in lgui's render caches.
//!
//! lgui stores rasterized images and shaped text but leaves the decision of
//! what to keep to the application. Loog keeps a cache's size proportional to
//! what the latest frame used: every frame that adds entries also removes the
//! least recently used ones, so the level follows the screen instead of
//! everything ever shown, and stays there once drawing stops.

use lgui::memory::{CacheEvent, CacheObserver, CacheView, EntryId, EntryInfo};

/// Bytes always kept so small, static screens never churn.
const FLOOR_BYTES: usize = 2 * 1024 * 1024;
/// Allowed size relative to the bytes the latest frame used, so scrolling back
/// by a screen or two still hits the cache.
const WORKING_SET_FACTOR: usize = 3;

pub(crate) struct RenderCachePolicy;

impl CacheObserver for RenderCachePolicy {
    fn on_event(&self, event: CacheEvent, cache: &mut dyn CacheView) {
        if matches!(event, CacheEvent::FrameEnd | CacheEvent::Requested) {
            trim_to_working_set(cache);
        }
    }
}

fn trim_to_working_set(cache: &mut dyn CacheView) {
    let resident = cache.usage().resident_bytes();
    if resident <= FLOOR_BYTES {
        return;
    }
    let generation = cache.generation();
    let mut used_now = 0usize;
    let mut entries = Vec::new();
    cache.visit_entries(&mut |entry: &EntryInfo| {
        if entry.last_used == generation {
            used_now = used_now.saturating_add(entry.bytes);
        }
        entries.push((entry.last_used, entry.id, entry.bytes));
    });
    let target = FLOOR_BYTES.max(used_now.saturating_mul(WORKING_SET_FACTOR));
    if resident <= target {
        return;
    }
    // Oldest first, until the cache is back at the target.
    entries.sort_unstable_by_key(|(last_used, ..)| *last_used);
    let mut excess = resident - target;
    let mut evict: Vec<EntryId> = Vec::new();
    for (_, id, bytes) in entries {
        if excess == 0 {
            break;
        }
        evict.push(id);
        excess = excess.saturating_sub(bytes);
    }
    cache.evict(&evict);
}

#[cfg(test)]
mod tests {
    use super::*;
    use lgui::memory::{CachePriority, CacheUsage, RetentionClass, ShrinkLevel};

    struct FakeCache {
        generation: u64,
        entries: Vec<EntryInfo>,
    }

    impl FakeCache {
        fn with(entries: &[(u64, u64, usize)], generation: u64) -> Self {
            Self {
                generation,
                entries: entries
                    .iter()
                    .map(|&(id, last_used, bytes)| EntryInfo {
                        id: EntryId(id),
                        bytes,
                        last_used,
                        retention: RetentionClass::Session,
                        priority: CachePriority::Normal,
                    })
                    .collect(),
            }
        }

        fn ids(&self) -> Vec<u64> {
            let mut ids: Vec<u64> = self.entries.iter().map(|entry| entry.id.0).collect();
            ids.sort_unstable();
            ids
        }
    }

    impl CacheView for FakeCache {
        fn name(&self) -> &'static str {
            "fake"
        }

        fn usage(&self) -> CacheUsage {
            CacheUsage {
                cache_bytes: self.entries.iter().map(|entry| entry.bytes).sum(),
                entries: self.entries.len(),
                ..CacheUsage::default()
            }
        }

        fn generation(&self) -> u64 {
            self.generation
        }

        fn visit_entries(&self, visit: &mut dyn FnMut(&EntryInfo)) {
            self.entries.iter().for_each(visit);
        }

        fn evict(&mut self, ids: &[EntryId]) -> usize {
            let before: usize = self.entries.iter().map(|entry| entry.bytes).sum();
            self.entries.retain(|entry| !ids.contains(&entry.id));
            before - self.entries.iter().map(|entry| entry.bytes).sum::<usize>()
        }

        fn shrink(&mut self, _level: ShrinkLevel) -> usize {
            0
        }
    }

    const MIB: usize = 1024 * 1024;

    #[test]
    fn a_small_cache_is_left_alone() {
        let mut cache = FakeCache::with(&[(1, 0, MIB), (2, 5, MIB / 2)], 9);
        RenderCachePolicy.on_event(CacheEvent::FrameEnd, &mut cache);
        assert_eq!(cache.ids(), [1, 2]);
    }

    #[test]
    fn the_oldest_entries_go_first_until_the_working_set_fits() {
        // Latest frame (generation 10) used 2 MiB, so up to 6 MiB may stay.
        let mut cache = FakeCache::with(
            &[
                (1, 1, 4 * MIB),
                (2, 3, 3 * MIB),
                (3, 8, 2 * MIB),
                (4, 10, 2 * MIB),
            ],
            10,
        );
        RenderCachePolicy.on_event(CacheEvent::FrameEnd, &mut cache);
        assert_eq!(cache.ids(), [3, 4]);
    }

    #[test]
    fn entries_of_the_latest_frame_are_never_the_first_to_go() {
        let mut cache = FakeCache::with(&[(1, 10, MIB), (2, 2, 9 * MIB)], 10);
        RenderCachePolicy.on_event(CacheEvent::FrameEnd, &mut cache);
        assert_eq!(cache.ids(), [1]);
    }

    #[test]
    fn inserts_alone_do_not_trigger_a_pass() {
        let mut cache = FakeCache::with(&[(1, 0, 20 * MIB), (2, 10, MIB)], 10);
        RenderCachePolicy.on_event(CacheEvent::Inserted(EntryId(2)), &mut cache);
        assert_eq!(cache.ids(), [1, 2]);
    }
}
