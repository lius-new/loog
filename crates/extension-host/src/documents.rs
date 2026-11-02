use super::*;
#[derive(Clone, Debug)]
pub struct DocumentView {
    pub reference: DocumentRef,
    pub text: Option<Arc<String>>,
}
#[derive(Default)]
pub(super) struct Documents {
    views: BTreeMap<u64, DocumentView>,
    captured: BTreeMap<u64, usize>,
    next: u64,
    bytes: usize,
}
impl Documents {
    pub fn update(&mut self, changed: Vec<DocumentView>, closed: &[u64]) {
        for id in closed {
            self.views.remove(id);
        }
        for view in changed {
            self.views.insert(view.reference.document, view);
        }
    }
    pub fn capture(&mut self, document: u64) -> Result<(DocumentSnapshot, Arc<String>), HostError> {
        let view = self.views.get(&document).ok_or_else(|| {
            host_error(ErrorKind::ExpiredHandle, "the document is no longer open")
        })?;
        let text = view.text.as_ref().ok_or_else(|| {
            host_error(
                ErrorKind::LimitExceeded,
                "the document exceeds the synchronization limit",
            )
        })?;
        let reference = view.reference.clone();
        let text = text.clone();
        self.capture_text(reference, text)
    }
    pub fn capture_text(
        &mut self,
        reference: DocumentRef,
        text: Arc<String>,
    ) -> Result<(DocumentSnapshot, Arc<String>), HostError> {
        if self.captured.len() >= 32 || self.bytes.saturating_add(text.len()) > 64 * 1024 * 1024 {
            return Err(host_error(
                ErrorKind::LimitExceeded,
                "captured document budget exceeded",
            ));
        }
        self.next += 1;
        let snapshot = (1 << 63) | self.next;
        self.bytes += text.len();
        self.captured.insert(snapshot, text.len());
        Ok((
            DocumentSnapshot {
                snapshot,
                document: reference,
                length: text.len() as u64,
            },
            text,
        ))
    }
    pub fn release(&mut self, snapshot: u64) {
        if let Some(bytes) = self.captured.remove(&snapshot) {
            self.bytes -= bytes;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn view(version: u64, text: Option<&str>) -> DocumentView {
        DocumentView {
            reference: DocumentRef {
                document: 1,
                lifecycle: 1,
                path: "/file.txt".into(),
                language: "text".into(),
                variant: None,
                version,
            },
            text: text.map(|text| Arc::new(text.to_owned())),
        }
    }
    #[test]
    fn snapshots_are_immutable_and_closed_or_oversized_documents_cannot_be_captured() {
        let mut documents = Documents::default();
        documents.update(vec![view(0, Some("中😀"))], &[]);
        let (first, text) = documents.capture(1).unwrap();
        assert_eq!(first.length, 7);
        documents.update(vec![view(1, Some("new"))], &[]);
        assert_eq!(text.as_str(), "中😀");
        assert_eq!(documents.capture(1).unwrap().0.document.version, 1);
        documents.update(vec![view(2, None)], &[]);
        assert!(documents.capture(1).is_err());
        documents.update(vec![], &[1]);
        assert!(documents.capture(1).is_err());
        documents.release(first.snapshot);
        assert_eq!(text.as_str(), "中😀");
    }
    #[test]
    fn releasing_captures_returns_handle_and_byte_credits() {
        let mut documents = Documents::default();
        documents.update(vec![view(0, Some("x"))], &[]);
        let mut snapshots = vec![];
        for _ in 0..32 {
            snapshots.push(documents.capture(1).unwrap().0.snapshot);
        }
        assert!(documents.capture(1).is_err());
        documents.release(snapshots[0]);
        assert!(documents.capture(1).is_ok());
    }
}
