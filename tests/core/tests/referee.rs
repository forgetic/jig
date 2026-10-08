//! Misbehaving outside peers deliberately contradict each promised invariant.
use std::collections::{BTreeMap, BTreeSet};

use jig_core_world::referee::*;
use jig_test_system::{Copy, Name as SystemName, Observed as Effect, SystemKey, ValueObserved};

fn pattern() -> Pattern {
    Pattern { base: vec![], terminal: b"owned".to_vec(), open: true }
}

fn name() -> Name {
    Name { connector: 1, path: vec![b"owned".to_vec(), b"item".to_vec()] }
}

fn scope() -> Scope {
    Scope {
        budget: 100,
        tools: 15,
        grants: vec![Grant { connector: 1, kind: 5, pattern: pattern() }],
        executors: BTreeSet::from([(0, 1)]),
        descendants: 3,
        depth: 2,
        notes: 7,
        ..Scope::default()
    }
}

fn policy(recovery: Recovery) -> Policy {
    Policy {
        deployment: scope(),
        projects: BTreeMap::from([(1, scope())]),
        people: BTreeMap::from([((1, 1), scope())]),
        effect_accepters: BTreeSet::from([(1, 1)]),
        requirements: vec![],
        implications: BTreeSet::new(),
        kinds: BTreeMap::from([((1, 5), Kind { recovery, price: 3 })]),
        owned: vec![(1, pattern())],
        participating: BTreeSet::new(),
        mechanics: BTreeSet::new(),
        delivery_bound: 20,
        uncertainty_bound: 30,
        story_steps: 10,
    }
}

fn task(number: u64) -> Task {
    Task {
        number,
        project: 1,
        source: Source::Person(1),
        scope: scope(),
        executor: (0, 1),
        budget: 100,
        spent: 3,
        spent_below: 0,
        reserved: 10,
        run_reserved: 10,
        run_spent: 0,
        attempt: 1,
        period: None,
        phase: Phase::Active,
        dependencies: vec![],
        delegates: vec![],
        pools: vec![],
        inbox: vec![],
        proposal: None,
    }
}

fn key() -> SystemKey {
    SystemKey { deployment: [31; 16], task: 1, attempt: 1, completion: 1, position: 0, purpose: 9 }
}

fn decision() -> Decision {
    Decision {
        connector: 1,
        key: key(),
        kind: 5,
        resources: vec![name()],
        condition: None,
        target: 13,
        judged_state: 13,
        accepted_by: None,
    }
}

fn snapshot() -> Snapshot {
    Snapshot {
        tasks: BTreeMap::from([(1, task(1))]),
        receipts: BTreeSet::from([Receipt::Task(1), Receipt::Claim(1, 1), Receipt::Outbox(1, key(), 1)]),
        decisions: vec![decision()],
        ..Snapshot::default()
    }
}

fn effect() -> Effect {
    Effect {
        key: key(),
        kind: 5,
        resources: vec![SystemName(name().path)],
        condition: None,
        target: 13,
        before: vec![ValueObserved { state: None, owner: None }],
        applied: true,
        copy: Copy::First,
    }
}

/// The misbehaving fake supplies observations through exactly the normal API.
struct Fake {
    referee: Referee,
    now: u64,
}

impl Fake {
    fn new(recovery: Recovery) -> Self {
        Self { referee: Referee::new(policy(recovery)), now: 1 }
    }
    fn send(&mut self, event: Observed) -> Result<(), Violation> {
        self.referee.observe(self.now, event)
    }
    fn commit(&mut self, number: u64, snapshot: Snapshot) -> Result<(), Violation> {
        self.send(Observed::Durable { number, snapshot })
    }
    fn ready(recovery: Recovery) -> Self {
        let mut fake = Self::new(recovery);
        fake.commit(1, snapshot()).expect("authorized initial decision");
        fake
    }
    fn caught(&mut self, event: Observed, promise: Promise) {
        let failure = self.send(event).expect_err("the misbehaving fake must be caught");
        assert_eq!(failure.promise, promise, "{failure:?}");
    }
}

