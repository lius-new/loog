//! Operation receipts and call-sequence de-duplication (design §7.2).
use crate::error::{ErrorKind, ExtensionError};
use std::collections::BTreeMap;

/// Lifecycle of an accepted host operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationState {
    Accepted,
    Validating,
    Committed {
        transaction: u64,
        version: u64,
    },
    /// Completion has been delivered (or recorded) and the receipt may expire.
    Settled {
        transaction: Option<u64>,
        version: u64,
    },
    Rejected,
    Cancelled,
}

impl OperationState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Settled { .. } | Self::Rejected | Self::Cancelled
        )
    }
}

/// Result of presenting a call sequence number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Begin {
    /// New operation; the host should execute it.
    Fresh,
    /// Same sequence and input: report the existing operation, do not run again.
    Duplicate(OperationState),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelOutcome {
    /// Cancelled before commit; no side effect happened.
    Cancelled,
    /// Already committed: the commit stands and the cancel could not undo it.
    TooLate(OperationState),
    AlreadyTerminal(OperationState),
}

struct Entry {
    fingerprint: u64,
    state: OperationState,
}

/// Bounded receipt store for one activation generation.
pub struct OperationLedger {
    capacity: usize,
    high_water: u64,
    entries: BTreeMap<u64, Entry>,
}

