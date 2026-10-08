//! The core's cold-start script (domain/engine.md, 6; domain/root.md, 8).
//! The application performs each requested step and reports that exact step
//! done. Only the core chooses the next step or opens decisions.

use crate::{Core, fleet};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RestartStep {
    LoadCore,
    RestoreConnector { connector: u16 },
    AdoptRuns,
    ReadAfresh { connector: u16 },
    SettleOutbox { connector: u16 },
    Open,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RestartRequest {
    Step(RestartStep),
    Idle,
    Refused { step: RestartStep },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Restart {
    Cold,
    Waiting(RestartStep),
    Open,
    Failed(RestartStep),
}

impl Core {
    /// Check all loaded claims together before the fleet can settle any of
    /// them. Engine claims settle immediately and need no recovered slot.
    pub fn restart_admit_claims(&mut self) -> bool {
        match self.restart {
            Restart::Waiting(RestartStep::AdoptRuns) => {}
            Restart::Cold | Restart::Waiting(_) | Restart::Open | Restart::Failed(_) => {
                let _request = self.restart_refuse();
                return false;
            }
        }
        let mut workers = 0_u32;
        for (task, _) in &self.unreported_restored {
            match self.proofs.get(task).expect("restored claim has its proof").host {
                fleet::HostKind::Worker => workers = workers.checked_add(1).expect("bounded restored claims"),
                fleet::HostKind::Engine => {}
            }
        }
        if workers > self.restart_attempts {
            let _request = self.restart_refuse();
            return false;
        }
        true
    }

    /// Begin once; a repeated start cannot repeat loads or open decisions.
    pub fn restart_begin(&mut self) -> RestartRequest {
        match self.restart {
            Restart::Cold => self.restart_ask(RestartStep::LoadCore),
            Restart::Waiting(_) | Restart::Open | Restart::Failed(_) => RestartRequest::Idle,
        }
    }

    /// A failed or oversized live range stops this start, identifying its step.
    pub fn restart_refuse(&mut self) -> RestartRequest {
        let step = match self.restart {
            Restart::Cold => RestartStep::LoadCore,
            Restart::Waiting(step) | Restart::Failed(step) => step,
            Restart::Open => RestartStep::Open,
        };
        self.restart = Restart::Failed(step);
        RestartRequest::Refused { step }
    }

    #[must_use]
    pub fn restart_ready(&self) -> bool {
        match self.restart {
            Restart::Open => true,
            Restart::Cold | Restart::Waiting(_) | Restart::Failed(_) => false,
        }
    }

    #[must_use]
    pub fn restart_failure(&self) -> Option<RestartStep> {
        match self.restart {
            Restart::Failed(step) => Some(step),
            Restart::Cold | Restart::Waiting(_) | Restart::Open => None,
        }
    }

    /// Connector continuations and timers may finish the step currently asked.
    #[must_use]
    pub fn restart_step(&self) -> Option<RestartStep> {
        match self.restart {
            Restart::Waiting(step) => Some(step),
            Restart::Cold | Restart::Open | Restart::Failed(_) => None,
        }
    }

    /// Advance only from the matching completed step. Proofs must all be loaded
    /// and every restored claim must reach the fleet before the script proceeds.
    pub fn restart_done(&mut self, done: RestartStep) -> RestartRequest {
        match self.restart {
            Restart::Waiting(step) if step == done => {}
            Restart::Waiting(_) | Restart::Cold => return self.restart_refuse(),
            Restart::Open | Restart::Failed(_) => return RestartRequest::Idle,
        }
        let next = match done {
            RestartStep::LoadCore => {
                if !self.restoring_proofs.is_empty() {
                    return self.restart_refuse();
                }
                match self.connectors.first() {
                    Some(&connector) => RestartStep::RestoreConnector { connector },
                    None => RestartStep::AdoptRuns,
                }
            }
            RestartStep::RestoreConnector { connector } => match self.restart_next_connector(connector) {
                Some(connector) => RestartStep::RestoreConnector { connector },
                None => RestartStep::AdoptRuns,
            },
            RestartStep::AdoptRuns => {
                if !self.adopted.is_empty() {
                    return self.restart_refuse();
                }
                match self.connectors.first() {
                    Some(&connector) => RestartStep::ReadAfresh { connector },
                    None => RestartStep::Open,
                }
            }
            RestartStep::ReadAfresh { connector } => match self.restart_next_connector(connector) {
                Some(connector) => RestartStep::ReadAfresh { connector },
                None => match self.connectors.first() {
                    Some(&connector) => RestartStep::SettleOutbox { connector },
                    None => RestartStep::Open,
                },
            },
            RestartStep::SettleOutbox { connector } => match self.restart_next_connector(connector) {
                Some(connector) => RestartStep::SettleOutbox { connector },
                None => RestartStep::Open,
            },
            RestartStep::Open => {
                self.restart = Restart::Open;
                return RestartRequest::Idle;
            }
        };
        self.restart_ask(next)
    }

    fn restart_ask(&mut self, step: RestartStep) -> RestartRequest {
        self.restart = Restart::Waiting(step);
        RestartRequest::Step(step)
    }

    fn restart_next_connector(&self, previous: u16) -> Option<u16> {
        let mut found = false;
        for &connector in &self.connectors {
            if found {
                return Some(connector);
            }
            found = connector == previous;
        }
        None
    }
}
