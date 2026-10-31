//! Admission limits for one atomic edit transaction (design §6.2.1).
//!
//! The limits are checked before any large text is read or copied. A request
//! over a limit is rejected with `limit-exceeded`; the host never splits it
//! into several visible commits.
use crate::error::{ErrorKind, ExtensionError};
use std::collections::BTreeMap;

pub const MIB: usize = 1024 * 1024;

/// Candidate per-transaction limits from the design; frozen after M0 measurement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditLimits {
    pub documents: usize,
    pub edits: usize,
    pub input_bytes: usize,
    pub insert_bytes: usize,
    pub output_bytes: usize,
    pub retained_bytes: usize,
    /// Views plus anchors touched by the transaction.
    pub view_work: usize,
}

impl Default for EditLimits {
    fn default() -> Self {
        Self {
            documents: 16,
            edits: 1024,
            input_bytes: 8 * MIB,
            insert_bytes: 2 * MIB,
            output_bytes: 10 * MIB,
            retained_bytes: 64 * MIB,
            view_work: 4096,
        }
    }
}

/// What one affected document contributes to the transaction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DocumentLoad {
    /// Full pre-edit text size, not only the edited ranges.
    pub input_bytes: usize,
    pub edits: usize,
    pub insert_bytes: usize,
    /// Full post-edit text size.
    pub output_bytes: usize,
    pub views: usize,
    pub anchors: usize,
}

/// Which dimension rejected a transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dimension {
    Documents,
    Edits,
    InputBytes,
    InsertBytes,
    OutputBytes,
    RetainedBytes,
    ViewWork,
}

impl Dimension {
    pub fn name(self) -> &'static str {
        match self {
            Self::Documents => "documents",
            Self::Edits => "edits",
            Self::InputBytes => "input-bytes",
            Self::InsertBytes => "insert-bytes",
            Self::OutputBytes => "output-bytes",
            Self::RetainedBytes => "retained-bytes",
            Self::ViewWork => "view-work",
        }
    }
}

/// Conservative retention estimate: input and output texts plus the insert
/// payloads and the change/index bookkeeping kept until the commit settles.
fn retained(input: usize, insert: usize, output: usize, edits: usize) -> usize {
    input
        .saturating_add(output)
        .saturating_add(insert)
        .saturating_add(edits.saturating_mul(64))
}

/// Check a whole transaction. Documents are keyed by identity so duplicates
/// (including documents that are not open) are counted once.
pub fn admit(limits: &EditLimits, loads: &[(u64, DocumentLoad)]) -> Result<(), ExtensionError> {
    let mut merged: BTreeMap<u64, DocumentLoad> = BTreeMap::new();
    for (id, load) in loads {
        let slot = merged.entry(*id).or_default();
        slot.edits += load.edits;
        slot.insert_bytes += load.insert_bytes;
        // The same document contributes its full text once.
        slot.input_bytes = slot.input_bytes.max(load.input_bytes);
        slot.output_bytes = slot.output_bytes.max(load.output_bytes);
        slot.views = slot.views.max(load.views);
        slot.anchors = slot.anchors.max(load.anchors);
    }
    let total = |field: fn(&DocumentLoad) -> usize| {
        merged
            .values()
            .map(field)
            .fold(0usize, usize::saturating_add)
    };
    let edits = total(|d| d.edits);
    let input = total(|d| d.input_bytes);
    let insert = total(|d| d.insert_bytes);
    let output = total(|d| d.output_bytes);
    let view_work = total(|d| d.views.saturating_add(d.anchors));
    let checks = [
        (Dimension::Documents, merged.len(), limits.documents),
        (Dimension::Edits, edits, limits.edits),
        (Dimension::InputBytes, input, limits.input_bytes),
        (Dimension::InsertBytes, insert, limits.insert_bytes),
        (Dimension::OutputBytes, output, limits.output_bytes),
        (
            Dimension::RetainedBytes,
            retained(input, insert, output, edits),
            limits.retained_bytes,
        ),
        (Dimension::ViewWork, view_work, limits.view_work),
    ];
    for (dimension, used, limit) in checks {
        if used > limit {
            return Err(ExtensionError::new(
                ErrorKind::LimitExceeded,
                format!("{} {used} exceeds the limit {limit}", dimension.name()),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(input: usize, edits: usize) -> DocumentLoad {
        DocumentLoad {
            input_bytes: input,
            edits,
            insert_bytes: 10,
            output_bytes: input + 10,
            views: 1,
            anchors: 1,
        }
    }

    #[test]
    fn accepts_a_request_within_every_limit() {
        assert!(
            admit(
                &EditLimits::default(),
                &[(1, load(1000, 3)), (2, load(500, 1))]
            )
            .is_ok()
        );
    }

    #[test]
    fn counts_duplicate_documents_once_and_sums_their_edits() {
        let limits = EditLimits {
            edits: 4,
            ..EditLimits::default()
        };
        assert!(admit(&limits, &[(1, load(10, 2)), (1, load(10, 2))]).is_ok());
        let error = admit(&limits, &[(1, load(10, 2)), (1, load(10, 3))]).unwrap_err();
        assert_eq!(error.kind, ErrorKind::LimitExceeded);
        assert!(error.message.contains("edits"));
    }

    #[test]
    fn names_the_rejecting_dimension() {
        let limits = EditLimits::default();
        let too_many: Vec<_> = (0..17).map(|id| (id, load(1, 1))).collect();
        assert!(
            admit(&limits, &too_many)
                .unwrap_err()
                .message
                .contains("documents")
        );
        let big = [(1, load(9 * MIB, 1))];
        assert!(
            admit(&limits, &big)
                .unwrap_err()
                .message
                .contains("input-bytes")
        );
    }

    #[test]
    fn retention_ledger_can_reject_below_the_individual_limits() {
        let limits = EditLimits {
            input_bytes: 40 * MIB,
            output_bytes: 40 * MIB,
            retained_bytes: 64 * MIB,
            ..EditLimits::default()
        };
        let error = admit(&limits, &[(1, load(33 * MIB, 1))]).unwrap_err();
        assert!(error.message.contains("retained-bytes"));
    }
}
