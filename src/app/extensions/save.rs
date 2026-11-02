//! Save participants (design §6.5): extensions that change a file just before
//! it is saved.
//!
//! The participants run one after another on a temporary copy of the text, each
//! reading what the one before it produced. Only when all of them have answered
//! is the result compared with the real text and committed in one step; if any
//! of them fails, times out or answers with something unusable, the copy is
//! thrown away and the real document is untouched.
use super::*;
use crate::features::extensions::savediff;
use std::collections::VecDeque;
use std::sync::Arc;

/// Time the whole chain gets, queueing and cold starts included.
pub const SAVE_DEADLINE_MS: u64 = 3_000;
/// Times a chain is planned again because the text moved on under it.
const REPLANS: u32 = 1;

/// The file a chain is working on.
struct Current {
    file: FileId,
    stamp: DocumentStamp,
    path: PathBuf,
    doc_id: u64,
    version: u64,
    original: Arc<String>,
    temp: Arc<String>,
    ending: &'static str,
    participants: Vec<(String, String)>,
    step: usize,
    request: Option<u64>,
    replans: u32,
}

pub(super) struct Chain {
    files: VecDeque<FileId>,
    current: Option<Current>,
    deadline: u64,
}

impl ExtensionService {
    /// Participants that apply to `file`, in the order they run: stable by
    /// provider id (a user-configured order can refine this later).
    fn participants_of(&self, app: &AppState, file: FileId) -> Vec<(String, String)> {
        let Some(document) = app.workspace.documents.get(&file) else {
            return Vec::new();
        };
        let language = document.meta.lang.definition().name.to_string();
        let mut found = Vec::new();
        for (owner, package) in &self.packages {
            let allowed = ["documents.read", "workspace.edit"]
                .iter()
                .all(|c| self.authorize(package, c).is_ok());
            if !allowed || !package.manifest.has_behaviour() {
                continue;
            }
            for provider in &package.manifest.contributes.language_service.providers {
                if provider.kind == DeclKind::SaveParticipant
                    && provider.selector.languages.contains(&language)
                    && package
                        .provider_configurations
                        .get(&provider.id)
                        .is_none_or(|c| c.enabled)
                {
                    found.push((provider.id.clone(), owner.clone()));
                }
            }
        }
        found.sort();
        found
            .into_iter()
            .map(|(provider, owner)| (owner, provider))
            .collect()
    }

    /// Whether any participant would run for these files.
    pub fn has_participants(&self, app: &AppState, files: &[FileId]) -> bool {
        files
            .iter()
            .any(|file| !self.participants_of(app, *file).is_empty())
    }

    /// Whether a chain is already working on `file`.
    pub fn saving(&self, file: FileId) -> bool {
        self.saves.values().any(|chain| {
            chain.current.as_ref().is_some_and(|c| c.file == file) || chain.files.contains(&file)
        })
    }

    /// Start a chain over `files`. Its end arrives as [`Effect::SaveReady`].
    pub fn begin_save(&mut self, app: &mut AppState, files: Vec<FileId>) -> (u64, Vec<Effect>) {
        self.next_save += 1;
        let id = self.next_save;
        self.saves.insert(
            id,
            Chain {
                files: files.into(),
                current: None,
                deadline: (self.clock)() + SAVE_DEADLINE_MS,
            },
        );
        let mut effects = Vec::new();
        self.advance(app, id, &mut effects);
        (id, effects)
    }

    /// Start working on the next file that has participants, or finish.
    fn next_file(&mut self, app: &AppState, id: u64) -> bool {
        loop {
            let Some(chain) = self.saves.get_mut(&id) else {
                return false;
            };
            let Some(file) = chain.files.pop_front() else {
                chain.current = None;
                return false;
            };
            let participants = self.participants_of(app, file);
            let Some(document) = app
                .workspace
                .documents
                .get(&file)
                .filter(|_| !participants.is_empty())
            else {
                continue;
            };
            let doc_id = match self.seen.get(&file) {
                Some(doc_id) => *doc_id,
                None => {
                    self.next_document += 1;
                    self.seen.insert(file, self.next_document);
                    self.next_document
                }
            };
            let text = document.buffer.text_snapshot();
            let current = Current {
                file,
                stamp: document.stamp(file),
                path: document.meta.path.clone(),
                doc_id,
                version: document.buffer.version(),
                original: text.clone(),
                temp: text,
                ending: document.buffer.line_ending(),
                participants,
                step: 0,
                request: None,
                replans: 0,
            };
            self.saves.get_mut(&id).expect("chain exists").current = Some(current);
            return true;
        }
    }

