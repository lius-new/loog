//! Background executor for extension instances (design §5, §7.1, §8).
//!
//! One thread owns the Wasmtime runtime and every instance, so the UI thread
//! never waits on guest code. The application sends [`Job`]s and receives
//! [`Event`]s through a sink; nothing UI-shaped crosses this boundary, only
//! typed values and opaque ids (design §5, last layering rule).
pub mod tasks;
pub mod trust_store;

use loom_extension_api::{ErrorKind, ExtensionError};
use loom_extension_host::{
    AcceptedEdit, ByteRange, CommandContext, CommittedReceipt, Completion, HostError, Inbound,
    Instance, Limits, Provided, ProviderKind, ProviderRequest, PublishedDiagnostics, RootRef,
    RoundStatus, Runtime, Selection, StartedTask, WitErrorKind,
};
use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

/// Rounds in a row the executor gives one instance when it keeps reporting
/// `runnable`, before other instances and controls get their turn.
const MAX_CONSECUTIVE_ROUNDS: usize = 8;

/// A request to a registered provider for one state of one document.
#[derive(Clone, Debug)]
pub struct ProviderJob {
    pub request: u64,
    pub provider: String,
    pub kind: ProviderKind,
    pub token: u64,
    pub text: Arc<String>,
    pub document: u64,
    pub version: u64,
    pub range: Option<Range<usize>>,
    /// Completion: the caret, the triggering character and, for a resolve request,
    /// the item to complete.
    pub cursor: Option<usize>,
    pub trigger: Option<String>,
    pub item: Option<loom_extension_host::WireCompletionItem>,
}

/// What the application tells an instance happened to an operation.
#[derive(Clone, Debug)]
pub enum Outcome {
    Committed { transaction: u64, version: u64 },
    Failed(ExtensionError),
}

#[derive(Clone, Debug)]
pub struct Run {
    pub call: u64,
    pub command: String,
    /// Captured target text, readable by the guest through this token.
    pub snapshot: Option<(u64, Arc<String>)>,
    /// `(range, reversed)` for every selection, in document order.
    pub selections: Vec<(Range<usize>, bool)>,
    pub selection_precondition: Option<u64>,
}

// Jobs are moved through a channel once each; the size gap between loading a
// component and the small messages is not worth boxing every field.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Job {
    Load {
        id: String,
        generation: u64,
        component: Vec<u8>,
        cache: PathBuf,
        limits: Limits,
    },
    Run {
        id: String,
        generation: u64,
        run: Run,
    },
    Complete {
        id: String,
        generation: u64,
        operation: u64,
        outcome: Outcome,
    },
    Cancel {
        id: String,
        generation: u64,
        call: u64,
    },
    /// Ask a provider; the answer comes back as [`Event::Provided`].
    Provide {
        id: String,
        generation: u64,
        job: Box<ProviderJob>,
    },
    /// The workspace folders the extension may name.
    Roots {
        id: String,
        generation: u64,
        roots: Vec<RootRef>,
    },
    /// Something happened to a task of the extension. `ended` frees its slot.
    Task {
        id: String,
        generation: u64,
        message: Inbound,
        ended: bool,
    },
    /// New resolved setting values; `changed` lists the keys whose value differs.
    Settings {
        id: String,
        generation: u64,
        values: std::collections::BTreeMap<String, String>,
        changed: Vec<String>,
    },
    /// Document events, in order, for an instance that subscribed to them.
    Deliver {
        id: String,
        generation: u64,
        messages: Vec<Inbound>,
    },
    Stop {
        id: String,
    },
    Shutdown,
}

#[derive(Debug)]
pub enum Event {
    Loaded {
        id: String,
        generation: u64,
        cache_hit: bool,
        result: Result<(), ExtensionError>,
    },
    EditRequested {
        id: String,
        generation: u64,
        edit: AcceptedEdit,
    },
    /// A provider answered. The application decides whether the answer is
    /// still current before using it.
    Provided {
        id: String,
        generation: u64,
        provided: Provided,
    },
    /// The guest asked to start a task. The application decides whether it may run.
    TaskRequested {
        id: String,
        generation: u64,
        task: StartedTask,
    },
    /// The guest asked to cancel tasks (their local numbers).
    TasksCancelled {
        id: String,
        generation: u64,
        tasks: Vec<u64>,
    },
    /// The guest pushed findings for one state of a file.
    DiagnosticsPublished {
        id: String,
        generation: u64,
        published: PublishedDiagnostics,
    },
    /// The instance trapped or ran past a limit; it has been dropped.
    Failed {
        id: String,
        generation: u64,
        error: ExtensionError,
    },
    Log {
        id: String,
        level: u8,
        text: String,
    },
}