#[test]
fn authority_rejects_task_creation_beyond_each_source_ceiling() {
    for source in [Source::Person(1), Source::Deployment, Source::Task(2)] {
        let mut fake = Fake::new(Recovery::Keyed);
        let mut rows = snapshot();
        let mut parent = task(2);
        parent.scope.budget = 10;
        rows.tasks.insert(2, parent);
        rows.tasks.get_mut(&1).expect("task").source = source;
        match source {
            Source::Person(_) => {
                fake.referee.policy.people.get_mut(&(1, 1)).expect("role").budget = 10;
            }
            Source::Deployment => {
                fake.referee.policy.deployment.budget = 10;
            }
            Source::Task(_) => {}
        }
        assert_eq!(fake.commit(1, rows).expect_err("creation exceeds its source").promise, Promise::Authority);
    }
}

#[test]
fn authority_rejects_grants_tools_note_scopes_and_delegation_outside_policy() {
    for variant in 0..5 {
        let mut fake = Fake::new(Recovery::Keyed);
        let mut rows = snapshot();
        let scope = &mut rows.tasks.get_mut(&1).expect("task").scope;
        match variant {
            0 => scope.grants[0].kind = 99,
            1 => scope.tools = 16,
            2 => scope.notes = 8,
            3 => {
                scope.executors.insert((0, 99));
            }
            4 => scope.depth = 3,
            _ => unreachable!(),
        }
        assert_eq!(fake.commit(1, rows).expect_err("scope widened").promise, Promise::Authority);
    }
}

#[test]
fn authority_checks_policy_at_the_effects_decision_and_the_actual_payload() {
    let mut fake = Fake::new(Recovery::Keyed);
    let mut rows = snapshot();
    rows.decisions.clear();
    fake.commit(1, rows).expect("task admitted");
    fake.referee.policy.projects.get_mut(&1).expect("project").grants.clear();
    assert_eq!(fake.commit(2, snapshot()).expect_err("policy changed before effect").promise, Promise::Authority);
    for variant in 0..4 {
        let mut fake = Fake::ready(Recovery::Keyed);
        let mut copy = effect();
        match variant {
            0 => copy.kind = 6,
            1 => copy.resources[0].0[0] = b"foreign".to_vec(),
            2 => copy.target = 14,
            3 => copy.condition = Some(7),
            _ => unreachable!(),
        }
        fake.caught(Observed::Effect { connector: 1, effect: copy }, Promise::Authority);
    }
}

#[test]
fn authority_requires_a_met_observation_within_freshness_when_decided() {
    for (state, at) in [(Some(12), 1), (Some(13), 0)] {
        let mut fake = Fake::new(Recovery::Keyed);
        fake.referee.policy.requirements.push(Requirement {
            project: None,
            connector: 1,
            kind: 5,
            pattern: pattern(),
            judge: 2,
            freshness: Some(2),
        });
        fake.send(Observed::Read { resource: Name { connector: 2, path: name().path }, state, at })
            .expect("fact observed");
        fake.now = 4;
        assert_eq!(fake.commit(1, snapshot()).expect_err("wrong or stale requirement").promise, Promise::Authority);
    }
}

#[test]
fn authority_requires_guarded_requirements_to_be_checked_by_the_system() {
    let mut fake = Fake::new(Recovery::Conditional);
    fake.referee.policy.requirements.push(Requirement {
        project: None,
        connector: 1,
        kind: 5,
        pattern: pattern(),
        judge: 1,
        freshness: None,
    });
    assert_eq!(fake.commit(1, snapshot()).expect_err("guard omitted").promise, Promise::Authority);
}

