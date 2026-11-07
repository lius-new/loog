//! Hidden GUI fixture for normal-close, persistence and cleanup regression tests.
#![windows_subsystem = "windows"]
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
type Handle = *mut c_void;
#[repr(C)]
struct WindowClass {
    style: u32,
    procedure: unsafe extern "system" fn(Handle, u32, usize, isize) -> isize,
    class_extra: i32,
    window_extra: i32,
    instance: Handle,
    icon: Handle,
    cursor: Handle,
    background: Handle,
    menu: *const u16,
    name: *const u16,
}
#[repr(C)]
struct Message {
    window: Handle,
    message: u32,
    wparam: usize,
    lparam: isize,
    time: u32,
    x: i32,
    y: i32,
    private: u32,
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(name: *const u16) -> Handle;
}
#[link(name = "user32")]
unsafe extern "system" {
    fn RegisterClassW(class: *const WindowClass) -> u16;
    fn CreateWindowExW(
        ex: u32,
        class: *const u16,
        title: *const u16,
        style: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        parent: Handle,
        menu: Handle,
        instance: Handle,
        parameter: Handle,
    ) -> Handle;
    fn DefWindowProcW(window: Handle, message: u32, wparam: usize, lparam: isize) -> isize;
    fn GetMessageW(message: *mut Message, window: Handle, min: u32, max: u32) -> i32;
    fn TranslateMessage(message: *const Message) -> i32;
    fn DispatchMessageW(message: *const Message) -> isize;
    fn DestroyWindow(window: Handle) -> i32;
    fn PostQuitMessage(code: i32);
}
unsafe extern "system" fn procedure(
    window: Handle,
    message: u32,
    wparam: usize,
    lparam: isize,
) -> isize {
    if message == 0x0010 {
        // WM_CLOSE
        let mode = std::env::var("LOOG_STARTUP_FIXTURE_MODE").unwrap_or_default();
        if mode == "ignore-close" {
            return 0;
        }
        fixture_delay("LOOG_STARTUP_FIXTURE_CLOSE_DELAY_MS");
        if mode != "no-session" {
            let root = std::path::PathBuf::from(std::env::var_os("APPDATA").unwrap()).join("Loog");
            std::fs::create_dir_all(&root).unwrap();
            let workspace = if mode == "wrong-session" {
                "wrong".into()
            } else {
                std::env::args().nth(1).unwrap()
            };
            let workspace = workspace.replace('\\', "\\\\").replace('"', "\\\"");
            std::fs::write(
                root.join("workspace.json"),
                format!("{{\"open_folders\":[\"{workspace}\"]}}"),
            )
            .unwrap();
        }
        unsafe {
            DestroyWindow(window);
        }
        return 0;
    }
    if message == 0x0002 {
        // WM_DESTROY
        let code = if std::env::var("LOOG_STARTUP_FIXTURE_MODE").as_deref() == Ok("bad-exit") {
            7
        } else {
            0
        };
        unsafe {
            PostQuitMessage(code);
        }
        return 0;
    }
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}
fn fixture_delay(name: &str) {
    if let Ok(value) = std::env::var(name) {
        std::thread::sleep(std::time::Duration::from_millis(value.parse().unwrap()));
    }
}
fn main() {
    fixture_delay("LOOG_STARTUP_FIXTURE_START_DELAY_MS");
    if std::env::var("LOOG_STARTUP_FIXTURE_MODE").as_deref() == Ok("early-exit") {
        std::process::exit(3);
    }
    let name: Vec<u16> = std::ffi::OsStr::new("LoogStartupSmokeFixture")
        .encode_wide()
        .chain(Some(0))
        .collect();
    let null = std::ptr::null_mut();
    let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
    let class = WindowClass {
        style: 0,
        procedure,
        class_extra: 0,
        window_extra: 0,
        instance,
        icon: null,
        cursor: null,
        background: null,
        menu: std::ptr::null(),
        name: name.as_ptr(),
    };
    assert_ne!(unsafe { RegisterClassW(&class) }, 0);
    // No WS_VISIBLE and no ShowWindow: the fixture never displays a GUI.
    let window = unsafe {
        CreateWindowExW(
            0,
            name.as_ptr(),
            name.as_ptr(),
            0x00cf0000,
            0,
            0,
            200,
            100,
            null,
            null,
            instance,
            null,
        )
    };
    assert!(!window.is_null());
    let mut message: Message = unsafe { std::mem::zeroed() };
    while unsafe { GetMessageW(&mut message, null, 0, 0) } > 0 {
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    std::process::exit(message.wparam as i32);
}
