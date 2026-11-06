//! What an extension found in files that are not open: bounded, replaceable by
//! (key, path), and handed to the application as changes after a round.
use std::collections::{BTreeMap, BTreeSet};

use crate::{Finding, FindingEncoding};

const MAX_KEY: usize = 64;
const MAX_PATH: usize = 4096;
/// Files one extension has findings for at once.
const MAX_FILES: usize = 4096;
/// Findings of one file.
const MAX_PER_FILE: usize = 500;
/// Findings of one extension in all.
const MAX_TOTAL: usize = 20_000;
const MAX_MESSAGE: usize = 2048;
const MAX_FIELD: usize = 128;

/// What the application should do, in this order: forget whole keys, then set
/// files. A file set after a key was forgotten belongs to the key's new life.
#[derive(Clone, Debug)]
pub struct FindingChanges {
    pub withdrawn: Vec<String>,
    pub files: Vec<FileFindings>,
}

#[derive(Clone, Debug)]
pub struct FileFindings {
    pub key: String,
    pub path: String,
    pub encoding: FindingEncoding,
    /// Empty: the file has none now.
    pub items: Vec<Finding>,
}

#[derive(Default)]
pub struct Findings {
    /// How many findings each published (key, path) holds, for the budgets.
    sizes: BTreeMap<(String, String), usize>,
    total: usize,
    withdrawn: BTreeSet<String>,
    pending: BTreeMap<(String, String), FileFindings>,
}

fn invalid(value: &str, max: usize) -> bool {
    value.is_empty() || value.len() > max || value.contains('\0')
}

fn absolute(path: &str) -> bool {
    path.starts_with('/')
        || path.starts_with("\\\\")
        || (path.len() > 2 && path.as_bytes()[1] == b':' && matches!(path.as_bytes()[2], b'\\' | b'/'))
}

impl Findings {
    pub fn publish(
        &mut self,
        key: &str,
        path: &str,
        encoding: FindingEncoding,
        items: Vec<Finding>,
    ) -> Result<(), String> {
        if invalid(key, MAX_KEY) {
            return Err("invalid findings key".into());
        }
        if invalid(path, MAX_PATH) || !absolute(path) {
            return Err("findings are published for an absolute file path".into());
        }
        if items.len() > MAX_PER_FILE {
            return Err("too many findings for one file".into());
        }
        for item in &items {
            if item.message.len() > MAX_MESSAGE || item.message.contains('\0') {
                return Err("a finding's message is at most 2 KiB".into());
            }
            if ![&item.source, &item.code]
                .into_iter()
                .flatten()
                .all(|text| text.len() <= MAX_FIELD && !text.contains('\0'))
            {
                return Err("a finding's source and code are at most 128 bytes".into());
            }
            if !(1..=4).contains(&item.severity) {
                return Err("a finding's severity is 1 to 4".into());
            }
        }
        let id = (key.to_owned(), path.to_owned());
        let before = self.sizes.get(&id).copied();
        if before.is_none() && !items.is_empty() && self.sizes.len() >= MAX_FILES {
            return Err("findings for too many files".into());
        }
        if self.total - before.unwrap_or(0) + items.len() > MAX_TOTAL {
            return Err("too many findings in all".into());
        }
        self.total = self.total - before.unwrap_or(0) + items.len();
        if items.is_empty() {
            self.sizes.remove(&id);
        } else {
            self.sizes.insert(id.clone(), items.len());
        }
        if before.is_some() || !items.is_empty() {
            self.pending.insert(
                id,
                FileFindings {
                    key: key.to_owned(),
                    path: path.to_owned(),
                    encoding,
                    items,
                },
            );
        }
        Ok(())
    }

    pub fn withdraw(&mut self, key: &str) -> Result<(), String> {
        if invalid(key, MAX_KEY) {
            return Err("invalid findings key".into());
        }
        let dropped = self
            .sizes
            .iter()
            .filter(|((k, _), _)| k == key)
            .map(|(_, n)| n)
            .sum::<usize>();
        self.sizes.retain(|(k, _), _| k != key);
        self.total -= dropped;
        self.pending.retain(|(k, _), _| k != key);
        // The application may hold findings of this key from an earlier hand-over,
        // so the key is always passed on.
        self.withdrawn.insert(key.to_owned());
        Ok(())
    }

    pub fn dirty(&self) -> bool {
        !self.pending.is_empty() || !self.withdrawn.is_empty()
    }

