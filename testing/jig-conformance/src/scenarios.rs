//! Shared adversarial stories (`domain/testing.md`, 5). These name jig's work
//! and faults; an application binds each action to its own kinds and resources.
use crate::referee::Violation;
use crate::{Application, Cut, Harness, Outcome};

/// An adversarial story shared by every application with the required kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scenario {
    /// A delayed effect copy reaches the system after its retry.
    LateCopy,
    /// Another participant reaches the conditional write's target first.
    OtherHand,
    /// Repeated cold starts must retain an uncertain attempt's retry deadline.
    UncertainRestarts,
    /// An observed judge changes facts after the verdict and before application.
    ChangedJudge,
    /// A shrinking pool retains holders while its queued task waits.
    ShrinkingPool,
    /// A narrower policy keeps committed entries and rechecks waiting proposals.
    NarrowingPolicy,
    /// A standing task makes bounded batches across many funding periods.
    StandingTask,
    /// Many sessions and sub-agent completions share one run's final allowance.
    RunBudget,
}

/// Every shared story, in a stable order for focused cuts and sweeps.
pub const ALL: [Scenario; 8] = [
    Scenario::LateCopy,
    Scenario::OtherHand,
    Scenario::UncertainRestarts,
    Scenario::ChangedJudge,
    Scenario::ShrinkingPool,
    Scenario::NarrowingPolicy,
    Scenario::StandingTask,
    Scenario::RunBudget,
];

/// One scenario action, translated at the application's outside boundaries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Let an unseen claim pass adoption grace and retry backoff if needed.
    Ready,
    /// Delay one effect copy independently of the connector.
    Late,
    /// Ask the active run for a numbered effect.
    Effect(u32),
    /// Deliver delayed copies after the retry.
    DeliverLate,
    /// Another participant reaches the decided conditional target.
    OtherHand,
    /// Withhold released system writes while the engine runs ahead.
    HoldWrites,
    /// Let released writes reach their independent system.
    ReleaseWrites,
    /// Compare the durable uncertain deadline across each cold start.
    CheckDeadline,
    /// Change the independent requirement judge's current fact.
    ChangeJudge,
    /// Start two holders and a waiter under a two-slot pool.
    FillPool,
    /// Shrink that pool to one slot.
    ShrinkPool,
    /// Complete one independently hosted holder in order.
    FinishHolder(u32),
    /// Make a proposal that needs a person's authority.
    Propose,
    /// Narrow scenario policy through an authenticated party request.
    NarrowPolicy,
    /// Let the holder accept the waiting proposal under current policy.
    AcceptProposal,
    /// Start a recurring template under its own finite funding period.
    StartStanding,
    /// Complete the current batch, advance funding, and duplicate the notification.
    Period(u64),
    /// Resume the run's independently priced sessions and sub-agents.
    Resume,
}

/// An application adds kind/resource bindings and outside story checks to its
/// engine adapter. Optional stories return no configuration when their kinds
/// do not exist; their absence remains visible to the caller.
pub trait ScenarioApplication: Application + Sized {
    /// Bind the story to tiny limits, independent system state and scripts.
    fn configuration(scenario: Scenario, seed: u64) -> Option<Self::Config>;
    /// Translate one action; implementations may read only store and peers.
    ///
    /// # Errors
    /// Returns a broken promise observed while playing this story action.
    fn action(world: &mut Harness<Self>, action: Action) -> Result<(), Violation>;
    /// Check the story's ending from independent peers and durable records.
    ///
    /// # Errors
    /// Returns the story condition that failed at its outside boundary.
    fn check(world: &Harness<Self>, scenario: Scenario) -> Result<(), Violation>;
}

/// Run one shared scenario, including the referee at every iteration.
///
/// # Errors
/// Returns the first broken promise or an application story whose ending failed.
pub fn run<A: ScenarioApplication>(scenario: Scenario, cut: Cut, seed: u64) -> Result<Option<Outcome>, Violation> {
    let Some(config) = A::configuration(scenario, seed) else { return Ok(None) };
    let mut world = Harness::<A>::new(config, seed);
    world.cut(cut);
    world.drain()?;
    A::action(&mut world, Action::Ready)?;
    world.drain()?;
    let actions: &[Action] = match scenario {
        Scenario::LateCopy => &[Action::Late, Action::Effect(1)],
        Scenario::OtherHand => &[Action::Late, Action::Effect(1), Action::OtherHand],
        Scenario::UncertainRestarts => &[Action::HoldWrites, Action::Effect(1)],
        Scenario::ChangedJudge => {
            &[Action::HoldWrites, Action::Effect(1), Action::ChangeJudge, Action::ReleaseWrites, Action::Effect(2)]
        }
        Scenario::ShrinkingPool => &[
            Action::FillPool,
            Action::ShrinkPool,
            Action::FinishHolder(0),
            Action::FinishHolder(1),
            Action::FinishHolder(2),
        ],
        Scenario::NarrowingPolicy => &[
            Action::HoldWrites,
            Action::Effect(1),
            Action::Propose,
            Action::NarrowPolicy,
            Action::ReleaseWrites,
            Action::AcceptProposal,
            Action::Effect(3),
        ],
        Scenario::StandingTask => {
            &[Action::StartStanding, Action::Period(2), Action::Period(3), Action::Period(4), Action::Period(5)]
        }
        Scenario::RunBudget => &[Action::Resume],
    };
    for &action in actions {
        A::action(&mut world, action)?;
        world.drain()?;
    }
    match scenario {
        Scenario::LateCopy | Scenario::OtherHand => {
            // A cut may hide the first release; the second interval also lets
            // that restored attempt pass its original deadline before the copy.
            for _ in 0..2 {
                world.advance(2_000_000_000);
                world.drain()?;
            }
            A::action(&mut world, Action::DeliverLate)?;
            world.drain()?;
        }
        Scenario::UncertainRestarts => {
            A::action(&mut world, Action::CheckDeadline)?;
            for _ in 0..3 {
                world.crash()?;
                world.drain()?;
                A::action(&mut world, Action::CheckDeadline)?;
            }
            A::action(&mut world, Action::ReleaseWrites)?;
            for _ in 0..2 {
                world.advance(2_000_000_000);
                world.drain()?;
            }
        }
        Scenario::ChangedJudge | Scenario::NarrowingPolicy => {
            for _ in 0..2 {
                world.advance(2_000_000_000);
                world.drain()?;
            }
            if scenario == Scenario::ChangedJudge {
                A::action(&mut world, Action::Effect(2))?;
            } else {
                A::action(&mut world, Action::NarrowPolicy)?;
                world.drain()?;
                A::action(&mut world, Action::AcceptProposal)?;
            }
            world.drain()?;
        }
        Scenario::ShrinkingPool | Scenario::StandingTask | Scenario::RunBudget => {}
    }
    A::check(&world, scenario)?;
    Ok(Some(world.outcome()))
}
