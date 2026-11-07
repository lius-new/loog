//! Binary, bidirectional background processes. No protocol or language knowledge.
use super::tasks;
use crate::features::process_framing::{self, Decoder};
use crate::infrastructure::process_job::Job;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc::{self, SyncSender},
};
use std::time::Duration;

pub const MAX_MESSAGE: usize = 32 * 1024 * 1024;
pub const INPUT_BUDGET: usize = 64 * 1024 * 1024;
pub const OUTPUT_BUDGET: usize = 64 * 1024 * 1024;
const POLL: Duration = Duration::from_millis(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Framing {
    Raw,
    ContentLength,
    ContentLengthText,
}
#[derive(Clone, Debug)]
pub struct Spec {
    pub task: tasks::Spec,
    pub framing: Framing,
}

/// Kept with an output through both application and executor queues. Releasing
/// it after guest dispatch returns credit to the reader; a slow guest stops IO.
#[derive(Debug)]
pub struct Credit {
    held: Arc<AtomicUsize>,
    bytes: usize,
}
impl Drop for Credit {
    fn drop(&mut self) {
        self.held.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}
#[derive(Debug)]
pub enum ProcessEvent {
    Output {
        process: u64,
        channel: tasks::Channel,
        bytes: Vec<u8>,
        text: Option<String>,
        credit: Credit,
    },
    Writable {
        process: u64,
        bytes: usize,
    },
    Exited {
        process: u64,
        code: Option<i32>,
        killed: bool,
        error: Option<String>,
    },
}
type Sink = Arc<dyn Fn(ProcessEvent) + Send + Sync>;
enum Input {
    Write(Vec<u8>),
    Close,
}
struct Running {
    cancel: Arc<AtomicBool>,
    input: Option<SyncSender<Input>>,
    queued: Arc<AtomicUsize>,
    failure: Arc<Mutex<Option<String>>>,
    closed: bool,
}
pub struct Runner {
    sink: Sink,
    running: Arc<Mutex<HashMap<u64, Running>>>,
}
impl Runner {
    pub fn new(sink: impl Fn(ProcessEvent) + Send + Sync + 'static) -> Self {
        Self {
            sink: Arc::new(sink),
            running: Arc::default(),
        }
    }
    pub fn start(&self, process: u64, spec: Spec) {
        let cancel = Arc::new(AtomicBool::new(false));
        let queued = Arc::new(AtomicUsize::new(0));
        let failure: Arc<Mutex<Option<String>>> = Arc::default();
        let (input, writes) = mpsc::sync_channel(65);
        let mut map = self.running.lock().unwrap();
        if map.contains_key(&process) {
            return;
        }
        map.insert(
            process,
            Running {
                cancel: cancel.clone(),
                input: Some(input),
                queued: queued.clone(),
                failure: failure.clone(),
                closed: false,
            },
        );
        drop(map);
        let running = self.running.clone();
        let sink = self.sink.clone();
        let spawn = std::thread::Builder::new()
            .name("loog-extension-process".into())
            .spawn(move || {
                let run = || -> Result<Option<i32>, String> {
                    if cancel.load(Ordering::Acquire) {
                        return Ok(None);
                    }
                    let mut command = tasks::command(&spec.task);
                    command.stdin(std::process::Stdio::piped());
                    let mut child = command.spawn().map_err(|error| error.to_string())?;
                    let Some(job) = Job::adopt(&child) else {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err("could not own the process tree".into());
                    };
                    let stdin = child.stdin.take().unwrap();
                    let stdout = child.stdout.take().unwrap();
                    let stderr = child.stderr.take().unwrap();
                    let fail = {
                        let failure = failure.clone();
                        let cancel = cancel.clone();
                        Arc::new(move |error: String| {
                            *failure.lock().unwrap() = Some(error);
                            cancel.store(true, Ordering::Release);
                        })
                    };
                    let output = Arc::new(AtomicUsize::new(0));
                    let reader = |channel, pipe: Box<dyn Read + Send>, framing| {
                        let sink = sink.clone();
                        let cancel = cancel.clone();
                        let held = output.clone();
                        let fail = fail.clone();
                        std::thread::spawn(move || {
                            if let Err(error) =
                                pump(process, channel, pipe, framing, &cancel, held, sink)
                            {
                                fail(error);
                            }
                        })
                    };
                    let out = reader(tasks::Channel::Stdout, Box::new(stdout), spec.framing);
                    let err = reader(tasks::Channel::Stderr, Box::new(stderr), Framing::Raw);
                    let writer = {
                        let sink = sink.clone();
                        let fail = fail.clone();
                        std::thread::spawn(move || {
                            let mut stdin = stdin;
                            while let Ok(input) = writes.recv() {
                                let Input::Write(bytes) = input else {
                                    break;
                                };
                                let length = bytes.len();
                                let bytes = match spec.framing {
                                    Framing::Raw => bytes,
                                    Framing::ContentLength | Framing::ContentLengthText => {
                                        process_framing::encode(&bytes)
                                    }
                                };
                                let result = stdin.write_all(&bytes).and_then(|_| stdin.flush());
                                queued.fetch_sub(length, Ordering::AcqRel);
                                if let Err(error) = result {
                                    fail(format!("stdin: {error}"));
                                    break;
                                }
                                sink(ProcessEvent::Writable {
                                    process,
                                    bytes: length,
                                });
                            }
                        })
                    };
                    let status = loop {
                        if cancel.load(Ordering::Acquire) {
                            let _ = child.kill();
                            break child.wait();
                        }
                        match child.try_wait() {
                            Ok(Some(status)) => break Ok(status),
                            Ok(None) => std::thread::sleep(POLL),
                            Err(error) => {
                                let _ = child.kill();
                                let _ = child.wait();
                                break Err(error);
                            }
                        }
                    };
                    // Helpers can retain pipe handles even after the direct child exits.
                    drop(job);
                    if let Some(process) = running.lock().unwrap().get_mut(&process) {
                        process.input.take();
                    }
                    let _ = writer.join();
                    let _ = out.join();
                    let _ = err.join();
                    if let Some(error) = failure.lock().unwrap().take() {
                        return Err(error);
                    }
                    status.map(|s| s.code()).map_err(|e| e.to_string())
                };
                let result = run();
                running.lock().unwrap().remove(&process);
                sink(ProcessEvent::Exited {
                    process,
                    code: result.as_ref().ok().copied().flatten(),
                    killed: cancel.load(Ordering::Acquire),
                    error: result.err(),
                });
            });
        if let Err(error) = spawn {
            self.running.lock().unwrap().remove(&process);
            (self.sink)(ProcessEvent::Exited {
                process,
                code: None,
                killed: false,
                error: Some(error.to_string()),
            });
        }
    }
    /// Whole-message admission, never blocking. Zero means retry after Writable.
    pub fn write(&self, process: u64, bytes: Vec<u8>) -> Result<usize, String> {
        if bytes.len() > MAX_MESSAGE {
            return Err("write exceeds the message budget".into());
        }
        let map = self.running.lock().unwrap();
        let running = map.get(&process).ok_or("process has exited")?;
        if running.closed {
            return Err("stdin is closed".into());
        }
        let input = running.input.as_ref().ok_or("process has exited")?;
        let length = bytes.len();
        if running
            .queued
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                n.checked_add(length).filter(|n| *n <= INPUT_BUDGET)
            })
            .is_err()
        {
            return Ok(0);
        }
        match input.try_send(Input::Write(bytes)) {
            Ok(()) => Ok(length),
            Err(error) => {
                running.queued.fetch_sub(length, Ordering::AcqRel);
                match error {
                    mpsc::TrySendError::Full(_) => Ok(0),
                    mpsc::TrySendError::Disconnected(_) => Err("stdin has exited".into()),
                }
            }
        }
    }
    pub fn close_stdin(&self, process: u64) -> Result<(), String> {
        let mut map = self.running.lock().unwrap();
        let running = map.get_mut(&process).ok_or("process has exited")?;
        if running.closed {
            return Ok(());
        }
        running
            .input
            .as_ref()
            .ok_or("process has exited")?
            .try_send(Input::Close)
            .map_err(|_| "stdin queue is full; retry after Writable")?;
        running.closed = true;
        Ok(())
    }
    pub fn kill(&self, process: u64) {
        if let Some(running) = self.running.lock().unwrap().get(&process) {
            running.cancel.store(true, Ordering::Release);
        }
    }
    pub fn fail(&self, process: u64, error: String) {
        if let Some(running) = self.running.lock().unwrap().get(&process) {
            *running.failure.lock().unwrap() = Some(error);
            running.cancel.store(true, Ordering::Release);
        }
    }
    pub fn cancel_all(&self) {
        for running in self.running.lock().unwrap().values() {
            running.cancel.store(true, Ordering::Release);
        }
    }
}
impl Drop for Runner {
    fn drop(&mut self) {
        self.cancel_all();
    }
}

