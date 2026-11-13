//! Bounded, sequential uploads using the generic resource write contract.
use super::{
    extensions::host::{ExtensionHost, ResourceContinuation},
    state::AppState,
};
use loog_workspace_api::{Answer, Failure, FailureKind, Operation, ProviderPath, ScopeId};
use std::sync::{Arc, Mutex, Weak};

#[derive(Clone)]
struct Transfer {
    host: Weak<ExtensionHost>,
    scope: ScopeId,
    bytes: Arc<Vec<u8>>,
    offset: usize,
    handle: Option<u64>,
    done: Arc<Mutex<Option<ResourceContinuation>>>,
}
enum Step {
    Begin,
    Append(usize),
    Commit,
}

pub(super) fn start(
    host: &Arc<ExtensionHost>,
    scope: ScopeId,
    path: ProviderPath,
    bytes: Vec<u8>,
    expected_revision: Option<String>,
    done: ResourceContinuation,
) -> Result<(), Failure> {
    if bytes.len() > 64 * 1024 * 1024 {
        return Err(Failure::new(
            FailureKind::LimitExceeded,
            "Encoded file exceeds the save size limit",
        ));
    }
    let size = bytes.len() as u64;
    let transfer = Transfer {
        host: Arc::downgrade(host),
        scope,
        bytes: Arc::new(bytes),
        offset: 0,
        handle: None,
        done: Arc::new(Mutex::new(Some(done))),
    };
    transfer.submit(
        Operation::BeginWrite {
            path,
            size,
            expected_revision,
        },
        Step::Begin,
    )
}
impl Transfer {
    fn submit(&self, operation: Operation, step: Step) -> Result<(), Failure> {
        let host = self.host.upgrade().ok_or_else(offline)?;
        let mut transfer = self.clone();
        host.request_resource(
            self.scope,
            operation,
            Box::new(move |app, result| {
                match (step, result) {
                    (Step::Begin, Ok(Answer::Started { handle })) => transfer.handle = Some(handle),
                    (Step::Append(end), Ok(Answer::Ok)) => transfer.offset = end,
                    (Step::Commit, Ok(answer @ Answer::Written { .. })) => {
                        transfer.finish(app, Ok(answer));
                        return;
                    }
                    (_, Err(error)) => {
                        transfer.fail(app, error);
                        return;
                    }
                    _ => {
                        transfer.fail(
                            app,
                            Failure::new(FailureKind::InvalidRequest, "Invalid write response"),
                        );
                        return;
                    }
                }
                let write = transfer.handle.expect("write began");
                let next = if transfer.offset < transfer.bytes.len() {
                    let end = (transfer.offset + 128 * 1024).min(transfer.bytes.len());
                    transfer.submit(
                        Operation::AppendWrite {
                            write,
                            offset: transfer.offset as u64,
                            bytes: transfer.bytes[transfer.offset..end].to_vec(),
                        },
                        Step::Append(end),
                    )
                } else {
                    transfer.submit(Operation::CommitWrite { write }, Step::Commit)
                };
                if let Err(error) = next {
                    transfer.fail(app, error);
                }
            }),
        )
        .map(|_| ())
    }
    fn fail(&self, app: &mut AppState, error: Failure) {
        if let (Some(host), Some(write)) = (self.host.upgrade(), self.handle) {
            let _ = host.request_resource(
                self.scope,
                Operation::AbortWrite { write },
                Box::new(|_, _| {}),
            );
        }
        self.finish(app, Err(error));
    }
    fn finish(&self, app: &mut AppState, result: Result<Answer, Failure>) {
        let done = self.done.lock().unwrap().take();
        if let Some(done) = done {
            done(app, result);
        }
    }
}
fn offline() -> Failure {
    Failure::new(FailureKind::Unavailable, "Resource provider stopped")
}
