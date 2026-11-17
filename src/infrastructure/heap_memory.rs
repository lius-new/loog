//! Return allocator scratch capacity after the startup allocation burst.

#[cfg(windows)]
pub(crate) fn release_startup_scratch() {
    use windows_sys::Win32::System::{
        Memory::{GetProcessHeap, HeapOptimizeResources, HeapSetInformation},
        SystemServices::{
            HEAP_OPTIMIZE_RESOURCES_CURRENT_VERSION, HEAP_OPTIMIZE_RESOURCES_INFORMATION,
        },
    };
    let information = HEAP_OPTIMIZE_RESOURCES_INFORMATION {
        Version: HEAP_OPTIMIZE_RESOURCES_CURRENT_VERSION,
        Flags: 0,
    };
    // Only the allocator's unused LFH caches are eligible for decommit. Live
    // buffers, shaped text, glyph caches and executable pages remain untouched.
    // Run once after startup, never on the editing/rendering hot path.
    unsafe {
        let _ = HeapSetInformation(
            GetProcessHeap(),
            HeapOptimizeResources,
            (&information as *const HEAP_OPTIMIZE_RESOURCES_INFORMATION).cast(),
            std::mem::size_of_val(&information),
        );
    }
}
