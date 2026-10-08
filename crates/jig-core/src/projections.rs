//! Whole task-tree feeds and closing retention (`domain/connectors.md`, 8;
//! `domain/engine.md`, 4.4 and 5.4). The core retains only the feed's task
//! facts and unread history milestones. Connector digests and writes stay
//! with their owners. Roots call `finish_decision` after routing ordinary
//! work, route its asks synchronously, and call again until it emits no work.
//!
//! | State | Event | Next state | Emits |
//! | --- | --- | --- | --- |
//! | Absent | Tracked goal made | Changed | Bounded goal and plan |
//! | Open | Subtree save/history | Changed | Updated plan and pending milestones |
//! | Changed | Decision finishing | Open or closing | Save and one feed per connector |
//! | Closing | Last connector settles | Absent | Erase and forget asks |

use crate::{Ask, Core, CoreKey, CoreRecord, Limits, Record, Request, Requests, Write};
use alloc::boxed::Box;
use jig_core_tasks as tasks;
use skein_lib::{Env, List, Queue};

/// Current tracked-goal facts, independent of executable authority and inboxes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ProjectionGoal {
    pub number: u64,
    pub project: u32,
    pub priority: u32,
    pub words: Box<[u8]>,
    pub phase: tasks::Phase,
}

/// One task in the whole bounded tree, including tasks that already ended.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ProjectionTask {
    pub number: u64,
    pub requester: tasks::Party,
    pub words: Box<[u8]>,
    pub phase: tasks::Phase,
}

/// Stable identity from the task's lifecycle or semantic history family.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum MilestoneId {
    /// A lifecycle transition, independently numbered by the task hub.
    Lifecycle { task: u64, position: u64 },
    /// A semantic change under its immutable task revision.
    Revision { task: u64, revision: u64 },
}

/// One newly recorded milestone; lifecycle details or semantic reason travel together.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ProjectionMilestone {
    pub identity: MilestoneId,
    pub phase: Option<tasks::Phase>,
    pub change: Option<tasks::Change>,
    pub words: Box<[u8]>,
}

/// One connector handoff, with the whole plan and milestones since its last feed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ProjectionFeed {
    pub goal: ProjectionGoal,
    pub plan: Box<[ProjectionTask]>,
    pub milestones: Box<[ProjectionMilestone]>,
    pub closing: bool,
}

/// Durable feed state retained until every connector settles the closing feed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Projection {
    pub goal: ProjectionGoal,
    pub plan: Box<[ProjectionTask]>,
    pub milestones: Box<[ProjectionMilestone]>,
    pub pending: Box<[u16]>,
    pub changed: bool,
    pub fed_at: Option<u64>,
}

/// Ordered durable projection families: header before its plan and pending history.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum ProjectionKey {
    /// A goal's feed metadata and final settlement cohort.
    Goal(u64),
    /// One task retained in the complete plan.
    Task { goal: u64, task: u64 },
    /// One unread lifecycle milestone.
    Lifecycle { goal: u64, task: u64, position: u64 },
    /// One unread semantic history milestone.
    Revision { goal: u64, task: u64, revision: u64 },
}

/// One bounded row of a projection; the whole tree is never one journal row.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ProjectionRecord {
    /// Feed metadata, retained through the last closing settlement.
    Goal { goal: ProjectionGoal, pending: Box<[u16]>, changed: bool, fed_at: Option<u64> },
    /// One current task in the bounded tree.
    Task { goal: u64, task: ProjectionTask },
    /// History recorded since the last feed.
    Milestone { goal: u64, milestone: ProjectionMilestone },
}

impl ProjectionRecord {
    /// The fixed-size address of this bounded row.
    #[must_use]
    pub fn key(&self) -> ProjectionKey {
        match self {
            Self::Goal { goal, .. } => ProjectionKey::Goal(goal.number),
            Self::Task { goal, task } => ProjectionKey::Task { goal: *goal, task: task.number },
            Self::Milestone { goal, milestone } => milestone_key(*goal, milestone.identity),
        }
    }
}

fn milestone_key(goal: u64, identity: MilestoneId) -> ProjectionKey {
    match identity {
        MilestoneId::Lifecycle { task, position } => ProjectionKey::Lifecycle { goal, task, position },
        MilestoneId::Revision { task, revision } => ProjectionKey::Revision { goal, task, revision },
    }
}

fn header(state: &Projection) -> ProjectionRecord {
    ProjectionRecord::Goal {
        goal: state.goal.clone(),
        pending: state.pending.clone(),
        changed: state.changed,
        fed_at: state.fed_at,
    }
}

