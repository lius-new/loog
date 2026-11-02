use super::*;
use crate::infrastructure::extensions::processes::{Framing, ProcessEvent, Spec};
use loom_extension_host::{
    ProcessControl, ProcessExit, ProcessFraming, ProcessOutput, ProcessWritable,
};

impl ExtensionService {
    pub(super) fn process_controls(
        &mut self,
        id: &str,
        generation: u64,
        controls: Vec<ProcessControl>,
    ) -> Vec<Effect> {
        if !self
            .packages
            .get(id)
            .is_some_and(|p| p.lifecycle.accepts(generation))
        {
            return vec![];
        }
        let mut effects = vec![];
        for control in controls {
            if let ProcessControl::Spawn { process, request } = control {
                // Tasks and persistent processes use exactly the same admission
                // rule, including the innermost workspace trust decision.
                let root = request
                    .root
                    .or_else(|| (self.roots.len() == 1).then(|| self.roots[0].0));
                let result = root
                    .ok_or_else(|| {
                        ExtensionError::new(
                            ErrorKind::PermissionDenied,
                            "a target workspace folder is required",
                        )
                    })
                    .and_then(|root| {
                        use crate::features::extensions::{
                            manifest::parse_requirement, resource_paths,
                        };
                        let package = &self.packages[id];
                        let runtime = |runtime: &str| {
                            let required = package
                                .manifest
                                .requires
                                .iter()
                                .filter_map(|text| parse_requirement(text))
                                .find(|(name, _)| *name == runtime)?;
                            let dependency = self.packages.get(runtime)?;
                            if !crate::features::extensions::range::parse_version(
                                &dependency.manifest.version,
                            )
                            .is_some_and(|version| required.1.matches(version))
                            {
                                return None;
                            }
                            Some(dependency.dir.clone())
                        };
                        let invalid =
                            |message| ExtensionError::new(ErrorKind::InvalidEdit, message);
                        let program = resource_paths::resolve_command(
                            &request.program,
                            &package.dir,
                            runtime,
                            cfg!(windows),
                        )
                        .map_err(invalid)?;
                        let args = request
                            .args
                            .iter()
                            .map(|arg| resource_paths::resolve_argument(arg, &package.dir, runtime))
                            .collect::<Result<Vec<_>, _>>()
                            .map_err(invalid)?;
                        self.plan_task(
                            id,
                            &loom_extension_host::TaskRequest {
                                call: 0,
                                root,
                                program,
                                args,
                                cwd: request.cwd,
                                env: request.env,
                                timeout_ms: None,
                            },
                        )
                    });
                match result {
                    Ok((root_id, task)) => {
                        self.next_process += 1;
                        let global = self.next_process;
                        self.processes.insert(
                            global,
                            TaskRecord {
                                extension: id.into(),
                                generation,
                                local: process,
                                root_id,
                                cwd: task.cwd.clone(),
                            },
                        );
                        let framing = match request.framing {
                            ProcessFraming::Raw => Framing::Raw,
                            ProcessFraming::ContentLength => Framing::ContentLength,
                            ProcessFraming::ContentLengthText => Framing::ContentLengthText,
                        };
                        effects.push(Effect::StartProcess {
                            process: global,
                            spec: Spec { task, framing },
                        });
                    }
                    Err(error) => effects.push(Effect::Submit(Job::Process {
                        id: id.into(),
                        generation,
                        credit: None,
                        message: Inbound::ProcessExited(ProcessExit {
                            process,
                            code: None,
                            killed: false,
                            error: Some(crate::infrastructure::extensions::wire_error(&error)),
                        }),
                    })),
                }
                continue;
            }
            let local = match &control {
                ProcessControl::Write { process, .. }
                | ProcessControl::Close { process }
                | ProcessControl::Kill { process } => *process,
                _ => unreachable!(),
            };
            let Some((&global, _)) = self.processes.iter().find(|(_, record)| {
                record.extension == id && record.generation == generation && record.local == local
            }) else {
                continue;
            };
            effects.push(match control {
                ProcessControl::Write { bytes, .. } => Effect::WriteProcess {
                    process: global,
                    bytes,
                },
                ProcessControl::Close { .. } => Effect::CloseProcess { process: global },
                ProcessControl::Kill { .. } => Effect::KillProcess { process: global },
                _ => unreachable!(),
            });
        }
        effects
    }

    pub fn handle_process_event(&mut self, event: ProcessEvent) -> Vec<Effect> {
        let global = match &event {
            ProcessEvent::Output { process, .. }
            | ProcessEvent::Writable { process, .. }
            | ProcessEvent::Exited { process, .. } => *process,
        };
        let Some(record) = self.processes.get(&global) else {
            return vec![];
        };
        let (id, generation, local) = (record.extension.clone(), record.generation, record.local);
        let (message, credit) = match event {
            ProcessEvent::Output {
                channel,
                bytes,
                text,
                credit,
                ..
            } => (
                Inbound::ProcessOutput(ProcessOutput {
                    process: local,
                    channel: match channel {
                        tasks::Channel::Stdout => loom_extension_host::OutputChannel::Stdout,
                        tasks::Channel::Stderr => loom_extension_host::OutputChannel::Stderr,
                    },
                    bytes,
                    text,
                }),
                Some(credit),
            ),
            ProcessEvent::Writable { bytes, .. } => (
                Inbound::ProcessWritable(ProcessWritable {
                    process: local,
                    bytes: bytes as u32,
                }),
                None,
            ),
            ProcessEvent::Exited {
                code,
                killed,
                error,
                ..
            } => {
                self.processes.remove(&global);
                (
                    Inbound::ProcessExited(ProcessExit {
                        process: local,
                        code,
                        killed,
                        error: error.map(|message| {
                            crate::infrastructure::extensions::wire_error(&ExtensionError::new(
                                ErrorKind::ExtensionFailed,
                                message,
                            ))
                        }),
                    }),
                    None,
                )
            }
        };
        if !self
            .packages
            .get(&id)
            .is_some_and(|p| p.lifecycle.accepts(generation))
        {
            return vec![];
        }
        vec![Effect::Submit(Job::Process {
            id,
            generation,
            message,
            credit,
        })]
    }
}