#[test]
fn spend_rejects_overdrawn_direct_below_and_reserved_balances() {
    for variant in 0..3 {
        let mut fake = Fake::new(Recovery::Keyed);
        let mut rows = snapshot();
        let task = rows.tasks.get_mut(&1).expect("task");
        match variant {
            0 => task.spent = 101,
            1 => task.spent_below = 98,
            2 => task.reserved = 98,
            _ => unreachable!(),
        }
        assert_eq!(fake.commit(1, rows).expect_err("overdrawn allotment").promise, Promise::Spend);
    }
}

#[test]
fn spend_requires_the_maximum_effect_price_in_the_deciding_commit() {
    let mut fake = Fake::new(Recovery::Keyed);
    let mut rows = snapshot();
    rows.tasks.get_mut(&1).expect("task").spent = 2;
    assert_eq!(fake.commit(1, rows).expect_err("undercharged effect").promise, Promise::Spend);
}

#[test]
fn spend_requires_a_reservation_before_assignment_and_each_completion() {
    let mut fake = Fake::ready(Recovery::Keyed);
    fake.caught(Observed::Assigned { task: 1, attempt: 1, budget: 11 }, Promise::Spend);
    let mut fake = Fake::ready(Recovery::Keyed);
    fake.send(Observed::Assigned { task: 1, attempt: 1, budget: 10 }).expect("reserved");
    fake.caught(Observed::Completion { task: 1, attempt: 1, cumulative: 11 }, Promise::Spend);
}

#[test]
fn spend_allows_only_the_lost_unacknowledged_turn_bound_beyond_budget() {
    let mut fake = Fake::ready(Recovery::Keyed);
    fake.send(Observed::Assigned { task: 1, attempt: 1, budget: 10 }).expect("reserved assignment");
    fake.send(Observed::LostTurns { task: 1, attempt: 1, turns: 2, spent: 4 }).expect("lost completions observed");
    fake.send(Observed::Spend { task: 1, spent: 104, lost_bound: 4 }).expect("permitted lost spend");
    fake.caught(Observed::Spend { task: 1, spent: 105, lost_bound: 4 }, Promise::Spend);
}

#[test]
fn once_catches_two_applied_keyed_or_conditional_copies_across_restart() {
    for recovery in [Recovery::Keyed, Recovery::Conditional] {
        let mut fake = Fake::ready(recovery);
        fake.send(Observed::Effect { connector: 1, effect: effect() }).expect("first copy");
        fake.send(Observed::Restart).expect("cold start retains evidence");
        let mut copy = effect();
        copy.copy = Copy::Late;
        fake.caught(Observed::Effect { connector: 1, effect: copy }, Promise::Once);
    }
}

#[test]
fn once_catches_a_repeated_idempotent_set_with_a_different_target() {
    let mut fake = Fake::ready(Recovery::Idempotent);
    fake.send(Observed::Effect { connector: 1, effect: effect() }).expect("first set");
    let mut rows = snapshot();
    rows.decisions[0].target = 14;
    assert_eq!(fake.commit(2, rows).expect_err("idempotent target changed").promise, Promise::Once);
}

#[test]
fn once_catches_unrecoverable_repetition_without_a_person_and_accepts_the_decision() {
    let mut fake = Fake::ready(Recovery::Unrecoverable);
    fake.send(Observed::Effect { connector: 1, effect: effect() }).expect("first send");
    fake.send(Observed::Uncertain { connector: 1, key: key() }).expect("uncertain");
    fake.send(Observed::Restart).expect("uncertainty survives");
    fake.caught(Observed::Effect { connector: 1, effect: effect() }, Promise::Once);
    fake.send(Observed::RetryDecided { connector: 1, key: key() }).expect("person decided");
    fake.send(Observed::Effect { connector: 1, effect: effect() }).expect("explicit retry");
}

#[test]
fn once_catches_partial_and_split_task_batches() {
    let mut fake = Fake::ready(Recovery::Keyed);
    fake.caught(Observed::Batch { members: vec![1, 2] }, Promise::Once);
    let mut rows = snapshot();
    rows.tasks.insert(2, task(2));
    fake.commit(2, rows).expect("another separately created task");
    fake.caught(Observed::Batch { members: vec![1, 2] }, Promise::Once);
}

