//! A small-object allocator for Windows.
//!
//! The system heap keeps a partly used bucket for every size it has ever served, so an
//! application whose small allocations come in many different sizes (strings, vectors, nodes)
//! holds far more committed memory than it uses. This allocator serves blocks of up to 1 KiB from
//! 16 KiB pages of one reserved region, in 20 size classes, with no per-block header (Rust gives
//! the size back on `dealloc`). A page is committed when a class needs one and decommitted as soon
//! as it is empty. Everything else (larger blocks, over-aligned blocks, or an exhausted region)
//! goes to the system allocator.
use std::alloc::{GlobalAlloc, Layout, System};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};

const REGION_BYTES: usize = 256 << 20;
const PAGE_SHIFT: usize = 14;
const PAGE_BYTES: usize = 1 << PAGE_SHIFT;
const PAGES: usize = REGION_BYTES / PAGE_BYTES;
const MAX_SMALL: usize = 16384;
const NONE: u32 = u32::MAX;

const CLASSES: usize = 36;

/// Block sizes: 16-byte steps to 128, then 32 to 256, 64 to 512, 128 to 1024, and quarter steps of
/// each power of two up to 16 KiB.
const CLASS_SIZES: [u32; CLASSES] = [
    16, 32, 48, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 640, 768, 896, 1024,
    1280, 1536, 1792, 2048, 2560, 3072, 3584, 4096, 5120, 6144, 7168, 8192, 10240, 12288, 14336,
    16384,
];

const MEM_COMMIT: u32 = 0x1000;
const MEM_RESERVE: u32 = 0x2000;
const MEM_DECOMMIT: u32 = 0x4000;
const PAGE_NOACCESS: u32 = 0x01;
const PAGE_READWRITE: u32 = 0x04;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn VirtualAlloc(address: *mut c_void, size: usize, kind: u32, protect: u32) -> *mut c_void;
    fn VirtualFree(address: *mut c_void, size: usize, kind: u32) -> i32;
}

#[derive(Clone, Copy)]
struct PageInfo {
    class: u8,
    in_partial: bool,
    /// Blocks in use.
    live: u16,
    /// Next never-used block.
    bump: u16,
    /// Head of the freed blocks, as `index + 1` (0 = none).
    free_head: u16,
    next: u32,
    prev: u32,
}

struct Pool {
    base: usize,
    /// The first page that has never been used.
    untouched: usize,
    /// Pages that were used and are decommitted, ready for reuse.
    free_pages: [u32; PAGES],
    free_page_count: usize,
    pages: [PageInfo; PAGES],
    /// Per class: head of the list of pages with room, and how many there are.
    partial: [u32; CLASSES],
    partial_count: [u32; CLASSES],
    failed: bool,
}

pub struct PoolAlloc;

static LOCK: AtomicBool = AtomicBool::new(false);
static mut POOL: Pool = Pool {
    base: 0,
    untouched: 0,
    free_pages: [0; PAGES],
    free_page_count: 0,
    pages: [PageInfo {
        class: 0,
        in_partial: false,
        live: 0,
        bump: 0,
        free_head: 0,
        next: NONE,
        prev: NONE,
    }; PAGES],
    partial: [NONE; CLASSES],
    partial_count: [0; CLASSES],
    failed: false,
};

struct Guard;

impl Guard {
    fn lock() -> Guard {
        let mut spins = 0u32;
        while LOCK
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            // The critical sections are a few instructions, so a short spin nearly always wins;
            // if the holder was descheduled, give the processor back instead of burning it.
            spins += 1;
            if spins < 64 {
                std::hint::spin_loop();
            } else {
                std::thread::yield_now();
            }
        }
        Guard
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        LOCK.store(false, Ordering::Release);
    }
}

/// Size class of a block of `size` bytes (1 to 1024).
fn class_of(size: usize) -> usize {
    match size {
        0..=128 => (size + 15) / 16 - 1,
        129..=256 => 8 + (size - 129) / 32,
        257..=512 => 12 + (size - 257) / 64,
        513..=1024 => 16 + (size - 513) / 128,
        _ => 20 + CLASS_SIZES[20..].partition_point(|&class| (class as usize) < size),
    }
}

fn capacity(class: usize) -> usize {
    PAGE_BYTES / CLASS_SIZES[class] as usize
}