fn write(out: &mut Queue<Request>, record: ProjectionRecord) {
    out.push(Request::Write(Write::Save(Record::Core(CoreRecord::Projection(record)))));
}

fn goal(row: &tasks::TaskRecord) -> ProjectionGoal {
    ProjectionGoal {
        number: row.number,
        project: row.project,
        priority: row.tracked.expect("tracked goal"),
        words: row.spec.words.clone(),
        phase: row.phase.clone(),
    }
}

pub(crate) fn save(core: &mut Core, limits: &Limits, row: &tasks::TaskRecord, out: &mut Queue<Request>) {
    if row.tracked.is_some() && !core.projections.contains_key(&row.number) {
        core.projections
            .insert(
                row.number,
                Box::new(Projection {
                    goal: goal(row),
                    plan: Box::new([]),
                    milestones: Box::new([]),
                    pending: Box::new([]),
                    changed: true,
                    fed_at: None,
                }),
            )
            .expect("tracked projection room admitted");
    }
    let Some(state) = core.projections.get_mut(&row.root) else { return };
    if row.number == row.root {
        let next = goal(row);
        state.changed |= state.goal != next;
        state.goal = next;
    }
    let next = ProjectionTask {
        number: row.number,
        requester: row.requester,
        words: row.spec.words.clone(),
        phase: row.phase.clone(),
    };
    let mut plan = List::with_capacity(limits.tasks.tree_tasks);
    let mut found = false;
    for old in &state.plan {
        if old.number == row.number {
            state.changed |= *old != next;
            plan.push(next.clone()).expect("existing tree task");
            found = true;
        } else {
            plan.push(old.clone()).expect("bounded tree plan");
        }
    }
    if !found {
        plan.push(next).expect("lifetime tree task room admitted");
        state.changed = true;
    }
    state.plan = plan.into_boxed();
    write(
        out,
        ProjectionRecord::Task {
            goal: row.root,
            task: ProjectionTask {
                number: row.number,
                requester: row.requester,
                words: row.spec.words.clone(),
                phase: row.phase.clone(),
            },
        },
    );
    write(out, header(state));
}

pub(crate) fn history(
    core: &mut Core,
    limits: &Limits,
    task: u64,
    milestone: ProjectionMilestone,
    out: &mut Queue<Request>,
) {
    let root = match core.tasks.root(task) {
        Some(root) => root,
        None => {
            let mut root = None;
            for (number, projection) in &core.projections {
                for row in &projection.plan {
                    if row.number == task {
                        root = Some(*number);
                    }
                }
            }
            let Some(root) = root else { return };
            root
        }
    };
    if !core.projections.contains_key(&root)
        && let Some(goal) = core.tasks.task(root)
        && goal.tracked.is_some()
    {
        let goal = goal.clone();
        save(core, limits, &goal, out);
    }
    let Some(state) = core.projections.get_mut(&root) else { return };
    let saved = milestone.clone();
    let mut milestones = List::with_capacity(crate::routing::room_max(limits).expect("valid feed room").writes);
    for old in &state.milestones {
        if old.identity == milestone.identity {
            return;
        }
        milestones.push(old.clone()).expect("pending history bounded by decision");
    }
    milestones.push(milestone).expect("decision history room");
    state.milestones = milestones.into_boxed();
    state.changed = true;
    write(out, ProjectionRecord::Milestone { goal: root, milestone: saved });
    write(out, header(state));
}

/// Complete a composed root decision's projection handoffs. Repeated calls
/// retain subsequent changes but never feed a goal twice under one commit.
pub fn finish_decision(core: &mut Core, env: &Env<Limits>) -> Requests {
    let room = crate::routing::room_max(&env.limits).expect("valid projection room").writes;
    let mut out = Queue::with_capacity(room.checked_add(1).expect("feed mark"));
    let commit = core.counters.deployment().commits;
    let mut goals = List::with_capacity(env.limits.tasks.tasks);
    for (number, _) in &core.projections {
        goals.push(*number).expect("bounded tracked goals");
    }
    for number in &goals {
        let state = core.projections.get_mut(number).expect("retained feed state");
        if !state.changed && (state.milestones.is_empty() || state.fed_at == Some(commit)) {
            continue;
        }
        let feed = if state.fed_at == Some(commit) {
            None
        } else {
            state.fed_at = Some(commit);
            let closing = match state.goal.phase {
                tasks::Phase::Ended(_) => true,
                tasks::Phase::Waiting
                | tasks::Phase::Active(_)
                | tasks::Phase::Closing(_)
                | tasks::Phase::Held { .. } => false,
            };
            if closing && state.pending.is_empty() {
                state.pending.clone_from(&core.connectors);
            }
            let feed = ProjectionFeed {
                goal: state.goal.clone(),
                plan: state.plan.clone(),
                milestones: state.milestones.clone(),
                closing,
            };
            for milestone in &state.milestones {
                out.push(Request::Write(Write::Erase(crate::Key::Core(CoreKey::Projection(milestone_key(
                    state.goal.number,
                    milestone.identity,
                ))))));
            }
            state.milestones = Box::new([]);
            Some(feed)
        };
        state.changed = false;
        write(&mut out, header(state));
        if let Some(feed) = feed {
            for &connector in core.connectors.as_ref() {
                out.push(Request::Ask { connector, ask: Ask::ProjectGoal { feed: Box::new(feed.clone()) } });
            }
        }
        let ended = match state.goal.phase {
            tasks::Phase::Ended(_) => true,
            tasks::Phase::Waiting | tasks::Phase::Active(_) | tasks::Phase::Closing(_) | tasks::Phase::Held { .. } => {
                false
            }
        };
        if ended && core.connectors.is_empty() {
            forget(core, *number, &mut out);
        }
    }
    out.push(Request::Decided);
    Requests::Out(out)
}

