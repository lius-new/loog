use std::{
    collections::HashMap,
    fmt,
    sync::Arc,
    time::{Duration, Instant},
};

#[cfg(feature = "renderer-skia")]
use std::num::NonZeroU32;

use softbuffer::Context as SoftContext;
#[cfg(feature = "renderer-skia")]
use softbuffer::Surface as SoftSurface;
use winit::{
    application::ApplicationHandler,
    dpi::{PhysicalPosition, PhysicalSize as WinitPhysicalSize},
    event::{
        ElementState, Ime, MouseButton, MouseScrollDelta, TouchPhase as WinitTouchPhase,
        WindowEvent,
    },
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy, OwnedDisplayHandle},
    keyboard::{
        Key as WinitKey, KeyLocation as WinitKeyLocation, ModifiersState,
        PhysicalKey as WinitPhysicalKey,
    },
    window::{
        CursorIcon as WinitCursorIcon, Fullscreen, ImePurpose, ResizeDirection, Window,
        WindowAttributes, WindowId as WinitWindowId, WindowLevel,
    },
};

use lgui_core::{
    application::{
        AppView, ApplicationBackend, ApplicationContext, ApplicationHandle, ApplicationTask,
    },
    core::{
        CursorIcon, ImeEvent, InputEvent, KeyLocation, KeyModifiers, KeyState, KeyboardEvent,
        LogicalKey, PhysicalKey, PhysicalPoint, PhysicalRect, PhysicalSize, Point, PointerButton,
        PointerData, PointerId, PointerKind, TouchPhase, UiRect, UiScale, WheelDelta,
        dispatch_runtime_output,
    },
    platform::dpi::{BASE_DPI, ScaleContext, WorkArea},
    session::UiSession,
    window::{ClosePolicy, WindowId, WindowMode, WindowOptions, WindowPosition},
};
#[cfg(feature = "diagnostics")]
use lgui_diagnostics::{
    DiagnosticPresentMode, DiagnosticsRegistration, FramePresentMetrics, FrameRenderMetrics,
    FrameSample,
};
#[cfg(all(feature = "tray-win32", target_os = "windows"))]
use lgui_services::{TrayRegistration, dispatch_tray_action};

use lgui_core::backend::{WindowCommand, application_root_view};
pub use lgui_render_api::GraphicsPreference;
use lgui_render_api::{FrameInfo, FrameReason, MemoryPressure};
#[cfg(feature = "renderer-skia")]
use lgui_render_skia::SkiaSoftwareSurface;

#[cfg(feature = "accessibility")]
#[path = "accessibility.rs"]
mod winit_accessibility;
#[cfg(feature = "renderer-skia-gl")]
#[allow(unsafe_code)]
#[path = "surface/gl.rs"]
mod winit_skia_gl;
#[cfg(all(feature = "renderer-skia-metal", target_os = "macos"))]
#[allow(unsafe_code)]
#[path = "surface/metal.rs"]
mod winit_skia_metal;
#[cfg(all(
    feature = "renderer-skia-vulkan",
    any(target_os = "windows", target_os = "linux")
))]
#[allow(unsafe_code)]
#[path = "surface/vulkan.rs"]
mod winit_skia_vulkan;
#[cfg(target_os = "windows")]
#[path = "windows.rs"]
mod winit_windows;

mod application;
mod event_loop;
mod input;
mod renderer;
mod window;
mod window_options;

pub use application::{WinitApplication, WinitApplicationError};
use event_loop::WinitHost;
pub(crate) use event_loop::WinitUserEvent;
use input::*;
#[cfg(any(
    feature = "renderer-skia-gl",
    feature = "renderer-skia-vulkan",
    feature = "renderer-skia-metal"
))]
pub(crate) use renderer::WinitFrameTimings;
use renderer::*;
use window::*;
pub use window_options::{WinitIconError, WinitWindowIcon, WinitWindowOptions};

#[path = "winit_test.rs"]
#[cfg(test)]
mod tests;