#[allow(static_mut_refs)]
unsafe fn pool() -> &'static mut Pool {
    unsafe { &mut POOL }
}

impl Pool {
    unsafe fn ensure_region(&mut self) -> bool {
        if self.base != 0 {
            return true;
        }
        if self.failed {
            return false;
        }
        let base = unsafe { VirtualAlloc(std::ptr::null_mut(), REGION_BYTES, MEM_RESERVE, PAGE_NOACCESS) };
        if base.is_null() {
            self.failed = true;
            return false;
        }
        self.base = base as usize;
        true
    }

    fn page_address(&self, page: usize) -> usize {
        self.base + (page << PAGE_SHIFT)
    }

    fn contains(&self, address: usize) -> bool {
        self.base != 0 && address >= self.base && address < self.base + REGION_BYTES
    }

    fn partial_insert(&mut self, page: usize) {
        let class = self.pages[page].class as usize;
        let head = self.partial[class];
        self.pages[page].next = head;
        self.pages[page].prev = NONE;
        if head != NONE {
            self.pages[head as usize].prev = page as u32;
        }
        self.partial[class] = page as u32;
        self.pages[page].in_partial = true;
        self.partial_count[class] += 1;
    }

    fn partial_remove(&mut self, page: usize) {
        let info = self.pages[page];
        let class = info.class as usize;
        if info.prev != NONE {
            self.pages[info.prev as usize].next = info.next;
        } else {
            self.partial[class] = info.next;
        }
        if info.next != NONE {
            self.pages[info.next as usize].prev = info.prev;
        }
        self.pages[page].in_partial = false;
        self.partial_count[class] -= 1;
    }

    /// Commits a page for `class` and puts it on the class's list.
    unsafe fn new_page(&mut self, class: usize) -> Option<usize> {
        let page = if self.free_page_count > 0 {
            self.free_page_count -= 1;
            self.free_pages[self.free_page_count] as usize
        } else if self.untouched < PAGES {
            self.untouched += 1;
            self.untouched - 1
        } else {
            return None;
        };
        let address = self.page_address(page);
        let committed = unsafe { VirtualAlloc(address as *mut c_void, PAGE_BYTES, MEM_COMMIT, PAGE_READWRITE) };
        if committed.is_null() {
            self.free_pages[self.free_page_count] = page as u32;
            self.free_page_count += 1;
            return None;
        }
        self.pages[page] = PageInfo {
            class: class as u8,
            in_partial: false,
            live: 0,
            bump: 0,
            free_head: 0,
            next: NONE,
            prev: NONE,
        };
        self.partial_insert(page);
        Some(page)
    }

    unsafe fn alloc(&mut self, size: usize) -> *mut u8 {
        if !unsafe { self.ensure_region() } {
            return std::ptr::null_mut();
        }
        let class = class_of(size);
        let mut page = self.partial[class];
        if page == NONE {
            match unsafe { self.new_page(class) } {
                Some(created) => page = created as u32,
                None => return std::ptr::null_mut(),
            }
        }
        let page = page as usize;
        let block = CLASS_SIZES[class] as usize;
        let index;
        {
            let info = &mut self.pages[page];
            if info.free_head != 0 {
                index = (info.free_head - 1) as usize;
                let address = self.base + (page << PAGE_SHIFT) + index * block;
                let next = unsafe { *(address as *const u16) };
                self.pages[page].free_head = next;
            } else {
                index = info.bump as usize;
                info.bump += 1;
            }
            self.pages[page].live += 1;
        }
        let info = self.pages[page];
        if info.live as usize == capacity(class) {
            self.partial_remove(page);
        }
        (self.page_address(page) + index * block) as *mut u8
    }

    unsafe fn dealloc(&mut self, address: usize) {
        let page = (address - self.base) >> PAGE_SHIFT;
        let class = self.pages[page].class as usize;
        let block = CLASS_SIZES[class] as usize;
        let index = (address - self.page_address(page)) / block;
        let was_full = self.pages[page].live as usize == capacity(class);
        unsafe { *(address as *mut u16) = self.pages[page].free_head };
        self.pages[page].free_head = (index + 1) as u16;
        self.pages[page].live -= 1;
        if was_full {
            self.partial_insert(page);
        }
        // Give an empty page back, unless it is the class's only page with room (that would make
        // an alloc/free pair at the boundary commit and decommit it over and over).
        if self.pages[page].live == 0 && self.partial_count[class] > 1 {
            self.partial_remove(page);
            let address = self.page_address(page);
            unsafe { VirtualFree(address as *mut c_void, PAGE_BYTES, MEM_DECOMMIT) };
            self.free_pages[self.free_page_count] = page as u32;
            self.free_page_count += 1;
        }
    }
}

