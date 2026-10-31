//! Call budgets with user-interaction pauses (design §8, §8.4).
//!
//! Time is supplied by the caller (milliseconds on any monotonic clock) so the
//! rules are deterministic in tests. Only an interaction the host has taken
//! over (`enter_interaction`) pauses the automatic budget; queueing, guest
//! rounds and ordinary host waits keep counting.
use crate::error::{ErrorKind, ExtensionError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InteractionLimits {
    /// Prompts allowed in one call chain.
    pub max_prompts: u32,
    /// Idle limit of a single prompt.
    pub idle_ms: u64,
    /// Total time a chain may spend waiting for the user.
    pub chain_ms: u64,
}

impl Default for InteractionLimits {
    fn default() -> Self {
        Self {
            max_prompts: 8,
            idle_ms: 10 * 60 * 1000,
            chain_ms: 30 * 60 * 1000,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Running { since: u64 },
    Waiting { since: u64 },
}

/// Automatic-processing budget of one command call.
#[derive(Clone, Debug)]
pub struct CallBudget {
    limit_ms: u64,
    used_ms: u64,
    phase: Phase,
    limits: InteractionLimits,
    prompts: u32,
    waited_ms: u64,
}

impl CallBudget {
    /// Candidate automatic budget for a command: 5 seconds.
    pub const COMMAND_MS: u64 = 5_000;

    pub fn new(now: u64, limit_ms: u64, limits: InteractionLimits) -> Self {
        Self {
            limit_ms,
            used_ms: 0,
            phase: Phase::Running { since: now },
            limits,
            prompts: 0,
            waited_ms: 0,
        }
    }

    fn settle(&mut self, now: u64) {
        if let Phase::Running { since } = self.phase {
            self.used_ms = self.used_ms.saturating_add(now.saturating_sub(since));
            self.phase = Phase::Running { since: now };
        }
    }

    /// Milliseconds of automatic budget consumed so far.
    pub fn used(&mut self, now: u64) -> u64 {
        self.settle(now);
        self.used_ms
    }

    pub fn is_waiting(&self) -> bool {
        matches!(self.phase, Phase::Waiting { .. })
    }

    /// `deadline-exceeded` once the automatic budget is spent. While the user
    /// is being waited on, only the interaction idle limit can fire.
    pub fn check(&mut self, now: u64) -> Result<(), ExtensionError> {
        match self.phase {
            Phase::Waiting { since } => {
                if now.saturating_sub(since) > self.limits.idle_ms {
                    return Err(ExtensionError::new(
                        ErrorKind::DeadlineExceeded,
                        "the prompt was left unanswered",
                    ));
                }
                if self.waited_ms.saturating_add(now.saturating_sub(since)) > self.limits.chain_ms {
                    return Err(ExtensionError::new(
                        ErrorKind::DeadlineExceeded,
                        "the interaction chain ran too long",
                    ));
                }
                Ok(())
            }
            Phase::Running { .. } => {
                self.settle(now);
                if self.used_ms > self.limit_ms {
                    Err(ExtensionError::new(
                        ErrorKind::DeadlineExceeded,
                        format!(
                            "automatic processing used {} ms of {} ms",
                            self.used_ms, self.limit_ms
                        ),
                    ))
                } else {
                    Ok(())
                }
            }
        }
    }

    /// The host has displayed a prompt and owns the wait.
    pub fn enter_interaction(&mut self, now: u64) -> Result<(), ExtensionError> {
        if self.is_waiting() {
            return Err(ExtensionError::new(
                ErrorKind::LimitExceeded,
                "only one interaction per call",
            ));
        }
        if self.prompts >= self.limits.max_prompts {
            return Err(ExtensionError::new(
                ErrorKind::LimitExceeded,
                "too many prompts in one call",
            ));
        }
        self.check(now)?;
        self.prompts += 1;
        self.phase = Phase::Waiting { since: now };
        Ok(())
    }

    /// The user answered or cancelled; automatic accounting resumes without a reset.
    pub fn leave_interaction(&mut self, now: u64) {
        if let Phase::Waiting { since } = self.phase {
            self.waited_ms = self.waited_ms.saturating_add(now.saturating_sub(since));
            self.phase = Phase::Running { since: now };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget() -> CallBudget {
        CallBudget::new(0, CallBudget::COMMAND_MS, InteractionLimits::default())
    }

    #[test]
    fn automatic_time_counts_and_expires() {
        let mut b = budget();
        assert!(b.check(4_999).is_ok());
        assert_eq!(
            b.check(5_001).unwrap_err().kind,
            ErrorKind::DeadlineExceeded
        );
    }

    #[test]
    fn waiting_for_the_user_does_not_consume_the_budget() {
        let mut b = budget();
        b.enter_interaction(1_000).unwrap();
        assert!(
            b.check(1_000 + 120_000).is_ok(),
            "two minutes of thinking is fine"
        );
        b.leave_interaction(1_000 + 120_000);
        assert_eq!(b.used(1_000 + 120_000), 1_000);
        assert!(b.check(1_000 + 120_000 + 3_999).is_ok());
        assert!(
            b.check(1_000 + 120_000 + 4_001).is_err(),
            "resuming does not reset the 5 s"
        );
    }

    #[test]
    fn repeated_prompts_never_regain_the_full_budget() {
        let mut b = budget();
        // Each round: 1.5 s of automatic work, then a long prompt, then 1 s more work.
        b.enter_interaction(1_500).unwrap();
        b.leave_interaction(20_000);
        b.enter_interaction(21_500).unwrap();
        b.leave_interaction(40_000);
        assert_eq!(b.used(40_000), 1_500 + 1_500);
        let error = b.enter_interaction(43_000).unwrap_err();
        assert_eq!(
            error.kind,
            ErrorKind::DeadlineExceeded,
            "prompting again cannot restore the 5 s"
        );
    }

    #[test]
    fn one_interaction_at_a_time_and_a_prompt_cap() {
        let mut b = budget();
        b.enter_interaction(0).unwrap();
        assert_eq!(
            b.enter_interaction(1).unwrap_err().kind,
            ErrorKind::LimitExceeded
        );
        let limits = InteractionLimits {
            max_prompts: 1,
            ..InteractionLimits::default()
        };
        let mut capped = CallBudget::new(0, 5_000, limits);
        capped.enter_interaction(0).unwrap();
        capped.leave_interaction(1);
        assert!(capped.enter_interaction(2).is_err());
    }

    #[test]
    fn an_unanswered_prompt_hits_the_idle_limit() {
        let mut b = budget();
        b.enter_interaction(0).unwrap();
        assert!(b.check(10 * 60 * 1000).is_ok());
        assert_eq!(
            b.check(10 * 60 * 1000 + 1).unwrap_err().kind,
            ErrorKind::DeadlineExceeded
        );
    }
}
