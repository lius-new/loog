// Shared measurement helper for the current and archived-source test harnesses.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Memory {
    pub resident_bytes: usize,
    pub private_bytes: usize,
    pub process_peak_resident_bytes: usize,
}
#[cfg(windows)]
pub fn snapshot() -> Option<Memory> {
    #[repr(C)]
    #[derive(Default)]
    struct Counters {
        cb: u32,
        faults: u32,
        peak_working: usize,
        working: usize,
        peak_paged: usize,
        paged: usize,
        peak_nonpaged: usize,
        nonpaged: usize,
        pagefile: usize,
        peak_pagefile: usize,
        private: usize,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        #[allow(clashing_extern_declarations)]
        fn K32GetProcessMemoryInfo(
            process: *mut std::ffi::c_void,
            counters: *mut Counters,
            size: u32,
        ) -> i32;
    }
    let mut counters = Counters {
        cb: std::mem::size_of::<Counters>() as u32,
        ..Default::default()
    };
    // Windows' documented current-process pseudo handle; no handle is opened.
    if unsafe {
        K32GetProcessMemoryInfo(
            -1isize as *mut _,
            &mut counters,
            std::mem::size_of::<Counters>() as u32,
        )
    } == 0
    {
        return None;
    }
    Some(Memory {
        resident_bytes: counters.working,
        private_bytes: counters.private,
        process_peak_resident_bytes: counters.peak_working,
    })
}
#[cfg(not(windows))]
pub fn snapshot() -> Option<Memory> {
    None
}
pub fn sample(peak: &mut Option<Memory>) {
    if let Some(next) = snapshot() {
        if let Some(peak) = peak {
            peak.resident_bytes = peak.resident_bytes.max(next.resident_bytes);
            peak.private_bytes = peak.private_bytes.max(next.private_bytes);
            peak.process_peak_resident_bytes = peak
                .process_peak_resident_bytes
                .max(next.process_peak_resident_bytes);
        } else {
            *peak = Some(next);
        }
    }
}
