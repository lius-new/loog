//! Application adapters for frame notification and themed terminal display.
use crate::features::terminal::TerminalSnapshot;
use crate::features::terminal::screen::Palette;
use crate::infrastructure::terminal::notification::FrameNotifier;
use crate::infrastructure::terminal::session::TerminalController;
use lgui::ApplicationHandle;
use std::sync::Arc;
impl From<Arc<ApplicationHandle>> for FrameNotifier {
    fn from(application: Arc<ApplicationHandle>) -> Self {
        Self::new(move || application.request_frame())
    }
}
impl From<&Arc<ApplicationHandle>> for FrameNotifier {
    fn from(application: &Arc<ApplicationHandle>) -> Self {
        Self::from(application.clone())
    }
}
impl From<&ApplicationHandle> for FrameNotifier {
    fn from(application: &ApplicationHandle) -> Self {
        Self::from(Arc::new(application.clone()))
    }
}
pub(crate) trait TerminalDisplay {
    fn snapshot(&self) -> TerminalSnapshot;
}
impl TerminalDisplay for TerminalController {
    fn snapshot(&self) -> TerminalSnapshot {
        let palette = Palette {
            ansi: crate::ui::theme::c().ansi,
            ansi_backgrounds: crate::ui::theme::c().ansi_backgrounds,
            foreground: crate::ui::theme::c().text.0,
            background: crate::ui::theme::c().bg.0,
        };
        self.snapshot_with_palette(&palette)
    }
}

impl TerminalDisplay for crate::infrastructure::terminal::runtime::TerminalHandle {
    fn snapshot(&self) -> TerminalSnapshot {
        let palette = Palette {
            ansi: crate::ui::theme::c().ansi,
            ansi_backgrounds: crate::ui::theme::c().ansi_backgrounds,
            foreground: crate::ui::theme::c().text.0,
            background: crate::ui::theme::c().bg.0,
        };
        self.snapshot_with_palette(&palette)
    }
}
