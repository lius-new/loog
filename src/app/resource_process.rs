//! Bounded background process execution through a resource provider. This is
//! protocol-independent plumbing; callers own their program/argument policy.
use super::{context::AppContext, extensions::host::ExtensionHost};
use loog_workspace_api::{
    Answer, Capability, Failure, FailureKind, Notification, Operation, ResourceRef, ScopeId,
};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

const INPUT_LIMIT: usize = 8 * 1024 * 1024;
const OUTPUT_LIMIT: usize = 16 * 1024 * 1024;
const ERROR_LIMIT: usize = 256 * 1024;
const WINDOW: usize = 256 * 1024;

pub(super) struct Command {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: ResourceRef,
    pub env: Vec<(String, String)>,
    pub input: Vec<u8>,
    pub timeout: Duration,
}
#[derive(Default, Debug)]
pub(super) struct Output {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}
#[derive(Default)]
struct Collected {
    output: Output,
    credit: usize,
    exited: bool,
    error: Option<Failure>,
}
impl Collected {
    fn event(&mut self, event: Result<Notification, Failure>) {
        if self.exited || self.error.is_some() {
            return;
        }
        match event {
            Err(error) => self.error = Some(error),
            Ok(Notification::Output { bytes, stderr, .. }) => {
                let (target, limit) = if stderr {
                    (&mut self.output.stderr, ERROR_LIMIT)
                } else {
                    (&mut self.output.stdout, OUTPUT_LIMIT)
                };
                if target.len().saturating_add(bytes.len()) > limit {
                    self.error = Some(Failure::new(
                        FailureKind::LimitExceeded,
                        "Process output exceeds its capture budget",
                    ));
                } else {
                    target.extend(bytes);
                }
            }
            Ok(Notification::Writable { bytes, .. }) => {
                if self.credit.saturating_add(bytes as usize) > WINDOW {
                    self.error = Some(Failure::new(
                        FailureKind::InvalidRequest,
                        "Process input credit exceeds its window",
                    ));
                } else {
                    self.credit += bytes as usize;
                }
            }
            Ok(Notification::Exited { code, .. }) => {
                self.output.code = code;
                self.exited = true;
            }
            _ => {
                self.error = Some(Failure::new(
                    FailureKind::InvalidRequest,
                    "Unexpected process notification",
                ))
            }
        }
    }
}
struct Handle {
    host: Arc<ExtensionHost>,
    scope: ScopeId,
    value: u64,
    exited: bool,
}
impl Drop for Handle {
    fn drop(&mut self) {
        self.host
            .forget_resource_handle(self.scope, Capability::Process, self.value);
        if !self.exited {
            let _ = self.host.request_resource(
                self.scope,
                Operation::Kill {
                    process: self.value,
                },
                Box::new(|_, _| {}),
            );
        }
    }
}

