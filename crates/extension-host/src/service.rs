//! The state an extension reports about the service it runs: bounded, replaceable by
//! key, and read by the application as one snapshot after a round.
use std::collections::BTreeMap;

use crate::ServiceReport;

/// Most sessions of one extension that report at once.
const MAX_REPORTS: usize = 16;
const MAX_KEY: usize = 64;
const MAX_DETAIL: usize = 200;
const MAX_CONTEXTS: usize = 256;
const MAX_CONTEXT: usize = 4096;
const MAX_CONTEXT_BYTES: usize = 32768;

#[derive(Default)]
pub struct Reports {
    reports: BTreeMap<String, ServiceReport>,
    dirty: bool,
}

impl Reports {
    /// Publish or replace the report of `report.key`. A refused report leaves the
    /// earlier one as it was.
    pub fn publish(&mut self, mut report: ServiceReport) -> Result<(), String> {
        if report.key.is_empty() || report.key.len() > MAX_KEY || report.key.contains('\0') {
            return Err("invalid service status key".into());
        }
        if let Some(detail) = &report.detail {
            if detail.len() > MAX_DETAIL || detail.contains(['\0', '\n', '\r']) {
                return Err("a service status detail is one line of at most 200 bytes".into());
            }
        }
        if let Some(contexts) = &mut report.contexts {
            if contexts.len() > MAX_CONTEXTS
                || contexts.iter().any(|value| {
                    value.is_empty() || value.len() > MAX_CONTEXT || value.contains('\0')
                })
                || contexts.iter().map(String::len).sum::<usize>() > MAX_CONTEXT_BYTES
            {
                return Err("service status contexts exceed their budget".into());
            }
            contexts.sort();
            contexts.dedup();
        }
        if !self.reports.contains_key(&report.key) && self.reports.len() >= MAX_REPORTS {
            return Err("too many service status reports".into());
        }
        let same = self.reports.get(&report.key).is_some_and(|known| {
            known.state == report.state
                && known.detail == report.detail
                && known.contexts == report.contexts
        });
        if !same {
            self.reports.insert(report.key.clone(), report);
            self.dirty = true;
        }
        Ok(())
    }

    pub fn withdraw(&mut self, key: &str) -> Result<(), String> {
        if self.reports.remove(key).is_some() {
            self.dirty = true;
        }
        Ok(())
    }

    pub fn dirty(&self) -> bool {
        self.dirty
    }

    /// Everything reported now, once after a change.
    pub fn take(&mut self) -> Option<Vec<ServiceReport>> {
        std::mem::take(&mut self.dirty).then(|| self.reports.values().cloned().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ServiceState;

    fn report(key: &str, state: ServiceState) -> ServiceReport {
        ServiceReport {
            key: key.into(),
            state,
            detail: None,
            contexts: None,
        }
    }

    #[test]
    fn a_report_replaces_the_one_of_its_key_and_only_a_change_is_taken() {
        let mut reports = Reports::default();
        assert!(reports.take().is_none());
        reports
            .publish(report("a", ServiceState::Starting))
            .unwrap();
        reports.publish(report("a", ServiceState::Ready)).unwrap();
        let taken = reports.take().unwrap();
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].state, ServiceState::Ready);
        reports.publish(report("a", ServiceState::Ready)).unwrap();
        assert!(!reports.dirty(), "the same report is no change");
        reports.withdraw("a").unwrap();
        assert!(reports.take().unwrap().is_empty());
    }

    #[test]
    fn a_refused_report_leaves_the_earlier_one() {
        let mut reports = Reports::default();
        reports.publish(report("a", ServiceState::Working)).unwrap();
        reports.take();
        let mut bad = report("a", ServiceState::Error);
        bad.detail = Some("two\nlines".into());
        assert!(reports.publish(bad).is_err());
        assert!(reports.publish(report("", ServiceState::Ready)).is_err());
        assert!(!reports.dirty());
        reports.publish(report("a", ServiceState::Working)).unwrap();
        assert_eq!(reports.reports["a"].state, ServiceState::Working);
    }

    #[test]
    fn the_number_of_sessions_and_the_contexts_are_bounded_and_sorted() {
        let mut reports = Reports::default();
        for index in 0..MAX_REPORTS {
            reports
                .publish(report(&format!("k{index}"), ServiceState::Ready))
                .unwrap();
        }
        assert!(
            reports
                .publish(report("extra", ServiceState::Ready))
                .is_err()
        );
        // Replacing one of them is not another session.
        reports
            .publish(report("k0", ServiceState::Working))
            .unwrap();
        let mut scoped = report("k1", ServiceState::Ready);
        scoped.contexts = Some(vec!["b".into(), "a".into(), "b".into()]);
        reports.publish(scoped).unwrap();
        assert_eq!(
            reports.reports["k1"].contexts,
            Some(vec!["a".to_owned(), "b".to_owned()])
        );
        let mut huge = report("k2", ServiceState::Ready);
        huge.contexts = Some(vec!["x".repeat(MAX_CONTEXT + 1)]);
        assert!(reports.publish(huge).is_err());
    }
}
