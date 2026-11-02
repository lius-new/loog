use super::*;
use std::collections::BTreeSet;
#[derive(Debug)]
pub enum WatchControl {
    Start { watch: u64, request: WatchRequest },
    Stop { watch: u64 },
}
#[derive(Default)]
pub(super) struct Watches {
    next: u64,
    live: BTreeSet<u64>,
    controls: Vec<WatchControl>,
}
impl Watches {
    pub fn start(&mut self, request: WatchRequest) -> Result<u64, HostError> {
        if self.live.len() >= 16 || self.controls.len() >= 32 {
            return Err(host_error(
                ErrorKind::LimitExceeded,
                "too many file watches or unconsumed controls",
            ));
        }
        if request.patterns.is_empty()
            || request.patterns.len() > 64
            || request
                .patterns
                .iter()
                .any(|p| p.is_empty() || p.len() > 1024 || p.contains('\0'))
        {
            return Err(host_error(
                ErrorKind::LimitExceeded,
                "invalid watch patterns",
            ));
        }
        self.next += 1;
        self.live.insert(self.next);
        self.controls.push(WatchControl::Start {
            watch: self.next,
            request,
        });
        Ok(self.next)
    }
    pub fn stop(&mut self, watch: u64) {
        if self.live.remove(&watch) {
            self.controls.push(WatchControl::Stop { watch });
        }
    }
    pub fn ended(&mut self, watch: u64) {
        self.live.remove(&watch);
    }
    pub fn take(&mut self) -> Vec<WatchControl> {
        std::mem::take(&mut self.controls)
    }
}
