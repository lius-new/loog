//! Restore and capture the main window's position, size, and maximized state.

#[cfg(target_os = "windows")]
use std::sync::atomic::AtomicIsize;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};

use lgui::core::Size;
use lgui::{WindowMode, WindowOptions, WindowPosition};

use crate::domain::window_geometry::WindowGeometry;

const DEFAULT_WIDTH: f32 = 900.0;
const DEFAULT_HEIGHT: f32 = 600.0;
const MIN_WIDTH: f32 = 480.0;
const MIN_HEIGHT: f32 = 320.0;
const MAX_DIMENSION: f32 = 16_384.0;

static WINDOWED_WIDTH: AtomicU32 = AtomicU32::new(DEFAULT_WIDTH.to_bits());
static WINDOWED_HEIGHT: AtomicU32 = AtomicU32::new(DEFAULT_HEIGHT.to_bits());
static WINDOW_X: AtomicI32 = AtomicI32::new(i32::MIN);
static WINDOW_Y: AtomicI32 = AtomicI32::new(i32::MIN);

#[cfg(target_os = "windows")]
static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);

pub fn restore_options(mut options: WindowOptions, saved: Option<WindowGeometry>) -> WindowOptions {
    let Some(geometry) = saved else {
        return options;
    };
    if !valid_size(geometry.width, geometry.height) {
        return options;
    }

    remember_windowed_size(geometry.width, geometry.height);
    remember_window_position(geometry.x, geometry.y);
    options = options.size(Size::new(geometry.width, geometry.height));
    if geometry.maximized {
        options = options.mode(WindowMode::Maximized);
    }
    if let (Some(x), Some(y)) = (geometry.x, geometry.y)
        && saved_position_is_visible(x, y)
    {
        options = options.position(WindowPosition::Absolute { x, y });
    }
    options
}

/// Keep the latest logical content size in memory. Disk writes happen on
/// focus loss or close rather than for every intermediate resize frame.
pub fn observe_viewport(width: f32, height: f32) {
    if !valid_size(width, height) {
        return;
    }

    #[cfg(target_os = "windows")]
    if let Some(hwnd) = main_window() {
        // Maximized and minimized client sizes are not the user's normal
        // windowed size. Preserve the last normal dimensions instead.
        if unsafe { windows_sys::Win32::UI::WindowsAndMessaging::IsZoomed(hwnd) } != 0
            || unsafe { windows_sys::Win32::UI::WindowsAndMessaging::IsIconic(hwnd) } != 0
        {
            return;
        }
        remember_native_position(hwnd);
    }

    remember_windowed_size(width, height);
}

/// lgui does not report maximize changes, so query the native window directly.
/// Toggling maximize resizes the viewport, which re-renders the caller.
pub fn is_maximized() -> bool {
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::IsZoomed;
        main_window().is_some_and(|hwnd| unsafe { IsZoomed(hwnd) } != 0)
    }
    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

/// A minimized window still renders at a tiny client size; callers use this to
/// avoid treating that size as a real layout.
pub fn is_minimized() -> bool {
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::IsIconic;
        main_window().is_some_and(|hwnd| unsafe { IsIconic(hwnd) } != 0)
    }
    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

pub fn remember_native_window() {
    #[cfg(target_os = "windows")]
    let _ = main_window();
}

pub fn capture(previous: Option<WindowGeometry>) -> WindowGeometry {
    let mut geometry = previous.unwrap_or(WindowGeometry {
        x: None,
        y: None,
        width: windowed_width(),
        height: windowed_height(),
        maximized: false,
    });

    geometry.width = windowed_width();
    geometry.height = windowed_height();
    let (x, y) = window_position();
    geometry.x = x.or(geometry.x);
    geometry.y = y.or(geometry.y);

    #[cfg(target_os = "windows")]
    if let Some(hwnd) = main_window() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{IsIconic, IsZoomed};

        let minimized = unsafe { IsIconic(hwnd) } != 0;
        let maximized = unsafe { IsZoomed(hwnd) } != 0;
        if !minimized {
            geometry.maximized = maximized;
        }
        if !minimized && !maximized {
            remember_native_position(hwnd);
            let (x, y) = window_position();
            geometry.x = x;
            geometry.y = y;
        }
    }

    geometry
}

fn remember_windowed_size(width: f32, height: f32) {
    WINDOWED_WIDTH.store(width.to_bits(), Ordering::Relaxed);
    WINDOWED_HEIGHT.store(height.to_bits(), Ordering::Relaxed);
}

fn windowed_width() -> f32 {
    f32::from_bits(WINDOWED_WIDTH.load(Ordering::Relaxed))
}

fn windowed_height() -> f32 {
    f32::from_bits(WINDOWED_HEIGHT.load(Ordering::Relaxed))
}

fn remember_window_position(x: Option<i32>, y: Option<i32>) {
    WINDOW_X.store(x.unwrap_or(i32::MIN), Ordering::Relaxed);
    WINDOW_Y.store(y.unwrap_or(i32::MIN), Ordering::Relaxed);
}

fn window_position() -> (Option<i32>, Option<i32>) {
    let x = WINDOW_X.load(Ordering::Relaxed);
    let y = WINDOW_Y.load(Ordering::Relaxed);
    ((x != i32::MIN).then_some(x), (y != i32::MIN).then_some(y))
}

fn valid_size(width: f32, height: f32) -> bool {
    width.is_finite()
        && height.is_finite()
        && (MIN_WIDTH..=MAX_DIMENSION).contains(&width)
        && (MIN_HEIGHT..=MAX_DIMENSION).contains(&height)
}

#[cfg(target_os = "windows")]
fn main_window() -> Option<windows_sys::Win32::Foundation::HWND> {
    use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

    let stored = MAIN_HWND.load(Ordering::Relaxed) as windows_sys::Win32::Foundation::HWND;
    if !stored.is_null() && window_belongs_to_current_process(stored) {
        return Some(stored);
    }

    let foreground = unsafe { GetForegroundWindow() };
    if foreground.is_null() || !window_belongs_to_current_process(foreground) {
        return None;
    }
    MAIN_HWND.store(foreground as isize, Ordering::Relaxed);
    Some(foreground)
}

#[cfg(target_os = "windows")]
fn window_belongs_to_current_process(hwnd: windows_sys::Win32::Foundation::HWND) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;

    let mut process_id = 0;
    unsafe { GetWindowThreadProcessId(hwnd, &mut process_id) };
    process_id == std::process::id()
}

#[cfg(target_os = "windows")]
fn remember_native_position(hwnd: windows_sys::Win32::Foundation::HWND) {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect;

    let mut rect = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut rect) } != 0 {
        remember_window_position(Some(rect.left), Some(rect.top));
    }
}

#[cfg(target_os = "windows")]
fn saved_position_is_visible(x: i32, y: i32) -> bool {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromRect};

    let probe = RECT {
        left: x,
        top: y,
        right: x.saturating_add(64),
        bottom: y.saturating_add(64),
    };
    !unsafe { MonitorFromRect(&probe, MONITOR_DEFAULTTONULL) }.is_null()
}

#[cfg(not(target_os = "windows"))]
fn saved_position_is_visible(_x: i32, _y: i32) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_implausible_saved_window_sizes() {
        assert!(valid_size(900.0, 600.0));
        assert!(!valid_size(200.0, 600.0));
        assert!(!valid_size(900.0, f32::NAN));
        assert!(!valid_size(MAX_DIMENSION + 1.0, 600.0));
    }
}