pub type Sink = Box<dyn Fn(Event) + Send + 'static>;

pub struct Executor {
    jobs: Sender<Job>,
    thread: Option<JoinHandle<()>>,
}

impl Executor {
    pub fn spawn(sink: Sink) -> Result<Self, String> {
        let (jobs, inbox) = channel();
        let thread = std::thread::Builder::new()
            .name("loom-extensions".to_owned())
            .spawn(move || {
                let Ok(runtime) = Runtime::new() else {
                    return;
                };
                Worker {
                    runtime,
                    sink,
                    instances: HashMap::new(),
                    inbox,
                }
                .run();
            })
            .map_err(|error| format!("could not start the extension executor: {error}"))?;
        Ok(Self {
            jobs,
            thread: Some(thread),
        })
    }

    pub fn submit(&self, job: Job) -> bool {
        self.jobs.send(job).is_ok()
    }

    /// Stop accepting work and wait for the executor to finish its queue.
    pub fn shutdown(&mut self) {
        let _ = self.jobs.send(Job::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Executor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct Slot {
    generation: u64,
    instance: Instance,
}

struct Worker {
    runtime: Runtime,
    sink: Sink,
    instances: HashMap<String, Slot>,
    inbox: Receiver<Job>,
}

pub fn wire_error(error: &ExtensionError) -> HostError {
    wit_error(error)
}

fn wit_error(error: &ExtensionError) -> HostError {
    HostError {
        kind: match error.kind {
            ErrorKind::UnsupportedApi => WitErrorKind::UnsupportedApi,
            ErrorKind::PermissionDenied => WitErrorKind::PermissionDenied,
            ErrorKind::StaleSnapshot => WitErrorKind::StaleSnapshot,
            ErrorKind::ExpiredHandle => WitErrorKind::ExpiredHandle,
            ErrorKind::Cancelled => WitErrorKind::Cancelled,
            ErrorKind::DeadlineExceeded => WitErrorKind::DeadlineExceeded,
            ErrorKind::LimitExceeded => WitErrorKind::LimitExceeded,
            ErrorKind::Conflict => WitErrorKind::Conflict,
            ErrorKind::InvalidEdit => WitErrorKind::InvalidEdit,
            ErrorKind::ExtensionFailed => WitErrorKind::ExtensionFailed,
        },
        message: error.message.clone(),
        committed: error.committed,
    }
}

impl Worker {
    fn run(mut self) {
        while let Ok(job) = self.inbox.recv() {
            match job {
                Job::Shutdown => break,
                Job::Load {
                    id,
                    generation,
                    component,
                    cache,
                    limits,
                } => self.load(id, generation, &component, &cache, limits),
                Job::Run {
                    id,
                    generation,
                    run,
                } => {
                    let message = Inbound::RunCommand(CommandContext {
                        call: run.call,
                        command: run.command,
                        snapshot: run.snapshot.as_ref().map(|(token, _)| *token),
                        selections: run
                            .selections
                            .iter()
                            .map(|(range, reversed)| Selection {
                                range: ByteRange {
                                    start: range.start as u64,
                                    end: range.end as u64,
                                },
                                reversed: *reversed,
                            })
                            .collect(),
                        selection_precondition: run.selection_precondition,
                    });
                    if let (Some(slot), Some((token, text))) =
                        (self.slot(&id, generation), run.snapshot)
                    {
                        slot.instance.put_snapshot(token, text);
                    }
                    self.deliver(&id, generation, vec![message]);
                }
                Job::Complete {
                    id,
                    generation,
                    operation,
                    outcome,
                } => {
                    let completion = match outcome {
                        Outcome::Committed {
                            transaction,
                            version,
                        } => Completion::Committed(CommittedReceipt {
                            transaction,
                            version,
                        }),
                        Outcome::Failed(error) => Completion::Failed(wit_error(&error)),
                    };
                    if let Some(slot) = self.slot(&id, generation) {
                        slot.instance.operation_settled();
                    }
                    self.deliver(
                        &id,
                        generation,
                        vec![Inbound::OperationComplete((operation, completion))],
                    );
                }
                Job::Cancel {
                    id,
                    generation,
                    call,
                } => {
                    self.deliver(&id, generation, vec![Inbound::Cancel(call)]);
                }
                Job::Provide {
                    id,
                    generation,
                    job,
                } => {
                    let job = *job;
                    let message = Inbound::ProviderRequest(ProviderRequest {
                        request: job.request,
                        provider: job.provider,
                        kind: job.kind,
                        snapshot: job.token,
                        document: job.document,
                        version: job.version,
                        length: job.text.len() as u64,
                        range: job.range.map(|range| ByteRange {
                            start: range.start as u64,
                            end: range.end as u64,
                        }),
                        cursor: job.cursor.map(|cursor| cursor as u64),
                        trigger: job.trigger,
                        item: job.item,
                    });
                    if let Some(slot) = self.slot(&id, generation) {
                        slot.instance.put_snapshot(job.token, job.text);
                    }
                    self.deliver(&id, generation, vec![message]);
                }
                Job::Roots {
                    id,
                    generation,
                    roots,
                } => {
                    if let Some(slot) = self.slot(&id, generation) {
                        slot.instance.put_roots(roots);
                    }
                }
                Job::Task {
                    id,
                    generation,
                    message,
                    ended,
                } => {
                    if ended && let Some(slot) = self.slot(&id, generation) {
                        slot.instance.task_ended();
                    }
                    self.deliver(&id, generation, vec![message]);
                }
                Job::Settings {
                    id,
                    generation,
                    values,
                    changed,
                } => {
                    if let Some(slot) = self.slot(&id, generation) {
                        slot.instance.put_settings(values);
                    }
                    if !changed.is_empty() {
                        self.deliver(&id, generation, vec![Inbound::SettingsChanged(changed)]);
                    }
                }
                Job::Deliver {
                    id,
                    generation,
                    messages,
                } => self.deliver(&id, generation, messages),
                Job::Stop { id } => {
                    self.instances.remove(&id);
                }
            }
        }
    }

    fn slot(&mut self, id: &str, generation: u64) -> Option<&mut Slot> {
        self.instances
            .get_mut(id)
            .filter(|slot| slot.generation == generation)
    }

    fn load(
        &mut self,
        id: String,
        generation: u64,
        component: &[u8],
        cache: &std::path::Path,
        limits: Limits,
    ) {
        // A reload replaces the old instance first, so its callbacks cannot act on the new one.
        self.instances.remove(&id);
        let result = self
            .runtime
            .compile_cached(component, cache)
            .and_then(|(compiled, hit)| {
                self.runtime
                    .instantiate(&compiled, limits)
                    .map(|instance| (instance, hit))
            });
        match result {
            Ok((instance, cache_hit)) => {
                self.instances.insert(
                    id.clone(),
                    Slot {
                        generation,
                        instance,
                    },
                );
                (self.sink)(Event::Loaded {
                    id,
                    generation,
                    cache_hit,
                    result: Ok(()),
                });
            }
            Err(error) => (self.sink)(Event::Loaded {
                id,
                generation,
                cache_hit: false,
                result: Err(error),
            }),
        }
    }

    /// Deliver one message, then keep running while the guest asks for more rounds.
    fn deliver(&mut self, id: &str, generation: u64, messages: Vec<Inbound>) {
        let mut batch = messages;
        for _ in 0..MAX_CONSECUTIVE_ROUNDS {
            let Some(slot) = self.slot(id, generation) else {
                return;
            };
            let status = slot.instance.dispatch(&batch);
            let accepted = slot.instance.take_accepted();
            let logs = slot.instance.take_logs();
            let provided = slot.instance.take_provided();
            let started = slot.instance.take_started_tasks();
            let cancelled = slot.instance.take_cancelled_tasks();
            let published = slot.instance.take_published();
            for (level, text) in logs {
                (self.sink)(Event::Log {
                    id: id.to_owned(),
                    level,
                    text,
                });
            }
            for task in started {
                (self.sink)(Event::TaskRequested {
                    id: id.to_owned(),
                    generation,
                    task,
                });
            }
            if !cancelled.is_empty() {
                (self.sink)(Event::TasksCancelled {
                    id: id.to_owned(),
                    generation,
                    tasks: cancelled,
                });
            }
            for published in published {
                (self.sink)(Event::DiagnosticsPublished {
                    id: id.to_owned(),
                    generation,
                    published,
                });
            }
            for answer in provided {
                (self.sink)(Event::Provided {
                    id: id.to_owned(),
                    generation,
                    provided: answer,
                });
            }
            for edit in accepted {
                (self.sink)(Event::EditRequested {
                    id: id.to_owned(),
                    generation,
                    edit,
                });
            }
            match status {
                Ok(RoundStatus::Runnable) => batch = Vec::new(),
                Ok(_) => return,
                Err(error) => {
                    // A trap or limit fails the instance; the application reports it
                    // and a retry creates a new generation (design §8).
                    self.instances.remove(id);
                    (self.sink)(Event::Failed {
                        id: id.to_owned(),
                        generation,
                        error,
                    });
                    return;
                }
            }
        }
    }
}