fn pump(
    process: u64,
    channel: tasks::Channel,
    mut pipe: Box<dyn Read + Send>,
    framing: Framing,
    cancel: &AtomicBool,
    held: Arc<AtomicUsize>,
    sink: Sink,
) -> Result<(), String> {
    let mut buffer = [0; 8192];
    let mut decoder = Decoder::new(MAX_MESSAGE);
    loop {
        let length = pipe.read(&mut buffer).map_err(|e| e.to_string())?;
        if length == 0 {
            if framing != Framing::Raw && !decoder.is_complete() {
                return Err("stdout ended in an incomplete frame".into());
            }
            return Ok(());
        }
        let chunks = match framing {
            Framing::Raw => vec![buffer[..length].to_vec()],
            Framing::ContentLength | Framing::ContentLengthText => {
                decoder.push(&buffer[..length]).map_err(|e| e.to_string())?
            }
        };
        for bytes in chunks {
            let length = bytes.len().max(4096);
            let (bytes, text) = if framing == Framing::ContentLengthText {
                (
                    Vec::new(),
                    Some(String::from_utf8(bytes).map_err(|_| "text frame is not valid UTF-8")?),
                )
            } else {
                (bytes, None)
            };
            loop {
                if cancel.load(Ordering::Acquire) {
                    return Ok(());
                }
                if held
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                        n.checked_add(length).filter(|n| *n <= OUTPUT_BUDGET)
                    })
                    .is_ok()
                {
                    break;
                }
                std::thread::sleep(POLL);
            }
            sink(ProcessEvent::Output {
                process,
                channel,
                bytes,
                text,
                credit: Credit {
                    held: held.clone(),
                    bytes: length,
                },
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    fn runner() -> (Runner, mpsc::Receiver<ProcessEvent>) {
        let (tx, rx) = mpsc::channel();
        (
            Runner::new(move |event| {
                let _ = tx.send(event);
            }),
            rx,
        )
    }
    fn node(script: &str, framing: Framing) -> Spec {
        Spec {
            task: tasks::Spec {
                program: "node".into(),
                args: vec!["-e".into(), script.into()],
                cwd: std::env::temp_dir(),
                env: vec![],
                timeout: Duration::from_secs(30),
            },
            framing,
        }
    }
    #[test]
    fn binary_round_trips_in_raw_and_framed_modes_and_closes_in_order() {
        for framing in [
            Framing::Raw,
            Framing::ContentLength,
            Framing::ContentLengthText,
        ] {
            let (runner, events) = runner();
            runner.start(1, node("process.stdin.pipe(process.stdout)", framing));
            let body = if framing == Framing::ContentLengthText {
                "中😀\r\n".as_bytes().to_vec()
            } else {
                vec![0, 255, 0, 228, 184, 173, 13, 10]
            };
            assert_eq!(runner.write(1, body.clone()).unwrap(), body.len());
            runner.close_stdin(1).unwrap();
            let mut bytes = vec![];
            let mut writable = 0;
            loop {
                match events.recv_timeout(Duration::from_secs(10)).unwrap() {
                    ProcessEvent::Output {
                        bytes: data,
                        text,
                        credit,
                        ..
                    } => {
                        assert_eq!(text.is_some(), framing == Framing::ContentLengthText);
                        bytes.extend(text.map(String::into_bytes).unwrap_or(data));
                        drop(credit);
                    }
                    ProcessEvent::Writable { bytes, .. } => writable += bytes,
                    ProcessEvent::Exited {
                        code,
                        killed,
                        error,
                        ..
                    } => {
                        assert_eq!(code, Some(0));
                        assert!(!killed);
                        assert!(error.is_none(), "{error:?}");
                        break;
                    }
                }
            }
            assert_eq!(bytes, body);
            assert_eq!(writable, body.len());
            assert!(events.try_recv().is_err(), "exit is delivered exactly once");
            assert!(runner.write(1, vec![1]).is_err());
        }
    }
    #[test]
    fn invalid_text_frames_fail_without_consuming_output_credits() {
        let held = Arc::new(AtomicUsize::new(0));
        let error = pump(
            1,
            tasks::Channel::Stdout,
            Box::new(std::io::Cursor::new(process_framing::encode(&[255]))),
            Framing::ContentLengthText,
            &AtomicBool::new(false),
            held.clone(),
            Arc::new(|_| panic!("invalid UTF-8 was delivered")),
        )
        .unwrap_err();
        assert!(error.contains("not valid UTF-8"));
        assert_eq!(held.load(Ordering::Acquire), 0);
    }
    #[test]
    fn killing_a_process_also_closes_pipes_inherited_by_helpers() {
        let (runner, events) = runner();
        runner.start(1, node("require('child_process').spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{stdio:'inherit'});process.stdout.write('ready');setInterval(()=>{},1000)", Framing::Raw));
        loop {
            if matches!(
                events.recv_timeout(Duration::from_secs(10)).unwrap(),
                ProcessEvent::Output { .. }
            ) {
                break;
            }
        }
        let start = Instant::now();
        runner.kill(1);
        loop {
            if let ProcessEvent::Exited { killed, error, .. } =
                events.recv_timeout(Duration::from_secs(10)).unwrap()
            {
                assert!(killed);
                assert!(error.is_none(), "{error:?}");
                break;
            }
        }
        assert!(start.elapsed() < Duration::from_secs(5));
    }
    #[test]
    fn a_header_without_its_body_is_a_failed_stream() {
        let (runner, events) = runner();
        runner.start(
            1,
            node(
                "process.stdout.write('Content-Length: 4\\r\\n\\r\\n')",
                Framing::ContentLength,
            ),
        );
        loop {
            if let ProcessEvent::Exited { error, .. } =
                events.recv_timeout(Duration::from_secs(10)).unwrap()
            {
                assert!(error.unwrap().contains("incomplete frame"));
                break;
            }
        }
    }
    #[test]
    fn output_waits_for_guest_credit_and_cancellation_unblocks_it() {
        let (tx, rx) = mpsc::channel();
        let held = Arc::new(AtomicUsize::new(OUTPUT_BUDGET));
        let cancel = Arc::new(AtomicBool::new(false));
        let worker = {
            let held = held.clone();
            let cancel = cancel.clone();
            std::thread::spawn(move || {
                pump(
                    1,
                    tasks::Channel::Stdout,
                    Box::new(std::io::Cursor::new(vec![255])),
                    Framing::Raw,
                    &cancel,
                    held,
                    Arc::new(move |event| {
                        let _ = tx.send(event);
                    }),
                )
            })
        };
        assert!(rx.recv_timeout(Duration::from_millis(50)).is_err());
        held.store(0, Ordering::Release);
        let event = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(&event, ProcessEvent::Output { bytes, .. } if bytes == &[255]));
        assert_eq!(held.load(Ordering::Acquire), 4096);
        drop(event);
        assert_eq!(held.load(Ordering::Acquire), 0);
        worker.join().unwrap().unwrap();
    }
}
