use std::any::Any;
use std::time::{Duration, Instant};

use lgui::core::{ComponentState, ElementRenderCx, UiId};

const HOVER_DELAY: Duration = Duration::from_millis(500);

pub(crate) fn is_hovered(cx: &ElementRenderCx<'_, '_, '_>, tab: &UiId, close: &UiId) -> bool {
    let tab = cx.context.interaction_flags(tab);
    let close = cx.context.interaction_flags(close);
    (tab.hovered || close.hovered) && !tab.pressed && !close.pressed
}

pub(crate) fn ready(cx: &ElementRenderCx<'_, '_, '_>, hovered: Option<UiId>) -> Option<UiId> {
    cx.context
        .component_state_mut(&cx.id, |state: &mut HoverDelay| {
            if state.target != hovered {
                state.show_at = hovered.as_ref().map(|_| Instant::now() + HOVER_DELAY);
                state.target = hovered;
            }
            if state.show_at.is_none() {
                return state.target.clone();
            }
            None
        })
}

#[derive(Clone, Default)]
struct HoverDelay {
    target: Option<UiId>,
    show_at: Option<Instant>,
}

impl ComponentState for HoverDelay {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn advance(&mut self, _elapsed_ms: f32) -> bool {
        if self
            .show_at
            .is_some_and(|deadline| deadline <= Instant::now())
        {
            self.show_at = None;
            return true;
        }
        false
    }

    fn wants_frame(&self) -> bool {
        self.show_at.is_some()
    }

    fn frame_interval_ms(&self) -> u64 {
        self.show_at.map_or(1, |deadline| {
            let remaining = deadline.saturating_duration_since(Instant::now());
            remaining.as_nanos().div_ceil(1_000_000).max(1) as u64
        })
    }
}