/// Must run on a background worker; the UI keeps delivering provider receipts.
/// A cancellation/timeout never retries execution and cannot imply rollback.
pub(super) fn run(
    context: &AppContext,
    scope: ScopeId,
    command: Command,
    cancelled: &dyn Fn() -> bool,
) -> Result<Output, Failure> {
    let ResourceRef::Provided { environment, path } = &command.cwd else {
        return Err(Failure::new(
            FailureKind::InvalidRequest,
            "Expected a provided process directory",
        ));
    };
    if command.input.len() > INPUT_LIMIT
        || command.timeout.is_zero()
        || command.timeout > Duration::from_secs(600)
    {
        return Err(Failure::new(
            FailureKind::LimitExceeded,
            "Process input or duration exceeds its budget",
        ));
    }
    if !context
        .get()
        .explorer
        .resources
        .root(scope)
        .is_some_and(|root| root.descriptor.resource.environment() == Some(environment))
    {
        return Err(Failure::new(
            FailureKind::Unavailable,
            "Process environment is no longer connected",
        ));
    }
    if cancelled() {
        return Err(Failure::new(
            FailureKind::Cancelled,
            "Process execution cancelled before start",
        ));
    }
    let host = context
        .services()
        .and_then(|runtime| runtime.extensions())
        .ok_or_else(|| {
            Failure::new(FailureKind::Unavailable, "Resource provider is unavailable")
        })?;
    let deadline = Instant::now() + command.timeout;
    let collected = Arc::new((Mutex::new(Collected::default()), Condvar::new()));
    let expired = || cancelled() || Instant::now() >= deadline;
    let unknown = || {
        Failure::new(
            FailureKind::UnknownOutcome,
            "Process execution cancelled or timed out; its effects may already have occurred and were not retried",
        )
    };
    let receive = |receiver: mpsc::Receiver<Result<Handle, Failure>>, request| {
        loop {
            if expired() {
                host.cancel_resource(request);
                return Err(unknown());
            }
            match receiver.recv_timeout(Duration::from_millis(20)) {
                Ok(value) => return value,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(Failure::new(
                        FailureKind::UnknownOutcome,
                        "Process receipt channel closed; execution was not retried",
                    ));
                }
            }
        }
    };
    let (send, recv) = mpsc::channel();
    let started_host = host.clone();
    let observer = collected.clone();
    let request = host.request_resource(
        scope,
        Operation::Spawn {
            program: command.program,
            args: command.args,
            cwd: path.clone(),
            env: command.env,
        },
        Box::new(move |_, result| {
            if let Ok(Answer::Started { handle }) = &result {
                let handle = *handle;
                started_host.observe_resource_handle(
                    scope,
                    Capability::Process,
                    handle,
                    Arc::new(move |event| {
                        observer.0.lock().unwrap().event(event);
                        observer.1.notify_one();
                    }),
                );
                // Ownership travels in the channel: cancellation also drops an
                // already queued, unread handle, not just a late callback result.
                let _ = send.send(Ok(Handle {
                    host: started_host,
                    scope,
                    value: handle,
                    exited: false,
                }));
            } else {
                let error = result.err().unwrap_or_else(|| {
                    Failure::new(
                        FailureKind::InvalidRequest,
                        "Provider returned an invalid process handle",
                    )
                });
                let _ = send.send(Err(error));
            }
        }),
    )?;
    let mut handle = receive(recv, request)?;
    let mut offset = 0;
    let mut input_closed = false;
    loop {
        if expired() {
            return Err(unknown());
        }
        let mut state = collected.0.lock().unwrap();
        if let Some(error) = state.error.take() {
            return Err(error);
        }
        if state.exited {
            handle.exited = true;
            if state.output.code.is_none() {
                return Err(Failure::new(
                    FailureKind::UnknownOutcome,
                    "Process ended without an exit status; its effects may already have occurred and were not retried",
                ));
            }
            return Ok(std::mem::take(&mut state.output));
        }
        let operation = if offset < command.input.len() && state.credit > 0 {
            let count = (command.input.len() - offset)
                .min(state.credit)
                .min(16 * 1024);
            let bytes = command.input[offset..offset + count].to_vec();
            state.credit -= count;
            offset += count;
            Some(Operation::Input {
                process: handle.value,
                bytes,
            })
        } else if offset == command.input.len() && !input_closed {
            input_closed = true;
            Some(Operation::CloseInput {
                process: handle.value,
            })
        } else {
            None
        };
        if let Some(operation) = operation {
            drop(state);
            let (send, recv) = mpsc::channel();
            let request = handle.host.request_resource(
                scope,
                operation,
                Box::new(move |_, result| {
                    let _ = send.send(result);
                }),
            );
            let mut input_error = match &request {
                Err(error)
                    if matches!(
                        error.kind,
                        FailureKind::Unavailable | FailureKind::PermissionDenied
                    ) =>
                {
                    Some((error.clone(), Instant::now() + Duration::from_secs(2)))
                }
                Err(error) => return Err(error.clone()),
                Ok(_) => None,
            };
            // The observer remains active while waiting for input acknowledgement,
            // so a child writing before reading cannot deadlock the UI or pipes.
            loop {
                if expired() {
                    if let Ok(request) = request {
                        handle.host.cancel_resource(request);
                    }
                    return Err(unknown());
                }
                let mut state = collected.0.lock().unwrap();
                if let Some(error) = state.error.take() {
                    if let Ok(request) = request {
                        handle.host.cancel_resource(request);
                    }
                    return Err(error);
                }
                if state.exited {
                    // Exit follows the drained output streams. A short process
                    // may finish before its input/EOF receipt reaches us; its
                    // authoritative exit wins without executing it again.
                    if let Ok(request) = request {
                        handle.host.cancel_resource(request);
                    }
                    break;
                }
                if let Some((error, deadline)) = &input_error {
                    if Instant::now() >= *deadline {
                        return Err(error.clone());
                    }
                    drop(
                        collected
                            .1
                            .wait_timeout(state, Duration::from_millis(20))
                            .unwrap(),
                    );
                    continue;
                }
                drop(state);
                match recv.recv_timeout(Duration::from_millis(20)) {
                    Ok(Ok(Answer::Ok)) => break,
                    Ok(Err(error))
                        if matches!(
                            error.kind,
                            FailureKind::Unavailable | FailureKind::PermissionDenied
                        ) =>
                    {
                        input_error = Some((error, Instant::now() + Duration::from_secs(2)));
                    }
                    Ok(Err(error)) => return Err(error),
                    Ok(_) => {
                        return Err(Failure::new(
                            FailureKind::InvalidRequest,
                            "Invalid process input receipt",
                        ));
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => return Err(unknown()),
                }
            }
        } else {
            drop(
                collected
                    .1
                    .wait_timeout(state, Duration::from_millis(20))
                    .unwrap(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capture_preserves_binary_streams_and_rejects_credit_and_output_overflow() {
        let mut state = Collected::default();
        state.event(Ok(Notification::Output {
            handle: 1,
            bytes: vec![0, 255],
            stderr: false,
        }));
        state.event(Ok(Notification::Output {
            handle: 1,
            bytes: vec![254, 0],
            stderr: true,
        }));
        state.event(Ok(Notification::Writable {
            handle: 1,
            bytes: WINDOW as u32,
        }));
        assert_eq!(state.output.stdout, [0, 255]);
        assert_eq!(state.output.stderr, [254, 0]);
        state.event(Ok(Notification::Writable {
            handle: 1,
            bytes: 1,
        }));
        assert!(state.error.is_some());
        let mut state = Collected::default();
        state.event(Ok(Notification::Output {
            handle: 1,
            bytes: vec![0; ERROR_LIMIT + 1],
            stderr: true,
        }));
        assert!(state.error.is_some());
        assert!(state.output.stderr.is_empty());
    }
}