pub(crate) fn settled(core: &mut Core, goal: u64, connector: u16, out: &mut Queue<Request>) {
    let Some(state) = core.projections.get_mut(&goal) else { return };
    if !state.pending.contains(&connector) {
        return;
    }
    let mut pending = List::with_capacity(u32::try_from(state.pending.len()).expect("bounded connectors"));
    for &number in &state.pending {
        if number != connector {
            pending.push(number).expect("connector subset");
        }
    }
    state.pending = pending.into_boxed();
    if state.pending.is_empty() {
        forget(core, goal, out);
    } else {
        write(out, header(state));
    }
}

fn forget(core: &mut Core, goal: u64, out: &mut Queue<Request>) {
    let state = core.projections.remove(&goal).expect("closing feed state retained");
    for row in &state.plan {
        out.push(Request::Write(Write::Erase(crate::Key::Core(CoreKey::Projection(ProjectionKey::Task {
            goal,
            task: row.number,
        })))));
    }
    for milestone in &state.milestones {
        out.push(Request::Write(Write::Erase(crate::Key::Core(CoreKey::Projection(milestone_key(
            goal,
            milestone.identity,
        ))))));
    }
    out.push(Request::Write(Write::Erase(crate::Key::Core(CoreKey::Projection(ProjectionKey::Goal(goal))))));
    for &connector in core.connectors.as_ref() {
        out.push(Request::Ask { connector, ask: Ask::ForgetProjection { goal } });
    }
}

/// Checked maximum owned bytes of one retained projection feed cache.
#[must_use]
pub fn projection_bytes(limits: &Limits) -> Option<u64> {
    let phase = u64::from(limits.tasks.result_bytes).checked_mul(2)?;
    let goal = u64::try_from(size_of::<ProjectionGoal>())
        .ok()?
        .checked_add(u64::from(limits.tasks.spec_bytes))?
        .checked_add(phase)?;
    let task = u64::try_from(size_of::<ProjectionTask>())
        .ok()?
        .checked_add(u64::from(limits.tasks.spec_bytes))?
        .checked_add(phase)?;
    let milestone = u64::try_from(size_of::<ProjectionMilestone>())
        .ok()?
        .checked_add(u64::from(limits.tasks.message_bytes.max(limits.tasks.result_bytes)))?
        .checked_add(phase)?;
    u64::try_from(size_of::<Projection>())
        .ok()?
        .checked_add(goal)?
        .checked_add(u64::from(limits.tasks.tree_tasks).checked_mul(task)?)?
        .checked_add(u64::from(limits.connectors).checked_mul(2)?)?
        .checked_add(u64::from(crate::routing::room_max(limits)?.writes).checked_mul(milestone)?)
}

/// Deep bytes of one durable feed row, excluding its inline record slot.
#[must_use]
pub fn projection_record_bytes(row: &ProjectionRecord) -> Option<u64> {
    record_bytes(row)
}

fn record_bytes(row: &ProjectionRecord) -> Option<u64> {
    match row {
        ProjectionRecord::Goal { goal, pending, .. } => u64::try_from(goal.words.len())
            .ok()?
            .checked_add(tasks::phase_bytes(&goal.phase)?)?
            .checked_add(u64::try_from(pending.len()).ok()?.checked_mul(2)?),
        ProjectionRecord::Task { task, .. } => {
            u64::try_from(task.words.len()).ok()?.checked_add(tasks::phase_bytes(&task.phase)?)
        }
        ProjectionRecord::Milestone { milestone, .. } => {
            u64::try_from(milestone.words.len()).ok()?.checked_add(match &milestone.phase {
                Some(phase) => tasks::phase_bytes(phase)?,
                None => 0,
            })
        }
    }
}