fn small(layout: &Layout) -> bool {
    layout.size() <= MAX_SMALL && layout.align() <= 16 && layout.size() > 0
}

unsafe impl GlobalAlloc for PoolAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if small(&layout) {
            let _guard = Guard::lock();
            let ptr = unsafe { pool().alloc(layout.size()) };
            if !ptr.is_null() {
                return ptr;
            }
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if small(&layout) {
            let ptr = unsafe { self.alloc(layout) };
            if !ptr.is_null() {
                unsafe { std::ptr::write_bytes(ptr, 0, layout.size()) };
            }
            return ptr;
        }
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        {
            let _guard = Guard::lock();
            let pool = unsafe { pool() };
            if pool.contains(ptr as usize) {
                unsafe { pool.dealloc(ptr as usize) };
                return;
            }
        }
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let in_pool = {
            let _guard = Guard::lock();
            unsafe { pool() }.contains(ptr as usize)
        };
        if in_pool {
            // Same size class: nothing to move.
            if new_size > 0 && new_size <= MAX_SMALL && class_of(new_size) == class_of(layout.size()) {
                return ptr;
            }
        } else if !(new_size <= MAX_SMALL && layout.align() <= 16) {
            return unsafe { System.realloc(ptr, layout, new_size) };
        }
        let new_layout = unsafe { Layout::from_size_align_unchecked(new_size, layout.align()) };
        let next = unsafe { self.alloc(new_layout) };
        if !next.is_null() {
            unsafe {
                std::ptr::copy_nonoverlapping(ptr, next, layout.size().min(new_size));
                self.dealloc(ptr, layout);
            }
        }
        next
    }
}