    /// Ask the next participant, or commit when all have answered.
    fn advance(&mut self, app: &mut AppState, id: u64, effects: &mut Vec<Effect>) {
        loop {
            let needs_file = self.saves.get(&id).is_some_and(|c| c.current.is_none());
            if needs_file && !self.next_file(app, id) {
                if self.saves.contains_key(&id) {
                    // Every file went through: the real commit happens per file in
                    // `answered`; nothing is left to ask.
                    self.saves.remove(&id);
                    effects.push(Effect::SaveReady {
                        chain: id,
                        ok: true,
                        message: None,
                    });
                }
                return;
            }
            let Some(chain) = self.saves.get(&id) else {
                return;
            };
            let current = chain.current.as_ref().expect("a current file");
            if current.step < current.participants.len() {
                let (owner, provider) = current.participants[current.step].clone();
                let target = Target {
                    file: current.file,
                    stamp: current.stamp,
                    path: current.path.clone(),
                    text: current.temp.clone(),
                    doc_id: current.doc_id,
                    version: current.version,
                    range: None,
                    word_start: 0,
                    cursor: None,
                    trigger: None,
                    item: None,
                    item_raw: None,
                };
                let deadline = chain.deadline;
                let before = self.next_request;
                self.issue(&owner, &provider, WireKind::Save, target, effects);
                let issued = self.next_request > before
                    && self.provider_calls.contains_key(&self.next_request);
                if !issued {
                    self.fail(id, format!("{owner} is not running"), effects);
                    return;
                }
                let request = self.next_request;
                if let Some(call) = self.provider_calls.get_mut(&request) {
                    // Cold activation and queueing eat into the one chain deadline.
                    call.deadline = deadline;
                }
                self.save_calls.insert(request, id);
                if let Some(current) = self.saves.get_mut(&id).and_then(|c| c.current.as_mut()) {
                    current.request = Some(request);
                }
                return;
            }
            // All participants answered for this file.
            match self.commit_current(app, id) {
                Ok(true) => {
                    if let Some(chain) = self.saves.get_mut(&id) {
                        chain.current = None;
                    }
                }
                // The text moved on: the same participants run again on the new text.
                Ok(false) => {}
                Err(message) => {
                    self.fail(id, message, effects);
                    return;
                }
            }
        }
    }

    /// A participant answered. Returns what to do next.
    pub(super) fn save_answered(
        &mut self,
        app: &mut AppState,
        request: u64,
        edits: Vec<loom_extension_host::TextEdit>,
    ) -> Vec<Effect> {
        let mut effects = Vec::new();
        let Some(id) = self.save_calls.remove(&request) else {
            return effects;
        };
        let Some(chain) = self.saves.get(&id) else {
            return effects;
        };
        if (self.clock)() > chain.deadline {
            self.fail(
                id,
                "a save participant answered too late".into(),
                &mut effects,
            );
            return effects;
        }
        let current = chain.current.as_ref().expect("a current file");
        if current.request != Some(request) {
            return effects;
        }
        let wire: Vec<loom_extension_api::edit::TextEdit> = edits
            .iter()
            .map(|e| loom_extension_api::edit::TextEdit {
                range: loom_extension_api::edit::ByteRange::new(
                    e.range.start as usize,
                    e.range.end as usize,
                ),
                text: loom_editor_core::text::normalize_endings(&e.text, current.ending),
            })
            .collect();
        match loom_extension_api::edit::apply(&current.temp, &wire) {
            Err(error) => {
                let who = current.participants[current.step].0.clone();
                self.fail(id, format!("{who}: {error}"), &mut effects);
            }
            Ok(next) if next.len() > self.limits.output_bytes => {
                let who = current.participants[current.step].0.clone();
                self.fail(id, format!("{who}: the result is too large"), &mut effects);
            }
            Ok(next) => {
                if let Some(current) = self.saves.get_mut(&id).and_then(|c| c.current.as_mut()) {
                    current.temp = Arc::new(next);
                    current.step += 1;
                    current.request = None;
                }
                self.advance(app, id, &mut effects);
            }
        }
        effects
    }

    /// Compare the chain's result with the real text and commit it as one step.
    /// `Ok(true)` means the file is done, `Ok(false)` that it is planned again
    /// because the person kept typing; a second change is an error.
    fn commit_current(&mut self, app: &mut AppState, id: u64) -> Result<bool, String> {
        let current = self
            .saves
            .get_mut(&id)
            .and_then(|chain| chain.current.as_mut())
            .expect("a current file");
        let edits = savediff::edits(&current.original, &current.temp);
        if edits.is_empty() {
            return Ok(true);
        }
        let Some(document) = app.workspace.documents.get(&current.file) else {
            return Err("the file was closed while saving".into());
        };
        if document.stamp(current.file) != current.stamp {
            if current.replans >= REPLANS {
                return Err("the file changed while the save participants worked".into());
            }
            let text = document.buffer.text_snapshot();
            current.stamp = document.stamp(current.file);
            current.version = document.buffer.version();
            current.original = text.clone();
            current.temp = text;
            current.ending = document.buffer.line_ending();
            current.step = 0;
            current.request = None;
            current.replans += 1;
            return Ok(false);
        }
        let transaction = TextTransaction {
            title: "Save".into(),
            documents: vec![DocumentChange {
                file: current.file,
                stamp: current.stamp,
                edits,
            }],
        };
        workspace_edits::commit(app, &transaction, &self.limits)
            .map(|_| true)
            .map_err(|error| error.to_string())
    }

    /// End a chain without changing anything; participants still holding the
    /// restriction are released.
    pub(super) fn fail(&mut self, id: u64, message: String, effects: &mut Vec<Effect>) {
        if let Some(chain) = self.saves.remove(&id)
            && let Some(current) = chain.current
            && let Some(request) = current.request
        {
            self.save_calls.remove(&request);
            if let Some(call) = self.provider_calls.remove(&request) {
                let generation = call.generation;
                effects.push(Effect::Submit(Job::EndSave {
                    id: call.extension,
                    generation,
                    request,
                }));
            }
        }
        effects.push(Effect::SaveReady {
            chain: id,
            ok: false,
            message: Some(message),
        });
    }

    /// The chain's time ran out while a participant was still working.
    pub fn expire_save(&mut self, chain: u64) -> Vec<Effect> {
        let mut effects = Vec::new();
        let due = self
            .saves
            .get(&chain)
            .is_some_and(|c| (self.clock)() >= c.deadline);
        if due {
            self.fail(
                chain,
                "a save participant did not answer in time".into(),
                &mut effects,
            );
        }
        effects
    }
}
