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
        record_output(&id, &event);
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

/// The findings of one file as the problems list keeps them. A finding whose
/// severity is not 1 to 4 was refused earlier, so each one is placed.
pub(super) fn closed_findings(
    file: &loom_extension_host::FileFindings,
) -> Vec<crate::features::problems::Finding> {
    use crate::features::language_service::diagnostics::Severity;
    use crate::features::providers::PositionEncoding;
    use crate::features::providers::position::Position;
    use loom_extension_host::FindingEncoding;
    let encoding = match file.encoding {
        FindingEncoding::Utf8 => PositionEncoding::Utf8,
        FindingEncoding::Utf16 => PositionEncoding::Utf16,
    };
    file.items
        .iter()
        .map(|f| crate::features::problems::Finding {
            severity: match f.severity {
                1 => Severity::Error,
                2 => Severity::Warning,
                3 => Severity::Information,
                _ => Severity::Hint,
            },
            message: f.message.clone(),
            source: f.source.clone(),
            code: f.code.clone(),
            span: crate::features::language_service::reply::Span {
                start: Position {
                    line: f.start.line,
                    character: f.start.character,
                },
                end: Position {
                    line: f.end.line,
                    character: f.end.character,
                },
                encoding,
            },
        })
        .collect()
}

/// Copies what an extension's process says into its Output channel, before the
/// extension sees it. Nothing is held back or waited for: flow control belongs
/// to the process pipeline, and a full channel just drops its oldest lines.
fn record_output(extension: &str, event: &ProcessEvent) {
    use crate::features::output::Level;
    let (level, text) = match event {
        ProcessEvent::Output {
            channel: tasks::Channel::Stderr,
            bytes,
            text,
            ..
        } => (Level::Warn, stream_text(bytes, text.as_deref())),
        // Protocol traffic is kept for debugging, below the default level.
        ProcessEvent::Output {
            channel: tasks::Channel::Stdout,
            bytes,
            text,
            ..
        } => (Level::Debug, stream_text(bytes, text.as_deref())),
        ProcessEvent::Exited {
            code,
            killed,
            error,
            ..
        } => match (error, code, killed) {
            (Some(error), ..) => (Level::Error, format!("process failed: {error}")),
            (None, _, true) => (Level::Info, "process stopped".to_owned()),
            (None, Some(0), _) => (Level::Info, "process exited".to_owned()),
            (None, Some(code), _) => (Level::Warn, format!("process exited with code {code}")),
            (None, None, _) => (Level::Info, "process exited".to_owned()),
        },
        ProcessEvent::Writable { .. } => return,
    };
    record_process_line(extension, level, &text);
}

/// What a chunk of process output says. A framed stream (such as a language
/// server's) arrives as `text` with no raw bytes; a raw one as bytes only.
fn stream_text(bytes: &[u8], text: Option<&str>) -> String {
    /// Longest chunk copied at once; the rest of a burst is not kept.
    const MAX_CHUNK: usize = 16 * 1024;
    let raw = text.map_or(bytes, str::as_bytes);
    String::from_utf8_lossy(&raw[..raw.len().min(MAX_CHUNK)]).into_owned()
}

/// One line in the process channel of an extension; blank text is not kept.
fn record_process_line(extension: &str, level: crate::features::output::Level, text: &str) {
    use crate::features::output::{self, ChannelId, Group};
    if text.trim().is_empty() {
        return;
    }
    let channel = ChannelId::new(format!("ext:{extension}/process"));
    let hub = output::hub();
    hub.declare(&channel, &format!("{extension} (process)"), Group::Process);
    hub.append(&channel, level, text);
}

#[cfg(test)]
mod output_tests {
    use super::*;
    use crate::features::output::{self, ChannelId, Level};

    #[test]
    fn framed_output_is_read_from_its_text_and_raw_output_from_its_bytes() {
        // A language server's stdout is framed: text, and no raw bytes.
        assert_eq!(stream_text(&[], Some(r#"{"id":1}"#)), r#"{"id":1}"#);
        assert_eq!(stream_text(b"raw", None), "raw");
        assert_eq!(stream_text(&[0xff, b'a'], None), "\u{fffd}a");
        let long = "é".repeat(20_000);
        assert!(stream_text(&[], Some(&long)).len() <= 16 * 1024 + 2);
    }

    #[test]
    fn process_output_is_copied_by_stream() {
        let exited = |code, killed, error: Option<&str>| ProcessEvent::Exited {
            process: 1,
            code,
            killed,
            error: error.map(str::to_owned),
        };
        record_output("test.proc", &exited(Some(0), false, None));
        record_output("test.proc", &exited(Some(3), false, None));
        record_output("test.proc", &exited(None, false, Some("spawn failed")));
        record_process_line("test.proc", Level::Debug, "   ");
        let id = ChannelId::new("ext:test.proc/process");
        let hub = output::hub();
        let lines = hub.lines(&id, Level::Trace, "");
        // A clean exit is Info, a non-zero code Warn, a failure Error; blanks are not kept.
        assert_eq!(
            lines.iter().map(|l| l.level).collect::<Vec<_>>(),
            [Level::Info, Level::Warn, Level::Error]
        );
        assert!(hub.channels().iter().any(|c| c.id == id && c.unread_error));
    }
}