#[test]
fn commit_catches_every_class_of_delivery_before_its_durable_prerequisite() {
    for receipt in [
        Receipt::Task(1),
        Receipt::Claim(1, 1),
        Receipt::Turn(1, 1, 1),
        Receipt::Terminal(1, 1),
        Receipt::Call(1, 1, 1, 0),
        Receipt::SignIn(1),
        Receipt::Party(1, [9; 16]),
        Receipt::Outbox(1, key(), 1),
        Receipt::Ended(1),
        Receipt::Word(1, 1),
    ] {
        let mut fake = Fake::new(Recovery::Keyed);
        fake.caught(Observed::Released { requires: vec![receipt] }, Promise::Commit);
    }
    let mut fake = Fake::new(Recovery::Keyed);
    fake.caught(Observed::Effect { connector: 1, effect: effect() }, Promise::Commit);
    fake.caught(Observed::Assigned { task: 1, attempt: 1, budget: 10 }, Promise::Commit);
}

#[test]
fn commit_catches_a_store_that_loses_a_previously_observed_receipt() {
    let mut fake = Fake::ready(Recovery::Keyed);
    let mut rows = snapshot();
    rows.receipts.remove(&Receipt::Claim(1, 1));
    fake.commit(2, rows).expect("misbehaving store lost claim");
    fake.send(Observed::Restart).expect("restart");
    fake.caught(Observed::Released { requires: vec![Receipt::Claim(1, 1)] }, Promise::Commit);
}

#[test]
fn order_catches_starting_before_dependencies_are_done_and_closed() {
    let mut fake = Fake::new(Recovery::Keyed);
    let mut rows = snapshot();
    rows.tasks.get_mut(&1).expect("task").dependencies = vec![2];
    rows.tasks.insert(2, task(2));
    fake.commit(1, rows).expect("waiting dependency admitted");
    fake.caught(Observed::Assigned { task: 1, attempt: 1, budget: 10 }, Promise::Order);
}

#[test]
fn order_catches_two_writer_slots_for_the_same_resource() {
    let mut fake = Fake::new(Recovery::Keyed);
    let mut rows = snapshot();
    rows.writers = vec![(name(), 1, 1), (name(), 2, 1)];
    assert_eq!(fake.commit(1, rows).expect_err("two writers").promise, Promise::Order);
}

#[test]
fn order_catches_pool_over_admission_and_admission_while_shrinking() {
    let mut fake = Fake::new(Recovery::Keyed);
    let mut rows = snapshot();
    for number in 1..=2 {
        let mut task = task(number);
        task.pools = vec![name()];
        rows.tasks.insert(number, task);
    }
    rows.pools.insert(name(), 1);
    assert_eq!(fake.commit(1, rows.clone()).expect_err("over admission").promise, Promise::Order);
    let mut fake = Fake::new(Recovery::Keyed);
    rows.pools.insert(name(), 2);
    fake.commit(1, rows.clone()).expect("two slots");
    rows.pools.insert(name(), 1);
    fake.commit(2, rows.clone()).expect("existing holders drain");
    let mut third = task(3);
    third.pools = vec![name()];
    rows.tasks.insert(3, third);
    assert_eq!(fake.commit(3, rows).expect_err("new holder while draining").promise, Promise::Order);
}

#[test]
fn order_catches_applying_a_condition_to_a_different_prior_state() {
    let mut fake = Fake::new(Recovery::Conditional);
    let mut rows = snapshot();
    rows.decisions[0].condition = Some(7);
    fake.commit(1, rows).expect("guarded decision");
    let mut copy = effect();
    copy.condition = Some(7);
    copy.before[0].state = Some(8);
    fake.caught(Observed::Effect { connector: 1, effect: copy }, Promise::Order);
}

