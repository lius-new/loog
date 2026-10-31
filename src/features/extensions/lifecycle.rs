//! Instance lifecycle (design §8).
//!
//! `Discovered → Validated → Dormant → Activating → Active → Stopping →
//! Stopped`; load, activation or execution failure goes to `Failed`, and only
//! an explicit retry creates a new generation. There is no automatic restart.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Discovered,
    Validated,
    Dormant,
    Activating,
    Active,
    Stopping,
    Stopped,
    Failed,
}

/// Where a failure happened, shown by the diagnostics entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureStage {
    Load,
    Link,
    Activation,
    Execution,
    BudgetExceeded,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidTransition {
    pub from: State,
    pub action: &'static str,
}

impl std::fmt::Display for InvalidTransition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cannot {} while {:?}", self.action, self.from)
    }
}

impl std::error::Error for InvalidTransition {}

#[derive(Clone, Debug)]
pub struct Lifecycle {
    state: State,
    generation: u64,
    failure: Option<FailureStage>,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self {
            state: State::Discovered,
            generation: 1,
            failure: None,
        }
    }
}

impl Lifecycle {
    /// A lifecycle for a reloaded package: generations keep counting up, so
    /// work stamped by the previous instance can never match the new one.
    pub fn after(previous_generation: u64) -> Self {
        Self {
            state: State::Discovered,
            generation: previous_generation + 1,
            failure: None,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn failure(&self) -> Option<FailureStage> {
        self.failure
    }

    fn step(
        &mut self,
        action: &'static str,
        from: &[State],
        to: State,
    ) -> Result<(), InvalidTransition> {
        if from.contains(&self.state) {
            self.state = to;
            Ok(())
        } else {
            Err(InvalidTransition {
                from: self.state,
                action,
            })
        }
    }

    pub fn validated(&mut self) -> Result<(), InvalidTransition> {
        self.step("validate", &[State::Discovered], State::Validated)
    }

    pub fn dormant(&mut self) -> Result<(), InvalidTransition> {
        self.step("make dormant", &[State::Validated], State::Dormant)
    }

    pub fn activating(&mut self) -> Result<(), InvalidTransition> {
        self.step("activate", &[State::Dormant], State::Activating)
    }

    pub fn active(&mut self) -> Result<(), InvalidTransition> {
        self.step("finish activation", &[State::Activating], State::Active)
    }

    /// Stopping bumps the generation first so late work of the old one is refused.
    pub fn stopping(&mut self) -> Result<(), InvalidTransition> {
        self.step("stop", &[State::Activating, State::Active], State::Stopping)?;
        self.generation += 1;
        Ok(())
    }

    pub fn stopped(&mut self) -> Result<(), InvalidTransition> {
        self.step("finish stopping", &[State::Stopping], State::Stopped)
    }

    pub fn fail(&mut self, stage: FailureStage) {
        if self.state != State::Failed {
            if matches!(
                self.state,
                State::Activating | State::Active | State::Stopping
            ) {
                self.generation += 1;
            }
            self.state = State::Failed;
            self.failure = Some(stage);
        }
    }

    /// Explicit retry: new generation, back to `Dormant`; failure details clear.
    pub fn retry(&mut self) -> Result<(), InvalidTransition> {
        self.step("retry", &[State::Failed, State::Stopped], State::Dormant)?;
        self.generation += 1;
        self.failure = None;
        Ok(())
    }

    /// Whether work stamped with `generation` may still act.
    pub fn accepts(&self, generation: u64) -> bool {
        self.generation == generation && matches!(self.state, State::Activating | State::Active)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active() -> Lifecycle {
        let mut l = Lifecycle::default();
        l.validated().unwrap();
        l.dormant().unwrap();
        l.activating().unwrap();
        l.active().unwrap();
        l
    }

    #[test]
    fn follows_the_documented_path() {
        let mut l = active();
        assert!(l.accepts(1));
        l.stopping().unwrap();
        l.stopped().unwrap();
        assert_eq!(l.state(), State::Stopped);
    }

    #[test]
    fn invalid_transitions_are_refused() {
        let mut l = Lifecycle::default();
        assert!(l.activating().is_err());
        assert!(l.stopped().is_err());
    }

    #[test]
    fn stopping_refuses_late_work_of_the_old_generation() {
        let mut l = active();
        let before = l.generation();
        l.stopping().unwrap();
        assert!(!l.accepts(before));
    }

    #[test]
    fn failure_is_sticky_until_an_explicit_retry_with_a_new_generation() {
        let mut l = active();
        let before = l.generation();
        l.fail(FailureStage::Execution);
        assert_eq!(l.failure(), Some(FailureStage::Execution));
        assert!(l.activating().is_err(), "no automatic restart");
        l.retry().unwrap();
        assert_eq!(l.state(), State::Dormant);
        assert!(l.generation() > before);
        assert_eq!(l.failure(), None);
    }

    #[test]
    fn a_second_failure_keeps_the_first_stage() {
        let mut l = Lifecycle::default();
        l.fail(FailureStage::Load);
        l.fail(FailureStage::Execution);
        assert_eq!(l.failure(), Some(FailureStage::Load));
    }
}
