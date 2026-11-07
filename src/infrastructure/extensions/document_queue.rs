//! A slow guest retains the latest document state, rather than a history of full texts.
use loog_extension_host::{DocumentRef, DocumentView, Inbound};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Default)]
pub struct Queue {
    pub queued: bool,
    port: bool,
    views: BTreeMap<u64, DocumentView>,
    closed: BTreeSet<u64>,
    installed: BTreeSet<u64>,
    references: BTreeMap<u64, DocumentRef>,
    delivered: BTreeMap<u64, DocumentRef>,
    events: BTreeMap<u64, Inbound>,
    saved: BTreeMap<u64, (DocumentRef, std::sync::Arc<String>)>,
    files: Vec<loog_extension_host::WorkspaceFileChange>,
}

pub struct Batch {
    pub port: bool,
    pub views: Vec<DocumentView>,
    pub closed: Vec<u64>,
    pub events: Vec<Inbound>,
    pub saved: Vec<(DocumentRef, std::sync::Arc<String>)>,
    pub files: Vec<loog_extension_host::WorkspaceFileChange>,
}

impl Queue {
    pub fn documents(&mut self, changed: Vec<DocumentView>, closed: Vec<u64>) {
        self.port = true;
        for id in closed {
            self.saved.remove(&id);
            self.views.remove(&id);
            if self.installed.contains(&id) {
                self.closed.insert(id);
            }
        }
        for view in changed {
            let id = view.reference.document;
            self.closed.remove(&id);
            self.views.insert(id, view);
        }
    }
    pub fn saved(&mut self, reference: DocumentRef, text: std::sync::Arc<String>) {
        self.saved.insert(reference.document, (reference, text));
    }
    pub fn files(&mut self, changes: Vec<loog_extension_host::WorkspaceFileChange>) {
        merge_files(&mut self.files, changes);
    }

    pub fn events(&mut self, references: Vec<DocumentRef>, messages: Vec<Inbound>) {
        for reference in references {
            self.references.insert(reference.document, reference);
        }
        for message in messages {
            let id = match &message {
                Inbound::DocumentOpened(r)
                | Inbound::DocumentResync(r)
                | Inbound::DocumentClosed(r) => r.document,
                Inbound::DocumentChanged(c) => c.document,
                _ => unreachable!("document queue accepts only document events"),
            };
            let previous = self.events.remove(&id);
            let next = match message {
                Inbound::DocumentClosed(reference) => {
                    self.references.remove(&id);
                    // A pending reopen may have a lifecycle the guest has never seen.
                    self.delivered
                        .get(&id)
                        .cloned()
                        .map(Inbound::DocumentClosed)
                        .or({
                            if matches!(previous, Some(Inbound::DocumentOpened(_))) {
                                None
                            } else {
                                Some(Inbound::DocumentClosed(reference))
                            }
                        })
                }
                Inbound::DocumentChanged(change) => {
                    let reference = self.references.get(&id).expect("change metadata").clone();
                    match previous {
                        Some(Inbound::DocumentOpened(_)) => {
                            Some(Inbound::DocumentOpened(reference))
                        }
                        Some(Inbound::DocumentChanged(mut before))
                            if before.lifecycle == change.lifecycle
                                && before.to_version == change.from_version
                                && before.edits.saturating_add(change.edits) <= 256 =>
                        {
                            before.to_version = change.to_version;
                            before.edits += change.edits;
                            Some(Inbound::DocumentChanged(before))
                        }
                        None => Some(Inbound::DocumentChanged(change)),
                        _ => Some(Inbound::DocumentResync(reference)),
                    }
                }
                other => Some(other),
            };
            if let Some(next) = next {
                self.events.insert(id, next);
            }
        }
    }

    pub fn drain(&mut self) -> Batch {
        self.queued = false;
        let views = std::mem::take(&mut self.views)
            .into_values()
            .collect::<Vec<_>>();
        let closed = std::mem::take(&mut self.closed)
            .into_iter()
            .collect::<Vec<_>>();
        for id in &closed {
            self.installed.remove(id);
        }
        for view in &views {
            self.installed.insert(view.reference.document);
        }
        let events = std::mem::take(&mut self.events)
            .into_values()
            .collect::<Vec<_>>();
        for event in &events {
            match event {
                Inbound::DocumentOpened(r) | Inbound::DocumentResync(r) => {
                    self.delivered.insert(r.document, r.clone());
                }
                Inbound::DocumentChanged(c) => {
                    if let Some(r) = self.references.get(&c.document) {
                        self.delivered.insert(c.document, r.clone());
                    }
                }
                Inbound::DocumentClosed(r) => {
                    self.delivered.remove(&r.document);
                }
                _ => unreachable!(),
            }
        }
        Batch {
            port: self.port,
            views,
            closed,
            events,
            saved: std::mem::take(&mut self.saved).into_values().collect(),
            files: std::mem::take(&mut self.files),
        }
    }
}