/// (committed KB, live KB, committed pages, pages with room) of the pool, for memory tests.
#[allow(dead_code)]
pub fn stats() -> (usize, usize, usize, [usize; CLASSES]) {
    let _guard = Guard::lock();
    let pool = unsafe { pool() };
    let mut live = 0usize;
    let mut committed_pages = 0usize;
    let mut per_class = [0usize; CLASSES];
    for page in 0..pool.untouched {
        // A page on the free-page stack is decommitted.
        let freed = pool.free_pages[..pool.free_page_count].contains(&(page as u32));
        if freed {
            continue;
        }
        committed_pages += 1;
        let info = pool.pages[page];
        live += info.live as usize * CLASS_SIZES[info.class as usize] as usize;
        per_class[info.class as usize] += 1;
    }
    (committed_pages * PAGE_BYTES / 1024, live / 1024, committed_pages, per_class)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(state: &mut u64) -> u64 {
        *state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *state >> 33
    }

    unsafe fn fill(ptr: *mut u8, len: usize, seed: u8) {
        for index in 0..len {
            unsafe { *ptr.add(index) = seed.wrapping_add(index as u8) };
        }
    }

    unsafe fn check(ptr: *const u8, len: usize, seed: u8) -> bool {
        (0..len).all(|index| unsafe { *ptr.add(index) } == seed.wrapping_add(index as u8))
    }

    #[test]
    fn every_class_keeps_its_contents_and_alignment() {
        let alloc = PoolAlloc;
        let mut live: Vec<(*mut u8, Layout, u8)> = Vec::new();
        let mut state = 7u64;
        for round in 0..6000u64 {
            let size = match lcg(&mut state) % 4 {
                0 => 1 + lcg(&mut state) as usize % 128,
                1 => 1 + lcg(&mut state) as usize % 1024,
                2 => 1 + lcg(&mut state) as usize % 16384,
                _ => 16384 + lcg(&mut state) as usize % 30000,
            };
            let align = [1usize, 2, 4, 8, 16][lcg(&mut state) as usize % 5];
            let layout = Layout::from_size_align(size, align).unwrap();
            let ptr = unsafe { alloc.alloc(layout) };
            assert!(!ptr.is_null());
            assert_eq!(ptr as usize % align, 0, "misaligned block of {size}");
            let seed = round as u8;
            unsafe { fill(ptr, size, seed) };
            live.push((ptr, layout, seed));
            // Free about half of what is live, in no particular order.
            if live.len() > 50 && lcg(&mut state) % 2 == 0 {
                let index = lcg(&mut state) as usize % live.len();
                let (ptr, layout, seed) = live.swap_remove(index);
                assert!(unsafe { check(ptr, layout.size(), seed) }, "block of {} was overwritten", layout.size());
                unsafe { alloc.dealloc(ptr, layout) };
            }
        }
        for (ptr, layout, seed) in live {
            assert!(unsafe { check(ptr, layout.size(), seed) });
            unsafe { alloc.dealloc(ptr, layout) };
        }
    }

    #[test]
    fn realloc_preserves_contents_across_every_tier() {
        let alloc = PoolAlloc;
        let sizes = [1usize, 8, 16, 17, 100, 1024, 1025, 3000, 16384, 16385, 40000, 5000, 200, 20, 1];
        let mut layout = Layout::from_size_align(sizes[0], 8).unwrap();
        let mut ptr = unsafe { alloc.alloc(layout) };
        let mut length = sizes[0];
        unsafe { fill(ptr, length, 3) };
        for &next in &sizes[1..] {
            ptr = unsafe { alloc.realloc(ptr, layout, next) };
            assert!(!ptr.is_null());
            let kept = length.min(next);
            assert!(unsafe { check(ptr, kept, 3) }, "contents lost going to {next}");
            layout = Layout::from_size_align(next, 8).unwrap();
            length = next;
            unsafe { fill(ptr, length, 3) };
        }
        unsafe { alloc.dealloc(ptr, layout) };
    }

    #[test]
    fn zeroed_blocks_are_zero_even_when_reusing_dirty_memory() {
        let alloc = PoolAlloc;
        for size in [8usize, 64, 500, 1024, 4000, 16384, 30000] {
            let layout = Layout::from_size_align(size, 8).unwrap();
            let dirty = unsafe { alloc.alloc(layout) };
            unsafe { std::ptr::write_bytes(dirty, 0xAB, size) };
            unsafe { alloc.dealloc(dirty, layout) };
            let zeroed = unsafe { alloc.alloc_zeroed(layout) };
            assert!((0..size).all(|index| unsafe { *zeroed.add(index) } == 0), "size {size}");
            unsafe { alloc.dealloc(zeroed, layout) };
        }
    }

    #[test]
    fn many_threads_allocating_and_freeing_do_not_corrupt_each_other() {
        let threads: Vec<_> = (0..8u64)
            .map(|id| {
                std::thread::spawn(move || {
                    let alloc = PoolAlloc;
                    let mut state = id + 1;
                    let mut live: Vec<(*mut u8, Layout, u8)> = Vec::new();
                    for round in 0..4000u64 {
                        let size = 1 + lcg(&mut state) as usize % 3000;
                        let layout = Layout::from_size_align(size, 8).unwrap();
                        let ptr = unsafe { alloc.alloc(layout) };
                        let seed = (round as u8).wrapping_add(id as u8);
                        unsafe { fill(ptr, size, seed) };
                        live.push((ptr, layout, seed));
                        if live.len() > 30 {
                            let index = lcg(&mut state) as usize % live.len();
                            let (ptr, layout, seed) = live.swap_remove(index);
                            assert!(unsafe { check(ptr, layout.size(), seed) });
                            unsafe { alloc.dealloc(ptr, layout) };
                        }
                    }
                    for (ptr, layout, seed) in live {
                        assert!(unsafe { check(ptr, layout.size(), seed) });
                        unsafe { alloc.dealloc(ptr, layout) };
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
    }

    #[test]
    fn freed_pages_are_reused_and_the_pool_does_not_grow_without_bound() {
        let alloc = PoolAlloc;
        let layout = Layout::from_size_align(200, 8).unwrap();
        // Each round takes about 26 pages. If freed pages were not reused, 50 rounds would add
        // over a thousand; other tests share the pool, so allow generous noise.
        let round = || {
            let blocks: Vec<_> = (0..2000).map(|_| unsafe { alloc.alloc(layout) }).collect();
            for block in blocks {
                unsafe { alloc.dealloc(block, layout) };
            }
        };
        round();
        round();
        let warm = stats().2;
        for _ in 0..50 {
            round();
        }
        let after = stats().2;
        assert!(after < warm + 300, "pages grew from {warm} to {after}");
    }
}
