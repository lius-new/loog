//! A terminal byte transport independent of process location and protocol.
use crate::features::terminal::TerminalSize;
use std::sync::Arc;

pub(crate) enum Event {
    Started,
    Output(Vec<u8>),
    Directory(Option<loog_workspace_api::ProviderPath>),
    Exited(Option<i32>),
    Failed(String),
}

/// Weak, generation-bound delivery; retaining a sink never keeps a terminal alive.
#[derive(Clone)]
pub(crate) struct Sink(pub Arc<dyn Fn(Event) + Send + Sync>);
impl Sink {
    pub fn send(&self, event: Event) {
        (self.0)(event);
    }
}

pub(crate) trait Transport: Send + Sync {
    fn start(&self, size: TerminalSize, sink: Sink);
    fn write(&self, bytes: &[u8]) -> Result<(), String>;
    fn resize(&self, size: TerminalSize);
    fn stop(&self);
}

pub(crate) struct Unavailable;
impl Transport for Unavailable {
    fn start(&self, _: TerminalSize, sink: Sink) {
        sink.send(Event::Failed("Terminal provider is unavailable".into()));
    }
    fn write(&self, _: &[u8]) -> Result<(), String> {
        Err("Terminal provider is unavailable".into())
    }
    fn resize(&self, _: TerminalSize) {}
    fn stop(&self) {}
}
