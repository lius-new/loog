use crate::domain::application_layout::{Content, DropPlan, Placement, RegionId, Visibility};
use lgui::core::{CursorIcon, Observable, Point, Signal};
use lgui::prelude::UiRect;
use std::sync::Arc;
#[derive(Clone, Debug, PartialEq)]
pub struct Zone {
    pub rect: UiRect,
    pub placement: Placement,
    /// A cell's body, as opposed to its edge, divider or window bands.
    pub center: bool,
    pub plan: Option<DropPlan>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct MapScene {
    pub version: u64,
    pub viewport: UiRect,
    pub visibility: Visibility,
    pub sessions: Vec<u64>,
    pub cells: Vec<(RegionId, Content, UiRect)>,
    pub zones: Vec<Zone>,
}
/// Horizontal slots of the pressed tab strip, so a drag that stays inside it
/// reorders tabs instead of opening the layout map.
#[derive(Clone, Debug, PartialEq)]
pub struct StripDrag {
    pub rect: UiRect,
    pub viewport: UiRect,
    /// Session id with the left and right edge of its tab, in strip order.
    pub tabs: Arc<[(u64, f32, f32)]>,
}
/// Vertical slack around the strip before a drag counts as leaving it.
const STRIP_SLACK: f32 = crate::ui::tabs::DRAG_REORDER_SLACK;
impl StripDrag {
    pub fn holds(&self, point: Point) -> bool {
        point.x >= self.rect.left
            && point.x < self.rect.right
            && point.y >= self.rect.top - STRIP_SLACK
            && point.y < self.rect.bottom + STRIP_SLACK
    }
    /// Final index of `session` when dropped at `x`: how many other tabs have
    /// their centre left of the pointer.
    pub fn target(&self, source: usize, x: f32) -> usize {
        let crossed = self
            .tabs
            .partition_point(|(_, left, right)| (left + right) / 2.0 < x);
        crossed - usize::from(source < crossed)
    }
    /// Where the insertion marker sits for a final index of `session`, on the
    /// edge the tab would move across. None when the tab would stay where it is.
    pub fn marker_x(&self, session: u64, index: usize) -> Option<f32> {
        let from = self.tabs.iter().position(|(id, _, _)| *id == session)?;
        let (_, left, right) = *self.tabs.get(index)?;
        match index.cmp(&from) {
            std::cmp::Ordering::Equal => None,
            std::cmp::Ordering::Less => Some(left),
            std::cmp::Ordering::Greater => Some(right),
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct TerminalDrag {
    pub session: u64,
    pub strip: Option<StripDrag>,
    /// Final tab index while the drag reorders within its own strip.
    pub reorder: Option<usize>,
    pub floating: bool,
    pub grab_x: f32,
    pub source_index: usize,
    pub origin: Point,
    pub point: Point,
    pub active: bool,
    pub scene: Option<MapScene>,
    pub hover: Option<usize>,
    /// Validate the framework's hover state on the release frame. Its cancel
    /// event is also delivered as PointerUp, but clears all hovered nodes.
    pub release: Option<Point>,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FloatingTab {
    pub strip: UiRect,
    pub session: u64,
    pub index: usize,
    pub grab_x: f32,
}

/// What subscribers observe: a change counter and the cursor it implies.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapSignal {
    pub revision: u64,
    pub cursor: CursorIcon,
    pub floating: Option<FloatingTab>,
}

use crate::domain::application_layout::Divider;
pub enum MapCommand {
    Begin {
        session: u64,
        point: Point,
        strip: Option<StripDrag>,
    },
    Pointer {
        point: Point,
        viewport: UiRect,
    },
    Release {
        point: Point,
        viewport: UiRect,
    },
    Finish {
        point: Point,
        viewport: UiRect,
        hovered: bool,
    },
    Cancel,
    DividerStart(Divider),
    DividerDrag(Point),
    DividerEnd,
}
#[derive(Default)]
pub struct MapOutcome {
    pub consumed: bool,
    pub focus: bool,
}
type Inspect = dyn Fn(&mut dyn FnMut(Option<&TerminalDrag>)) + Send + Sync;
#[derive(Clone)]
pub struct MapBinding {
    inspect: Arc<Inspect>,
    signal: Arc<dyn Fn() -> Observable<MapSignal> + Send + Sync>,
    pointer_x: Option<Signal<f32>>,
    run: Arc<dyn Fn(MapCommand) -> MapOutcome + Send + Sync>,
    sash: Arc<dyn Fn(&Divider) -> bool + Send + Sync>,
}
impl MapBinding {
    pub fn new(
        inspect: impl Fn(&mut dyn FnMut(Option<&TerminalDrag>)) + Send + Sync + 'static,
        signal: impl Fn() -> Observable<MapSignal> + Send + Sync + 'static,
        pointer_x: Option<Signal<f32>>,
        run: impl Fn(MapCommand) -> MapOutcome + Send + Sync + 'static,
        sash: impl Fn(&Divider) -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            inspect: Arc::new(inspect),
            signal: Arc::new(signal),
            pointer_x,
            run: Arc::new(run),
            sash: Arc::new(sash),
        }
    }
    pub fn with<R>(&self, read: impl FnOnce(Option<&TerminalDrag>) -> R) -> R {
        let mut read = Some(read);
        let mut result = None;
        (self.inspect)(&mut |drag| {
            result = Some(read.take().expect("read projection called twice")(drag))
        });
        result.expect("read projection was called")
    }
    pub fn snapshot(&self) -> Option<TerminalDrag> {
        self.with(|d| d.cloned())
    }
    pub fn observable(&self) -> Observable<MapSignal> {
        (self.signal)()
    }
    pub fn pointer_x(&self) -> Option<Signal<f32>> {
        self.pointer_x.clone()
    }
    pub fn is_active(&self) -> bool {
        self.with(|d| d.is_some_and(|d| d.active))
    }
    pub fn is_dragging(&self) -> bool {
        self.with(|d| d.is_some())
    }
    pub fn cancel(&self) -> bool {
        self.run(MapCommand::Cancel).consumed
    }
    pub fn run(&self, command: MapCommand) -> MapOutcome {
        (self.run)(command)
    }
    pub fn sash_active(&self, divider: &Divider) -> bool {
        (self.sash)(divider)
    }
}