#[test]
fn order_catches_a_parent_ending_before_its_delegate() {
    let mut fake = Fake::new(Recovery::Keyed);
    let mut rows = snapshot();
    rows.decisions.clear();
    let parent = rows.tasks.get_mut(&1).expect("parent");
    parent.phase = Phase::Done;
    parent.delegates = vec![2];
    rows.tasks.insert(2, task(2));
    assert_eq!(fake.commit(1, rows).expect_err("delegate is still live").promise, Promise::Order);
}

#[test]
fn nothing_lost_catches_words_and_results_missing_their_deadlines() {
    let mut fake = Fake::ready(Recovery::Keyed);
    fake.send(Observed::Words { task: 1, message: 9 }).expect("words accepted");
    fake.now = 22;
    fake.caught(Observed::Tick, Promise::Lost);
    let mut fake = Fake::ready(Recovery::Keyed);
    let mut rows = snapshot();
    rows.decisions.clear();
    rows.tasks.get_mut(&1).expect("task").phase = Phase::Done;
    fake.commit(2, rows).expect("result committed");
    fake.now = 22;
    fake.caught(Observed::Tick, Promise::Lost);
}

#[test]
fn nothing_lost_catches_a_proposal_hidden_from_every_live_holder() {
    let mut fake = Fake::new(Recovery::Keyed);
    let mut rows = snapshot();
    rows.tasks.get_mut(&1).expect("task").proposal = Some((8, Source::Person(999)));
    assert_eq!(fake.commit(1, rows).expect_err("proposal invisible").promise, Promise::Lost);
}

#[test]
fn nothing_lost_catches_uncertainty_that_restarts_keep_postponing() {
    let mut fake = Fake::ready(Recovery::Keyed);
    fake.send(Observed::Uncertain { connector: 1, key: key() }).expect("uncertain");
    for now in [10, 20, 30] {
        fake.now = now;
        fake.send(Observed::Restart).expect("deadline not reset");
    }
    fake.now = 32;
    fake.caught(Observed::Restart, Promise::Lost);
}

#[test]
fn ownership_catches_writes_outside_the_owned_namespace() {
    let mut fake = Fake::ready(Recovery::Keyed);
    fake.referee.policy.owned.clear();
    fake.caught(Observed::Effect { connector: 1, effect: effect() }, Promise::Ownership);
}

#[test]
fn ownership_catches_overwriting_another_hands_unchecked_change() {
    let mut fake = Fake::ready(Recovery::Keyed);
    let mut copy = effect();
    copy.before[0].state = Some(12);
    fake.caught(Observed::Effect { connector: 1, effect: copy }, Promise::Ownership);
}

#[test]
fn bounded_catches_heap_growth_and_a_story_that_never_ends() {
    let mut fake = Fake::ready(Recovery::Keyed);
    fake.caught(Observed::Heap { held: 11, maximum: 10 }, Promise::Bounded);
    for _ in 0..10 {
        fake.send(Observed::Tick).expect("within story steps");
    }
    fake.caught(Observed::Tick, Promise::Bounded);
}

#[test]
fn legitimate_repeated_sets_visible_proposals_and_delivered_words_remain_valid() {
    let mut fake = Fake::ready(Recovery::Idempotent);
    for _ in 0..2 {
        fake.send(Observed::Effect { connector: 1, effect: effect() }).expect("same idempotent target");
    }
    fake.send(Observed::Words { task: 1, message: 9 }).expect("words accepted");
    fake.send(Observed::Reached(Receipt::Word(1, 9))).expect("words delivered");
    let mut rows = snapshot();
    rows.tasks.get_mut(&1).expect("task").proposal = Some((8, Source::Person(1)));
    fake.commit(2, rows).expect("visible proposal may wait");
    fake.now = 100;
    fake.send(Observed::Restart).expect("satisfied deliveries survive restart");
}