impl OperationLedger {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            high_water: 0,
            entries: BTreeMap::new(),
        }
    }

    pub fn high_water(&self) -> u64 {
        self.high_water
    }

    /// Present `sequence` with an input `fingerprint`.
    ///
    /// Sequences start at 1 and are reserved before execution, so a rejected
    /// request for lack of capacity leaves the ledger untouched.
    pub fn begin(&mut self, sequence: u64, fingerprint: u64) -> Result<Begin, ExtensionError> {
        if let Some(entry) = self.entries.get(&sequence) {
            return if entry.fingerprint == fingerprint {
                Ok(Begin::Duplicate(entry.state))
            } else {
                Err(ExtensionError::new(
                    ErrorKind::Conflict,
                    format!("sequence {sequence} was used with different input"),
                ))
            };
        }
        if sequence <= self.high_water {
            return Err(ExtensionError::new(
                ErrorKind::ExpiredHandle,
                format!("receipt for sequence {sequence} has expired; it is not executed again"),
            ));
        }
        if self.entries.len() >= self.capacity {
            return Err(ExtensionError::new(
                ErrorKind::LimitExceeded,
                "receipt capacity is exhausted",
            ));
        }
        self.high_water = sequence;
        self.entries.insert(
            sequence,
            Entry {
                fingerprint,
                state: OperationState::Accepted,
            },
        );
        Ok(Begin::Fresh)
    }

    pub fn state(&self, sequence: u64) -> Option<OperationState> {
        self.entries.get(&sequence).map(|entry| entry.state)
    }

    fn entry(&mut self, sequence: u64) -> Result<&mut Entry, ExtensionError> {
        self.entries.get_mut(&sequence).ok_or_else(|| {
            ExtensionError::new(
                ErrorKind::ExpiredHandle,
                format!("unknown sequence {sequence}"),
            )
        })
    }

    pub fn validating(&mut self, sequence: u64) -> Result<(), ExtensionError> {
        let entry = self.entry(sequence)?;
        match entry.state {
            OperationState::Accepted => {
                entry.state = OperationState::Validating;
                Ok(())
            }
            OperationState::Cancelled => Err(ExtensionError::new(
                ErrorKind::Cancelled,
                "cancelled before validation",
            )),
            other => Err(ExtensionError::new(
                ErrorKind::Conflict,
                format!("cannot validate from {other:?}"),
            )),
        }
    }

    /// Record the commit point. A cancelled or rejected operation never commits.
    pub fn commit(
        &mut self,
        sequence: u64,
        transaction: u64,
        version: u64,
    ) -> Result<(), ExtensionError> {
        let entry = self.entry(sequence)?;
        match entry.state {
            OperationState::Accepted | OperationState::Validating => {
                entry.state = OperationState::Committed {
                    transaction,
                    version,
                };
                Ok(())
            }
            OperationState::Cancelled => Err(ExtensionError::new(
                ErrorKind::Cancelled,
                "cancelled before commit",
            )),
            other => Err(ExtensionError::new(
                ErrorKind::Conflict,
                format!("cannot commit from {other:?}"),
            )),
        }
    }

    pub fn reject(&mut self, sequence: u64) -> Result<(), ExtensionError> {
        let entry = self.entry(sequence)?;
        match entry.state {
            OperationState::Accepted | OperationState::Validating => {
                entry.state = OperationState::Rejected;
                Ok(())
            }
            other => Err(ExtensionError::new(
                ErrorKind::Conflict,
                format!("cannot reject from {other:?}"),
            )),
        }
    }

    pub fn cancel(&mut self, sequence: u64) -> Result<CancelOutcome, ExtensionError> {
        let entry = self.entry(sequence)?;
        Ok(match entry.state {
            OperationState::Accepted | OperationState::Validating => {
                entry.state = OperationState::Cancelled;
                CancelOutcome::Cancelled
            }
            state @ OperationState::Committed { .. } => CancelOutcome::TooLate(state),
            state => CancelOutcome::AlreadyTerminal(state),
        })
    }

    /// Delivery of the final result to the guest (or its recording for diagnostics).
    pub fn settle(&mut self, sequence: u64) -> Result<(), ExtensionError> {
        let entry = self.entry(sequence)?;
        entry.state = match entry.state {
            OperationState::Committed {
                transaction,
                version,
            } => OperationState::Settled {
                transaction: Some(transaction),
                version,
            },
            OperationState::Accepted | OperationState::Validating => {
                return Err(ExtensionError::new(
                    ErrorKind::Conflict,
                    "an uncommitted operation must be rejected or cancelled first",
                ));
            }
            other => other,
        };
        Ok(())
    }

    /// Drop the detail of a terminal receipt. Its sequence stays below the
    /// high-water mark, so presenting it again reports `expired-handle`.
    pub fn release(&mut self, sequence: u64) -> bool {
        match self.entries.get(&sequence) {
            Some(entry) if entry.state.is_terminal() => self.entries.remove(&sequence).is_some(),
            _ => false,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_input_is_a_duplicate_different_input_conflicts() {
        let mut ledger = OperationLedger::new(8);
        assert_eq!(ledger.begin(1, 42).unwrap(), Begin::Fresh);
        assert_eq!(
            ledger.begin(1, 42).unwrap(),
            Begin::Duplicate(OperationState::Accepted)
        );
        assert_eq!(ledger.begin(1, 43).unwrap_err().kind, ErrorKind::Conflict);
    }

    #[test]
    fn cancel_before_commit_blocks_the_side_effect() {
        let mut ledger = OperationLedger::new(8);
        ledger.begin(1, 1).unwrap();
        assert_eq!(ledger.cancel(1).unwrap(), CancelOutcome::Cancelled);
        assert_eq!(
            ledger.commit(1, 7, 2).unwrap_err().kind,
            ErrorKind::Cancelled
        );
    }

    #[test]
    fn cancel_after_commit_reports_the_commit() {
        let mut ledger = OperationLedger::new(8);
        ledger.begin(1, 1).unwrap();
        ledger.validating(1).unwrap();
        ledger.commit(1, 7, 2).unwrap();
        assert_eq!(
            ledger.cancel(1).unwrap(),
            CancelOutcome::TooLate(OperationState::Committed {
                transaction: 7,
                version: 2
            })
        );
        ledger.settle(1).unwrap();
        assert_eq!(
            ledger.state(1),
            Some(OperationState::Settled {
                transaction: Some(7),
                version: 2
            })
        );
    }

    #[test]
    fn released_receipts_are_expired_not_re_executed() {
        let mut ledger = OperationLedger::new(8);
        ledger.begin(1, 1).unwrap();
        ledger.commit(1, 7, 2).unwrap();
        assert!(
            !ledger.release(1),
            "a committed but unsettled receipt is kept"
        );
        ledger.settle(1).unwrap();
        assert!(ledger.release(1));
        assert_eq!(
            ledger.begin(1, 1).unwrap_err().kind,
            ErrorKind::ExpiredHandle
        );
        assert_eq!(ledger.begin(2, 9).unwrap(), Begin::Fresh);
    }

    #[test]
    fn capacity_is_reserved_before_execution() {
        let mut ledger = OperationLedger::new(1);
        ledger.begin(1, 1).unwrap();
        assert_eq!(
            ledger.begin(2, 1).unwrap_err().kind,
            ErrorKind::LimitExceeded
        );
        assert_eq!(
            ledger.high_water(),
            1,
            "the rejected sequence was not recorded"
        );
        ledger.reject(1).unwrap();
        assert!(ledger.release(1));
        assert_eq!(ledger.begin(2, 1).unwrap(), Begin::Fresh);
    }

    #[test]
    fn an_uncommitted_operation_cannot_be_settled() {
        let mut ledger = OperationLedger::new(2);
        ledger.begin(1, 1).unwrap();
        assert!(ledger.settle(1).is_err());
    }
}
