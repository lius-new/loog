use super::*;
use std::collections::BTreeMap;

const MAX_MESSAGE: usize = 32 * 1024 * 1024;
const INPUT_BUDGET: usize = 64 * 1024 * 1024;
const MAX_WRITES: usize = 64;
const MAX_CONTROLS: usize = 128;

#[derive(Debug)]
pub enum ProcessControl {
    Spawn {
        process: u64,
        request: ProcessRequest,
    },
    Write {
        process: u64,
        bytes: Vec<u8>,
    },
    Close {
        process: u64,
    },
    Kill {
        process: u64,
    },
}
#[derive(Default)]
struct Stream {
    bytes: usize,
    writes: usize,
    closed: bool,
    killed: bool,
}
#[derive(Default)]
pub(super) struct Processes {
    next: u64,
    streams: BTreeMap<u64, Stream>,
    controls: Vec<ProcessControl>,
}
impl Processes {
    fn budget(&self) -> Result<(), HostError> {
        if self.controls.len() >= MAX_CONTROLS {
            Err(host_error(
                ErrorKind::LimitExceeded,
                "too many unconsumed process controls",
            ))
        } else {
            Ok(())
        }
    }
    pub fn spawn(&mut self, request: ProcessRequest, limit: usize) -> Result<u64, HostError> {
        self.budget()?;
        if self.streams.len() >= limit {
            return Err(host_error(
                ErrorKind::LimitExceeded,
                "too many running processes",
            ));
        }
        self.next += 1;
        self.streams.insert(self.next, Stream::default());
        self.controls.push(ProcessControl::Spawn {
            process: self.next,
            request,
        });
        Ok(self.next)
    }
    pub fn write(&mut self, process: u64, bytes: Vec<u8>) -> Result<u32, HostError> {
        self.budget()?;
        let stream = self
            .streams
            .get_mut(&process)
            .ok_or_else(|| host_error(ErrorKind::ExpiredHandle, "unknown process"))?;
        if stream.closed || stream.killed {
            return Err(host_error(
                ErrorKind::ExpiredHandle,
                "process input is closed",
            ));
        }
        if bytes.len() > MAX_MESSAGE {
            return Err(host_error(
                ErrorKind::LimitExceeded,
                "write exceeds the message budget",
            ));
        }
        if stream.writes >= MAX_WRITES || stream.bytes + bytes.len() > INPUT_BUDGET {
            return Ok(0);
        }
        let length = bytes.len();
        stream.bytes += length;
        stream.writes += 1;
        self.controls.push(ProcessControl::Write { process, bytes });
        Ok(length as u32)
    }
    pub fn close(&mut self, process: u64) -> Result<(), HostError> {
        self.budget()?;
        let stream = self
            .streams
            .get_mut(&process)
            .ok_or_else(|| host_error(ErrorKind::ExpiredHandle, "unknown process"))?;
        if !stream.closed {
            stream.closed = true;
            self.controls.push(ProcessControl::Close { process });
        }
        Ok(())
    }
    pub fn kill(&mut self, process: u64) {
        if let Some(stream) = self.streams.get_mut(&process)
            && !stream.killed
        {
            stream.killed = true;
            // At most one kill per live stream; a full ordinary-control queue
            // never prevents shutdown.
            self.controls.push(ProcessControl::Kill { process });
        }
    }
    pub fn take(&mut self) -> Vec<ProcessControl> {
        std::mem::take(&mut self.controls)
    }
    pub fn ended(&mut self, process: u64) {
        self.streams.remove(&process);
    }
    pub fn writable(&mut self, process: u64, bytes: usize) {
        if let Some(stream) = self.streams.get_mut(&process) {
            stream.bytes = stream.bytes.saturating_sub(bytes);
            stream.writes = stream.writes.saturating_sub(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn write_credits_survive_round_draining_and_kill_is_never_blocked_by_writes() {
        let mut processes = Processes::default();
        let id = processes
            .spawn(
                ProcessRequest {
                    root: Some(1),
                    program: "tool".into(),
                    args: vec![],
                    cwd: None,
                    env: vec![],
                    framing: ProcessFraming::Raw,
                },
                1,
            )
            .unwrap();
        for _ in 0..MAX_WRITES {
            assert_eq!(processes.write(id, vec![255]).unwrap(), 1);
        }
        assert_eq!(processes.write(id, vec![1]).unwrap(), 0);
        processes.take();
        assert_eq!(
            processes.write(id, vec![1]).unwrap(),
            0,
            "draining the executor queue does not mean stdin was written"
        );
        processes.writable(id, 1);
        assert_eq!(processes.write(id, vec![1]).unwrap(), 1);
        processes.kill(id);
        assert!(
            matches!(processes.take().last(), Some(ProcessControl::Kill { process }) if *process == id)
        );
        assert!(processes.write(id, vec![1]).is_err());
        processes.ended(id);
        assert!(processes.close(id).is_err());
    }
}