#[test]
fn authority_catches_lifetime_delegate_exhaustion_even_after_children_end() {
    let mut fake = Fake::new(Recovery::Keyed);
    let mut rows = snapshot();
    rows.decisions.clear();
    fake.commit(1, rows.clone()).expect("parent admitted");
    for number in 2..=4 {
        let mut child = task(number);
        child.source = Source::Task(1);
        child.phase = Phase::Done;
        rows.tasks.insert(number, child);
        fake.commit(number, rows.clone()).expect("authorized child");
        fake.send(Observed::Reached(Receipt::Ended(number))).expect("child result delivered");
    }
    let mut fourth = task(5);
    fourth.source = Source::Task(1);
    rows.tasks.insert(5, fourth);
    assert_eq!(fake.commit(5, rows).expect_err("lifetime capacity exhausted").promise, Promise::Authority);
}

#[test]
fn spend_aggregates_all_effect_maxima_in_one_atomic_commit() {
    let mut fake = Fake::new(Recovery::Keyed);
    let mut rows = snapshot();
    let mut second = decision();
    second.key.purpose = 10;
    rows.decisions.push(second);
    assert_eq!(fake.commit(1, rows).expect_err("two maxima charged only once").promise, Promise::Spend);
}

#[test]
fn skeins_referee_drives_the_same_observations_and_keeps_deadlines_across_restart() {
    use skein_lib::Time;
    use skein_world::domain::{Referee as Scenario, Verdict};
    let mut scenario = Scenario::new(Referee::new(policy(Recovery::Keyed)));
    let mut out = vec![];
    scenario.observe(Time::from_nanos(1), Observed::Durable { number: 1, snapshot: snapshot() }, &mut out);
    scenario.observe(Time::from_nanos(1), Observed::Words { task: 1, message: 9 }, &mut out);
    assert_eq!(scenario.next_deadline(), Some(Time::from_nanos(21)));
    scenario.observe(Time::from_nanos(10), Observed::Restart, &mut out);
    assert_eq!(scenario.next_deadline(), Some(Time::from_nanos(21)));
    scenario.fire(Time::from_nanos(22), &mut out);
    assert!(matches!(scenario.verdict(), Verdict::Failed(_)));
}

#[test]
fn authority_catches_claiming_a_holder_exception_without_durable_acceptance() {
    let mut fake = Fake::new(Recovery::Keyed);
    let mut rows = snapshot();
    rows.decisions[0].accepted_by = Some(Source::Person(1));
    assert_eq!(fake.commit(1, rows).expect_err("no durable proposal acceptance").promise, Promise::Authority);
}

#[test]
fn nothing_lost_catches_a_result_sent_to_the_wrong_requester() {
    let mut fake = Fake::ready(Recovery::Keyed);
    let mut rows = snapshot();
    rows.decisions.clear();
    rows.tasks.get_mut(&1).expect("task").phase = Phase::Done;
    fake.commit(2, rows).expect("result committed");
    fake.caught(Observed::Result { task: 1, to: Source::Person(2) }, Promise::Lost);
}

#[test]
fn commit_catches_a_cold_store_that_lost_something_a_host_already_saw() {
    let mut fake = Fake::ready(Recovery::Keyed);
    fake.send(Observed::Released { requires: vec![Receipt::Claim(1, 1)] }).expect("durable claim released");
    fake.caught(Observed::ColdStore { receipts: BTreeSet::new() }, Promise::Commit);
}

#[test]
fn durable_words_reach_the_task_even_while_its_requester_keeps_execution_held() {
    let mut fake = Fake::new(Recovery::Keyed);
    let mut rows = snapshot();
    rows.tasks.get_mut(&1).expect("task").phase = Phase::Held;
    rows.receipts.insert(Receipt::Word(1, 9));
    fake.commit(1, rows).expect("words kept in held task inbox");
    fake.send(Observed::Words { task: 1, message: 9 }).expect("person's words accepted");
    fake.now = 100;
    fake.send(Observed::Restart).expect("execution need not run to retain delivered words");
}