pub fn merge_files(
    pending: &mut Vec<loog_extension_host::WorkspaceFileChange>,
    changes: Vec<loog_extension_host::WorkspaceFileChange>,
) {
    use loog_extension_host::WorkspaceFileChange::Rescan;
    if matches!(pending.first(), Some(Rescan)) {
        return;
    }
    if pending.len().saturating_add(changes.len()) > 1024
        || changes.iter().any(|c| matches!(c, Rescan))
    {
        pending.clear();
        pending.push(Rescan);
    } else {
        pending.extend(changes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loog_extension_host::DocumentChange;
    use std::sync::Arc;
    fn reference(lifecycle: u64, version: u64) -> DocumentRef {
        DocumentRef {
            document: 1,
            lifecycle,
            version,
            path: "a.rs".into(),
            language: "rust".into(),
            variant: None,
        }
    }
    fn change(from: u64, to: u64) -> Inbound {
        Inbound::DocumentChanged(DocumentChange {
            document: 1,
            lifecycle: 1,
            from_version: from,
            to_version: to,
            edits: 1,
        })
    }
    #[test]
    fn a_slow_guest_keeps_one_text_and_a_contiguous_summary_or_resync() {
        let mut q = Queue::default();
        q.events(
            vec![reference(1, 0)],
            vec![Inbound::DocumentOpened(reference(1, 0))],
        );
        q.drain();
        for version in 1..1000 {
            q.documents(
                vec![DocumentView {
                    reference: reference(1, version),
                    text: Some(Arc::new(version.to_string())),
                }],
                vec![],
            );
            q.events(
                vec![reference(1, version)],
                vec![change(version - 1, version)],
            );
        }
        let batch = q.drain();
        assert_eq!(batch.views.len(), 1);
        assert_eq!(batch.views[0].text.as_ref().unwrap().as_str(), "999");
        assert!(matches!(&batch.events[..], [Inbound::DocumentResync(r)] if r.version == 999));
        q.events(vec![reference(1, 1000)], vec![change(999, 1000)]);
        q.events(vec![reference(1, 1001)], vec![change(1000, 1001)]);
        assert!(
            matches!(&q.drain().events[..], [Inbound::DocumentChanged(c)] if c.from_version == 999 && c.to_version == 1001)
        );
    }
    #[test]
    fn unseen_opens_disappear_and_close_uses_the_delivered_lifecycle() {
        let mut q = Queue::default();
        q.events(
            vec![reference(1, 0)],
            vec![
                Inbound::DocumentOpened(reference(1, 0)),
                Inbound::DocumentClosed(reference(1, 0)),
            ],
        );
        assert!(q.drain().events.is_empty());
        assert!(q.references.is_empty());
        q.events(
            vec![reference(1, 0)],
            vec![Inbound::DocumentOpened(reference(1, 0))],
        );
        q.drain();
        q.events(
            vec![reference(2, 4)],
            vec![
                Inbound::DocumentOpened(reference(2, 4)),
                Inbound::DocumentClosed(reference(2, 4)),
            ],
        );
        assert!(matches!(&q.drain().events[..], [Inbound::DocumentClosed(r)] if r.lifecycle == 1));
        assert!(q.delivered.is_empty());
    }
    #[test]
    fn an_unseen_document_does_not_accumulate_close_records() {
        let mut q = Queue::default();
        q.documents(
            vec![DocumentView {
                reference: reference(1, 0),
                text: Some(Arc::new("x".into())),
            }],
            vec![],
        );
        q.documents(vec![], vec![1]);
        let batch = q.drain();
        assert!(batch.views.is_empty());
        assert!(batch.closed.is_empty());
    }
    #[test]
    fn a_close_only_subscriber_receives_its_boundary_and_file_overflow_requests_a_rescan() {
        let mut q = Queue::default();
        q.events(
            vec![reference(1, 0)],
            vec![Inbound::DocumentClosed(reference(1, 0))],
        );
        assert!(matches!(
            &q.drain().events[..],
            [Inbound::DocumentClosed(_)]
        ));
        let changes = (0..1025)
            .map(|i| loog_extension_host::WorkspaceFileChange::Created(i.to_string()))
            .collect();
        q.files(changes);
        assert!(matches!(
            &q.drain().files[..],
            [loog_extension_host::WorkspaceFileChange::Rescan]
        ));
    }
}