fn valid_number(number: u64, maximum: u64) -> bool {
    number != 0 && number <= maximum
}

fn phase_within(phase: &tasks::Phase, limits: &Limits) -> bool {
    match tasks::phase_bytes(phase) {
        Some(bytes) => bytes <= u64::from(limits.tasks.result_bytes) * 2,
        None => false,
    }
}

pub(crate) fn restore(core: &mut Core, limits: &Limits, row: ProjectionRecord) -> bool {
    match row {
        ProjectionRecord::Goal { goal, pending, changed, fed_at } => {
            restore_goal(core, limits, goal, pending, changed, fed_at)
        }
        ProjectionRecord::Task { goal, task } => restore_task(core, limits, goal, task),
        ProjectionRecord::Milestone { goal, milestone } => restore_milestone(core, limits, goal, milestone),
    }
}

fn restore_goal(
    core: &mut Core,
    limits: &Limits,
    goal: ProjectionGoal,
    pending: Box<[u16]>,
    changed: bool,
    fed_at: Option<u64>,
) -> bool {
    let deployment = core.counters.deployment();
    let fed_after = match fed_at {
        Some(number) => number > deployment.commits,
        None => false,
    };
    if !valid_number(goal.number, deployment.tasks)
        || core.authority.policy(goal.project).is_none()
        || goal.words.len() > usize::try_from(limits.tasks.spec_bytes).expect("u32 fits usize")
        || !phase_within(&goal.phase, limits)
        || fed_after
        || pending.len() > core.connectors.len()
        || core.projections.contains_key(&goal.number)
    {
        return false;
    }
    let closing = match goal.phase {
        tasks::Phase::Ended(_) => true,
        tasks::Phase::Waiting | tasks::Phase::Active(_) | tasks::Phase::Closing(_) | tasks::Phase::Held { .. } => false,
    };
    if !closing && !pending.is_empty() {
        return false;
    }
    let mut seen = List::with_capacity(limits.connectors);
    for connector in &pending {
        if !core.connectors.contains(connector) || seen.as_slice().contains(connector) {
            return false;
        }
        seen.push(*connector).expect("bounded restored connector cohort");
    }
    core.projections
        .insert(
            goal.number,
            Box::new(Projection { goal, pending, changed, fed_at, plan: Box::new([]), milestones: Box::new([]) }),
        )
        .is_ok()
}

fn restore_task(core: &mut Core, limits: &Limits, goal: u64, task: ProjectionTask) -> bool {
    if !valid_number(task.number, core.counters.deployment().tasks)
        || task.words.len() > usize::try_from(limits.tasks.spec_bytes).expect("u32 fits usize")
        || !phase_within(&task.phase, limits)
    {
        return false;
    }
    let Some(state) = core.projections.get_mut(&goal) else { return false };
    let mut plan = List::with_capacity(limits.tasks.tree_tasks);
    for old in &state.plan {
        if old.number == task.number || plan.push(old.clone()).is_err() {
            return false;
        }
    }
    if plan.push(task).is_err() {
        return false;
    }
    state.plan = plan.into_boxed();
    true
}

fn restore_milestone(core: &mut Core, limits: &Limits, goal: u64, milestone: ProjectionMilestone) -> bool {
    let (task, position, shape) = match milestone.identity {
        MilestoneId::Lifecycle { task, position } => {
            (task, position, milestone.phase.is_some() && milestone.change.is_none() && milestone.words.is_empty())
        }
        MilestoneId::Revision { task, revision } => {
            (task, revision, milestone.phase.is_none() && milestone.change.is_some())
        }
    };
    let valid_phase = match &milestone.phase {
        Some(phase) => phase_within(phase, limits),
        None => true,
    };
    if !valid_number(task, core.counters.deployment().tasks)
        || position == 0
        || !shape
        || !valid_phase
        || milestone.words.len()
            > usize::try_from(limits.tasks.message_bytes.max(limits.tasks.result_bytes)).expect("u32 fits usize")
    {
        return false;
    }
    let Some(state) = core.projections.get_mut(&goal) else { return false };
    let mut milestones = List::with_capacity(crate::routing::room_max(limits).expect("valid feed room").writes);
    for old in &state.milestones {
        if old.identity == milestone.identity || milestones.push(old.clone()).is_err() {
            return false;
        }
    }
    if milestones.push(milestone).is_err() {
        return false;
    }
    state.milestones = milestones.into_boxed();
    true
}
