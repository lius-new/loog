//! Native file drops until lgui exposes its platform file events to applications.
use std::path::PathBuf;

pub fn install(
    logical_width: f32,
    handler: impl Fn(Vec<PathBuf>, (f32, f32)) + 'static,
) -> impl FnOnce() {
    #[cfg(windows)]
    {
        let subscription = windows::install(logical_width, handler);
        move || drop(subscription)
    }
    #[cfg(not(windows))]
    {
        let _ = (logical_width, handler);
        || {}
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::{cell::RefCell, collections::HashMap, os::windows::ffi::OsStringExt, rc::Rc};
    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        System::{Ole::RevokeDragDrop, Threading::GetCurrentThreadId},
        UI::{
            Shell::{
                DefSubclassProc, DragAcceptFiles, DragFinish, DragQueryFileW, DragQueryPoint,
                RemoveWindowSubclass, SetWindowSubclass,
            },
            WindowsAndMessaging::{
                EnumThreadWindows, GetClientRect, GetWindowTextW, WM_DROPFILES, WM_NCDESTROY,
            },
        },
    };

    type Handler = Rc<dyn Fn(Vec<PathBuf>, (f32, f32))>;
    thread_local! {
        static HANDLERS: RefCell<HashMap<usize, Handler>> = RefCell::new(HashMap::new());
    }
    const SUBCLASS: usize = 0x4c4f4f4d;

    pub struct Subscription(HWND);

    impl Drop for Subscription {
        fn drop(&mut self) {
            let removed =
                HANDLERS.with(|handlers| handlers.borrow_mut().remove(&(self.0 as usize)));
            if removed.is_some() {
                // Installed and removed by the window's UI thread.
                unsafe {
                    DragAcceptFiles(self.0, 0);
                    RemoveWindowSubclass(self.0, Some(window_proc), SUBCLASS);
                }
            }
        }
    }

    pub fn install(
        logical_width: f32,
        handler: impl Fn(Vec<PathBuf>, (f32, f32)) + 'static,
    ) -> Subscription {
        let mut hwnd: HWND = std::ptr::null_mut();
        unsafe {
            EnumThreadWindows(
                GetCurrentThreadId(),
                Some(find_window),
                &mut hwnd as *mut HWND as isize,
            );
            if !hwnd.is_null() && SetWindowSubclass(hwnd, Some(window_proc), SUBCLASS, 0) != 0 {
                HANDLERS.with(|handlers| {
                    handlers.borrow_mut().insert(
                        hwnd as usize,
                        Rc::new(move |paths, (x, y)| {
                            let mut rect = RECT::default();
                            // Lgui's automatic scale can differ from the OS DPI.
                            // Derive its actual scale from the rendered viewport.
                            if GetClientRect(hwnd, &mut rect) != 0
                                && rect.right > 0
                                && logical_width > 0.0
                            {
                                let scale = rect.right as f32 / logical_width;
                                handler(paths, (x / scale, y / scale));
                            }
                        }),
                    )
                });
                // Winit's OLE target consumes drops before WM_DROPFILES. Use the
                // shell's file-drop fallback, which also supplies the exact point.
                RevokeDragDrop(hwnd);
                DragAcceptFiles(hwnd, 1);
            }
        }
        Subscription(hwnd)
    }

    unsafe extern "system" fn find_window(hwnd: HWND, output: LPARAM) -> i32 {
        let mut title = [0u16; 16];
        let len = unsafe { GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32) };
        if "Loog"
            .encode_utf16()
            .eq(title[..len as usize].iter().copied())
        {
            unsafe { *(output as *mut HWND) = hwnd };
            return 0;
        }
        1
    }

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _: usize,
        _: usize,
    ) -> LRESULT {
        if message == WM_DROPFILES {
            let drop = wparam as *mut std::ffi::c_void;
            let mut point = POINT::default();
            let client = unsafe { DragQueryPoint(drop, &mut point) } != 0;
            let count = unsafe { DragQueryFileW(drop, u32::MAX, std::ptr::null_mut(), 0) };
            let mut paths = Vec::new();
            for index in 0..count {
                let len = unsafe { DragQueryFileW(drop, index, std::ptr::null_mut(), 0) };
                let mut name = vec![0u16; len as usize + 1];
                let copied = unsafe { DragQueryFileW(drop, index, name.as_mut_ptr(), len + 1) };
                if copied > 0 {
                    paths.push(PathBuf::from(std::ffi::OsString::from_wide(
                        &name[..copied as usize],
                    )));
                }
            }
            unsafe { DragFinish(drop) };
            if client {
                let handler =
                    HANDLERS.with(|handlers| handlers.borrow().get(&(hwnd as usize)).cloned());
                if let Some(handler) = handler {
                    handler(paths, (point.x as f32, point.y as f32));
                }
            }
            return 0;
        }
        if message == WM_NCDESTROY {
            HANDLERS.with(|handlers| handlers.borrow_mut().remove(&(hwnd as usize)));
            unsafe { RemoveWindowSubclass(hwnd, Some(window_proc), SUBCLASS) };
        }
        unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use windows_sys::Win32::{
            System::Memory::{GHND, GlobalAlloc, GlobalLock, GlobalUnlock},
            UI::{
                Shell::DROPFILES,
                WindowsAndMessaging::{
                    CreateWindowExW, DestroyWindow, SendMessageW, WS_OVERLAPPEDWINDOW,
                },
            },
        };

        struct TestWindow(HWND);
        impl Drop for TestWindow {
            fn drop(&mut self) {
                unsafe {
                    DestroyWindow(self.0);
                }
            }
        }

        fn send_drop(hwnd: HWND, non_client: bool) {
            let names: Vec<u16> = "C:\\资料\\比赛.txt\0C:\\folder with spaces\\second.rs\0\0"
                .encode_utf16()
                .collect();
            unsafe {
                let size = std::mem::size_of::<DROPFILES>();
                let memory = GlobalAlloc(GHND, size + names.len() * 2);
                assert!(!memory.is_null());
                let data = GlobalLock(memory).cast::<u8>();
                assert!(!data.is_null());
                data.cast::<DROPFILES>().write_unaligned(DROPFILES {
                    pFiles: size as u32,
                    pt: POINT { x: 100, y: 50 },
                    fNC: non_client as i32,
                    fWide: 1,
                });
                std::ptr::copy_nonoverlapping(
                    names.as_ptr().cast::<u8>(),
                    data.add(size),
                    names.len() * 2,
                );
                GlobalUnlock(memory);
                // The receiving window owns the HDROP and releases it with DragFinish.
                SendMessageW(hwnd, WM_DROPFILES, memory as usize, 0);
            }
        }

        #[test]
        fn native_drop_decodes_unicode_files_scales_the_point_and_cleans_up() {
            let hwnd = unsafe {
                CreateWindowExW(
                    0,
                    windows_sys::core::w!("STATIC"),
                    windows_sys::core::w!("Loog"),
                    WS_OVERLAPPEDWINDOW,
                    0,
                    0,
                    600,
                    400,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                )
            };
            assert!(!hwnd.is_null());
            let window = TestWindow(hwnd);
            let deliveries = Rc::new(RefCell::new(Vec::new()));
            let output = deliveries.clone();
            let subscription = install(300.0, move |paths, point| {
                output.borrow_mut().push((paths, point))
            });
            assert_eq!(subscription.0, hwnd);
            send_drop(hwnd, false);
            send_drop(hwnd, true);
            let deliveries = deliveries.borrow();
            assert_eq!(deliveries.len(), 1);
            assert_eq!(
                deliveries[0].0,
                [
                    PathBuf::from("C:\\资料\\比赛.txt"),
                    PathBuf::from("C:\\folder with spaces\\second.rs")
                ]
            );
            let mut client = RECT::default();
            unsafe {
                GetClientRect(hwnd, &mut client);
            }
            let scale = client.right as f32 / 300.0;
            assert_eq!(deliveries[0].1, (100.0 / scale, 50.0 / scale));
            // Destruction can precede effect cleanup during application shutdown.
            drop(window);
            assert!(HANDLERS.with(|handlers| handlers.borrow().is_empty()));
            drop(subscription);
        }
    }
}
