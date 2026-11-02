use super::*;
use crate::infrastructure::extensions::watches::{self, WatchEvent};
use loom_extension_host::{FileChanged, WatchControl, WatchFailure};
pub(super) struct WatchRecord {
    pub extension: String,
    pub generation: u64,
    pub local: u64,
    pub root: u64,
}
impl ExtensionService {
    pub(super) fn watch_controls(
        &mut self,
        id: &str,
        generation: u64,
        controls: Vec<WatchControl>,
    ) -> Vec<Effect> {
        let Some(package) = self
            .packages
            .get(id)
            .filter(|p| p.lifecycle.accepts(generation))
        else {
            return vec![];
        };
        let allowed = files_of(&self.grants, package);
        let mut effects = vec![];
        for control in controls {
            match control {
                WatchControl::Start { watch, request } => {
                    let result = if !allowed {
                        Err(ExtensionError::new(
                            ErrorKind::PermissionDenied,
                            "file read access is not granted",
                        ))
                    } else {
                        self.roots
                            .iter()
                            .find(|(root, _)| *root == request.root)
                            .map(|(_, path)| path.clone())
                            .ok_or_else(|| {
                                ExtensionError::new(
                                    ErrorKind::ExpiredHandle,
                                    "folder is no longer open",
                                )
                            })
                    };
                    match result {
                        Ok(root) => {
                            self.next_watch += 1;
                            let global = self.next_watch;
                            self.watches.insert(
                                global,
                                WatchRecord {
                                    extension: id.into(),
                                    generation,
                                    local: watch,
                                    root: request.root,
                                },
                            );
                            effects.push(Effect::StartWatch {
                                watch: global,
                                spec: watches::Spec {
                                    root,
                                    patterns: request.patterns,
                                },
                            });
                        }
                        Err(error) => effects.push(Effect::Submit(Job::Watch {
                            id: id.into(),
                            generation,
                            credit: None,
                            message: Inbound::WatchFailed(WatchFailure {
                                watch,
                                error: crate::infrastructure::extensions::wire_error(&error),
                            }),
                        })),
                    }
                }
                WatchControl::Stop { watch } => {
                    let global = self
                        .watches
                        .iter()
                        .find(|(_, record)| {
                            record.extension == id
                                && record.generation == generation
                                && record.local == watch
                        })
                        .map(|(global, _)| *global);
                    if let Some(global) = global {
                        self.watches.remove(&global);
                        effects.push(Effect::StopWatch { watch: global });
                    }
                }
            }
        }
        effects
    }
    pub fn handle_watch_event(&mut self, event: WatchEvent) -> Vec<Effect> {
        let global = match &event {
            WatchEvent::Ready { watch }
            | WatchEvent::Changed { watch, .. }
            | WatchEvent::Failed { watch, .. } => *watch,
        };
        let Some(record) = self.watches.get(&global) else {
            return vec![];
        };
        let (id, generation, watch, root) = (
            record.extension.clone(),
            record.generation,
            record.local,
            record.root,
        );
        let (message, credit) = match event {
            WatchEvent::Ready { .. } => (Inbound::WatchReady(watch), None),
            WatchEvent::Changed {
                changes, credit, ..
            } => (
                Inbound::FileChanged(FileChanged {
                    watch,
                    root,
                    changes,
                }),
                Some(credit),
            ),
            WatchEvent::Failed { message, .. } => {
                self.watches.remove(&global);
                (
                    Inbound::WatchFailed(WatchFailure {
                        watch,
                        error: crate::infrastructure::extensions::wire_error(&ExtensionError::new(
                            ErrorKind::ExtensionFailed,
                            message,
                        )),
                    }),
                    None,
                )
            }
        };
        let mut effects = if matches!(&message, Inbound::WatchFailed(_)) {
            vec![Effect::StopWatch { watch: global }]
        } else {
            vec![]
        };
        if self
            .packages
            .get(&id)
            .is_some_and(|p| p.lifecycle.accepts(generation))
        {
            effects.push(Effect::Submit(Job::Watch {
                id,
                generation,
                message,
                credit,
            }));
        }
        effects
    }
}
