//! Document event continuity (design §7.3).
//!
//! Change events carry `from-version / to-version` and a bounded summary.
//! Consecutive changes merge only while they stay contiguous and within the
//! summary budget; anything else becomes `ResyncRequired`, after which the
//! extension reads a fresh snapshot and rebuilds its baseline. Open and close
//! are lifecycle boundaries and are never merged with changes.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Delivery {
    Opened {
        lifecycle: u64,
        version: u64,
    },
    Changed {
        lifecycle: u64,
        from: u64,
        to: u64,
        edits: usize,
    },
    /// The incremental stream has a gap; read a new snapshot at `version`.
    ResyncRequired {
        lifecycle: u64,
        version: u64,
    },
    Closed {
        lifecycle: u64,
    },
}

#[derive(Debug)]
pub struct DocumentStream {
    lifecycle: u64,
    open: bool,
    /// Latest version already included in an emitted or pending message.
    version: u64,
    max_edits: usize,
    max_messages: usize,
    queue: Vec<Delivery>,
}

impl DocumentStream {
    pub fn new(max_edits: usize, max_messages: usize) -> Self {
        Self {
            lifecycle: 0,
            open: false,
            version: 0,
            max_edits,
            max_messages,
            queue: Vec::new(),
        }
    }

    /// A new lifecycle: events of earlier ones can no longer be delivered.
    pub fn open(&mut self, version: u64) {
        self.lifecycle += 1;
        self.open = true;
        self.version = version;
        self.queue.clear();
        self.queue.push(Delivery::Opened {
            lifecycle: self.lifecycle,
            version,
        });
    }

    pub fn close(&mut self) {
        if self.open {
            self.open = false;
            self.queue.push(Delivery::Closed {
                lifecycle: self.lifecycle,
            });
        }
    }

    /// The document moved from `from` to `to` with `edits` edit operations.
    pub fn changed(&mut self, from: u64, to: u64, edits: usize) {
        if !self.open {
            return;
        }
        if from != self.version {
            self.resync(to);
            return;
        }
        self.version = to;
        if let Some(Delivery::Changed {
            to: last_to,
            edits: last_edits,
            ..
        }) = self.queue.last_mut()
        {
            if *last_edits + edits <= self.max_edits {
                *last_to = to;
                *last_edits += edits;
                return;
            }
        }
        if matches!(self.queue.last(), Some(Delivery::ResyncRequired { .. })) {
            // Already waiting for a baseline: just move the target version.
            self.queue.pop();
            self.queue.push(Delivery::ResyncRequired {
                lifecycle: self.lifecycle,
                version: to,
            });
            return;
        }
        if self.queue.len() >= self.max_messages {
            self.resync(to);
            return;
        }
        self.queue.push(Delivery::Changed {
            lifecycle: self.lifecycle,
            from,
            to,
            edits,
        });
    }

    fn resync(&mut self, version: u64) {
        self.version = version;
        // Only the lifecycle boundary survives; the rest is replaced by one resync.
        self.queue
            .retain(|d| matches!(d, Delivery::Opened { .. } | Delivery::Closed { .. }));
        self.queue.push(Delivery::ResyncRequired {
            lifecycle: self.lifecycle,
            version,
        });
    }

    /// Queued messages, oldest first, without consuming them.
    pub fn peek(&self) -> &[Delivery] {
        &self.queue
    }

    pub fn pending(&self) -> usize {
        self.queue.len()
    }
    pub fn lifecycle(&self) -> u64 {
        self.lifecycle
    }

    pub fn drain(&mut self) -> Vec<Delivery> {
        std::mem::take(&mut self.queue)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contiguous_changes_merge_into_one_message() {
        let mut stream = DocumentStream::new(100, 8);
        stream.open(1);
        stream.drain();
        stream.changed(1, 2, 1);
        stream.changed(2, 3, 2);
        assert_eq!(
            stream.drain(),
            [Delivery::Changed {
                lifecycle: 1,
                from: 1,
                to: 3,
                edits: 3
            }]
        );
    }

    #[test]
    fn a_gap_becomes_a_resync_not_a_fake_stream() {
        let mut stream = DocumentStream::new(100, 8);
        stream.open(1);
        stream.drain();
        stream.changed(1, 2, 1);
        stream.changed(5, 6, 1); // versions 2..5 were never reported
        assert_eq!(
            stream.drain(),
            [Delivery::ResyncRequired {
                lifecycle: 1,
                version: 6
            }]
        );
        // After the resync the next contiguous change is incremental again.
        stream.changed(6, 7, 1);
        assert_eq!(
            stream.drain(),
            [Delivery::Changed {
                lifecycle: 1,
                from: 6,
                to: 7,
                edits: 1
            }]
        );
    }

    #[test]
    fn exceeding_the_summary_budget_starts_a_new_message_and_then_overflows_to_resync() {
        let mut stream = DocumentStream::new(2, 2);
        stream.open(0);
        stream.drain();
        stream.changed(0, 1, 2);
        stream.changed(1, 2, 2);
        assert_eq!(stream.pending(), 2);
        stream.changed(2, 3, 2); // queue full: replaced by one resync
        assert_eq!(
            stream.drain(),
            [Delivery::ResyncRequired {
                lifecycle: 1,
                version: 3
            }]
        );
    }

    #[test]
    fn open_and_close_are_boundaries_and_old_lifecycles_are_dropped() {
        let mut stream = DocumentStream::new(100, 8);
        stream.open(1);
        stream.changed(1, 2, 1);
        stream.close();
        stream.changed(2, 3, 1); // closed documents emit nothing
        assert_eq!(
            stream.drain(),
            [
                Delivery::Opened {
                    lifecycle: 1,
                    version: 1
                },
                Delivery::Changed {
                    lifecycle: 1,
                    from: 1,
                    to: 2,
                    edits: 1
                },
                Delivery::Closed { lifecycle: 1 },
            ]
        );
        stream.open(10);
        assert_eq!(
            stream.drain(),
            [Delivery::Opened {
                lifecycle: 2,
                version: 10
            }]
        );
    }

    #[test]
    fn a_pending_resync_only_moves_its_target_version() {
        let mut stream = DocumentStream::new(100, 8);
        stream.open(1);
        stream.drain();
        stream.changed(9, 10, 1);
        stream.changed(10, 11, 1);
        assert_eq!(
            stream.drain(),
            [Delivery::ResyncRequired {
                lifecycle: 1,
                version: 11
            }]
        );
    }
}