    pub fn take(&mut self) -> Option<FindingChanges> {
        self.dirty().then(|| FindingChanges {
            withdrawn: std::mem::take(&mut self.withdrawn).into_iter().collect(),
            files: std::mem::take(&mut self.pending).into_values().collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FindingPosition;

    fn finding(message: &str) -> Finding {
        Finding {
            start: FindingPosition { line: 1, character: 2 },
            end: FindingPosition { line: 1, character: 5 },
            severity: 1,
            message: message.into(),
            source: None,
            code: None,
        }
    }

    const FILE: &str = "/work/a.rs";

    #[test]
    fn a_file_replaces_its_findings_and_an_empty_list_takes_it_out() {
        let mut findings = Findings::default();
        findings.publish("k", FILE, FindingEncoding::Utf16, vec![finding("one")]).unwrap();
        findings.publish("k", FILE, FindingEncoding::Utf16, vec![finding("two"), finding("three")]).unwrap();
        let taken = findings.take().unwrap();
        assert_eq!(taken.files.len(), 1, "the later publication replaces the earlier");
        assert_eq!(taken.files[0].items.len(), 2);
        findings.publish("k", FILE, FindingEncoding::Utf16, vec![]).unwrap();
        assert!(findings.take().unwrap().files[0].items.is_empty());
        // Nothing was published for this file, so there is nothing to take out.
        findings.publish("k", "/work/b.rs", FindingEncoding::Utf16, vec![]).unwrap();
        assert!(findings.take().is_none());
    }

    #[test]
    fn withdrawing_a_key_discards_what_was_waiting_for_it() {
        let mut findings = Findings::default();
        findings.publish("k", FILE, FindingEncoding::Utf8, vec![finding("x")]).unwrap();
        findings.publish("other", FILE, FindingEncoding::Utf8, vec![finding("y")]).unwrap();
        findings.withdraw("k").unwrap();
        findings.publish("k", "/work/c.rs", FindingEncoding::Utf8, vec![finding("z")]).unwrap();
        let taken = findings.take().unwrap();
        assert_eq!(taken.withdrawn, ["k"]);
        let paths = taken.files.iter().map(|f| (f.key.as_str(), f.path.as_str())).collect::<Vec<_>>();
        assert_eq!(paths, [("k", "/work/c.rs"), ("other", FILE)]);
    }

    #[test]
    fn refused_publications_change_nothing() {
        let mut findings = Findings::default();
        assert!(findings.publish("", FILE, FindingEncoding::Utf8, vec![]).is_err());
        assert!(findings.publish("k", "relative.rs", FindingEncoding::Utf8, vec![finding("x")]).is_err());
        assert!(findings.publish("k", FILE, FindingEncoding::Utf8, vec![finding(&"x".repeat(MAX_MESSAGE + 1))]).is_err());
        let mut bad = finding("x");
        bad.severity = 9;
        assert!(findings.publish("k", FILE, FindingEncoding::Utf8, vec![bad]).is_err());
        let many = (0..=MAX_PER_FILE).map(|_| finding("x")).collect();
        assert!(findings.publish("k", FILE, FindingEncoding::Utf8, many).is_err());
        assert!(!findings.dirty());
    }

    #[test]
    fn budgets_count_files_and_all_findings() {
        let mut findings = Findings::default();
        for n in 0..MAX_FILES {
            findings
                .publish("k", &format!("/w/{n}.rs"), FindingEncoding::Utf8, vec![finding("x")])
                .unwrap();
        }
        assert!(findings.publish("k", "/w/extra.rs", FindingEncoding::Utf8, vec![finding("x")]).is_err());
        // A file already held may still be replaced.
        findings.publish("k", "/w/0.rs", FindingEncoding::Utf8, vec![finding("y")]).unwrap();

        let mut findings = Findings::default();
        for n in 0..(MAX_TOTAL / MAX_PER_FILE) {
            findings
                .publish("k", &format!("/w/{n}.rs"), FindingEncoding::Utf8, (0..MAX_PER_FILE).map(|_| finding("x")).collect())
                .unwrap();
        }
        assert!(findings.publish("k", "/w/over.rs", FindingEncoding::Utf8, vec![finding("x")]).is_err());
        findings.withdraw("k").unwrap();
        findings.publish("k", "/w/over.rs", FindingEncoding::Utf8, vec![finding("x")]).unwrap();
    }

    #[test]
    fn windows_paths_are_absolute_too() {
        assert!(absolute(r"D:\work\a.rs"));
        assert!(absolute("D:/work/a.rs"));
        assert!(absolute(r"\\server\share\a.rs"));
        assert!(!absolute("a.rs"));
        assert!(!absolute("src/a.rs"));
    }
}
