use jig_core_tasks::{Active, End, Event, Key, Party, Phase, Refusal, Stored, TaskResult};
use jig_tasks_world::{LIMITS, Reply, World, task};
use skein_lib::Wall;

#[test]
fn make_and_claim_have_independent_before_and_after_durable_cuts() {
    let mut w = World::new(20, LIMITS);
    let reply_to = w.to();
    w.stage(Event::Make { reply_to, creator: Party::Person(1), batch: Box::new([task(1, &[])]) });
    assert_eq!(w.live(), 0);
    assert!(w.activations.is_empty());
    w.restart();
    assert_eq!(w.live(), 0);
    let reply_to = w.to();
    w.stage(Event::Make { reply_to, creator: Party::Person(1), batch: Box::new([task(2, &[])]) });
    w.durable();
    assert!(w.activations.is_empty());
    w.restart();
    assert!(w.activations.contains(&2));
    let reply_to = w.to();
    w.send(Event::Prepare { reply_to, task: 2 });
    let reply_to = w.to();
    w.stage(Event::Claim { reply_to, task: 2, attempt: 10, budget: 100 });
    assert!(w.runs.is_empty());
    w.restart();
    assert_eq!(w.record(2).attempt, 0);
    assert!(w.activations.contains(&2));
    let reply_to = w.to();
    w.send(Event::Prepare { reply_to, task: 2 });
    let reply_to = w.to();
    w.stage(Event::Claim { reply_to, task: 2, attempt: 11, budget: 100 });
    w.durable();
    assert!(w.runs.is_empty());
    w.restart();
    assert_eq!(w.runs.get(&2), Some(&11));
    assert_eq!(w.record(2).phase, Phase::Active(Active::Claimed { attempt: 11 }));
    w.finish(2);
    w.settle(2);
}

#[test]
fn stale_claims_and_replayed_terminal_are_typed_and_do_not_mutate() {
    let mut w = World::new(22, LIMITS);
    w.make(Party::Person(1), vec![task(1, &[])]);
    w.claim(1, 4);
    let reply_to = w.to();
    w.send(Event::Claim { reply_to, task: 1, attempt: 4, budget: 100 });
    assert!(
        matches!(w.replies.last_key_value().expect("reply").1, Reply::Refused(problem) if problem.why == Refusal::Attempt)
    );
    w.terminal(1, End::Refused);
    let record = w.record(1).clone();
    let reply_to = w.to();
    w.send(Event::Activation {
        saved: None,
        reply_to,
        task: 1,
        attempt: 4,
        end: End::Failed(jig_core_tasks::Class::Lost),
        cause: jig_core_tasks::Cause::Unpriced,
    });
    assert_eq!(w.record(1), &record);
    assert!(matches!(
        w.replies.last_key_value().expect("reply").1,
        Reply::Acknowledged(jig_core_tasks::Accepted::Already)
    ));
}

#[test]
fn a_crash_before_finished_decision_resumes_current_attempt() {
    let mut w = World::new(24, LIMITS);
    w.make(Party::Person(1), vec![task(1, &[])]);
    w.claim(1, 1);
    let reply_to = w.to();
    w.stage(Event::Activation {
        saved: None,
        reply_to,
        task: 1,
        attempt: 1,
        end: End::Finished { result: TaskResult::Report { words: Box::new([1]) }, cancel_delegates: false },
        cause: jig_core_tasks::Cause::Unpriced,
    });
    assert!(w.results.is_empty());
    assert!(w.closing.is_empty());
    w.restart();
    assert_eq!(w.runs.get(&1), Some(&1));
    w.finish(1);
    w.settle(1);
}

#[test]
fn restore_rejects_corrupt_links_cycles_and_contracts_without_panicking() {
    use jig_core_tasks::{Domain, Request, max_out, step};
    use skein_lib::{Env, Queue, Time, Wall};
    let mut source = World::new(25, LIMITS);
    source.make(Party::Person(1), vec![task(1, &[]), task(2, &[])]);
    for corrupt in 0..4 {
        let mut domain = Domain::new(&LIMITS, 25, Box::new([1]));
        let env = Env { limits: LIMITS, now: Time::ZERO, wall: Wall::EPOCH };
        let mut out = Queue::with_capacity(max_out(&LIMITS));
        for number in [1, 2] {
            let mut record = source.record(number).clone();
            match corrupt {
                0 => record.root = 99,
                1 => {
                    record.phase = Phase::Waiting;
                    record.dependencies = Box::new([3 - number]);
                    record.waiting_on = Box::new([3 - number]);
                }
                2 => record.contract = jig_core_tasks::Contract::Report { words: 33 },
                3 => {
                    if number == 1 {
                        record.delegates = Box::new([2]);
                    } else {
                        record.requester = Party::Task(1);
                        record.root = 1;
                        record.depth = 1;
                    }
                }
                _ => unreachable!(),
            }
            step(&mut domain, &env, Event::Restore { record: Stored::Live(Box::new(record)) }, &mut out);
        }
        step(&mut domain, &env, Event::Restored, &mut out);
        let mut rejected = false;
        while let Some(request) = out.pop() {
            if matches!(request, Request::RestoreRefused { .. }) {
                rejected = true;
            }
        }
        assert!(rejected);
    }
}

#[test]
fn terminal_and_settlement_each_have_a_durable_cut_before_delivery() {
    let mut w = World::new(26, LIMITS);
    w.make(Party::Person(1), vec![task(1, &[])]);
    w.claim(1, 1);
    let reply_to = w.to();
    w.stage(Event::Activation {
        saved: None,
        reply_to,
        task: 1,
        attempt: 1,
        end: End::Finished { result: TaskResult::Report { words: Box::new([1]) }, cancel_delegates: false },
        cause: jig_core_tasks::Cause::Unpriced,
    });
    w.durable();
    assert!(w.closing.is_empty());
    assert!(w.results.is_empty());
    w.restart();
    assert!(w.closing.contains(&1));
    assert!(!w.runs.contains_key(&1));
    w.stage(Event::EffectsSettled { task: 1 });
    w.durable();
    assert!(w.results.is_empty());
    w.restart();
    w.observe(jig_tasks_world::referee::Seen::Settled { task: 1 });
    w.stage(Event::Settled { task: 1 });
    w.durable();
    assert_eq!(w.results.len(), 1);
    assert!(!w.records.contains_key(&Key::Live(1)));
    w.restart();
    assert_eq!(w.results.len(), 1);
    assert!(w.closing.is_empty());
}

#[test]
fn wall_correction_does_not_move_a_live_backoff_but_restore_reprojects_it() {
    use skein_lib::{Duration, Wall};
    let mut w = World::new(28, LIMITS);
    w.make(Party::Person(1), vec![task(1, &[])]);
    w.claim(1, 1);
    w.terminal(1, End::Refused);
    w.env.wall = Wall::from_nanos(Duration::from_secs(100).as_nanos());
    w.advance();
    assert!(w.activations.contains(&1));
    w.claim(1, 2);
    w.terminal(1, End::Refused);
    w.env.wall = Wall::from_nanos(Duration::from_secs(200).as_nanos());
    w.restart();
    w.advance();
    assert!(w.activations.contains(&1));
}

#[test]
fn delegate_cancel_cut_after_durability_stops_adopted_runs_then_closes_deepest() {
    let mut w = World::new(27, LIMITS);
    w.make(Party::Person(1), vec![task(1, &[])]);
    w.claim(1, 1);
    w.make(Party::Task(1), vec![task(2, &[])]);
    w.claim(2, 2);
    w.make(Party::Task(2), vec![task(3, &[])]);
    w.claim(3, 3);
    w.observe(jig_tasks_world::referee::Seen::Cancelled { task: 2 });
    let reply_to = w.to();
    w.stage(Event::Activation {
        saved: None,
        reply_to,
        task: 1,
        attempt: 1,
        end: End::Finished { result: TaskResult::Report { words: Box::new([1]) }, cancel_delegates: true },
        cause: jig_core_tasks::Cause::Unpriced,
    });
    w.durable();
    assert!(w.stops.is_empty());
    w.restart();
    assert_eq!(w.stops.len(), 2);
    w.complete_cancel();
    assert_eq!(w.results.len(), 3);
}

#[test]
fn dependency_progress_survives_restore_without_loading_historical_ends() {
    let mut w = World::new(29, LIMITS);
    w.make(Party::Person(1), vec![task(1, &[]), task(2, &[]), task(3, &[1, 2])]);
    w.claim(1, 1);
    w.finish(1);
    w.settle(1);
    assert_eq!(w.record(3).waiting_on.as_ref(), [2]);
    let before = w.records.clone();
    w.restart();
    assert_eq!(w.records, before);
    assert!(!w.activations.contains(&3));
    w.claim(2, 2);
    w.finish(2);
    w.settle(2);
    assert!(w.record(3).waiting_on.is_empty());
    w.restart();
    w.claim(3, 3);
    w.finish(3);
    w.settle(3);
    assert_eq!(w.results.len(), 3);
}

#[test]
fn restore_refuses_unrepresentable_eventual_actual_funding_postings() {
    use jig_core_tasks::{Cause, Domain, Funder, Request, step};
    use skein_lib::Queue;
    let mut source = World::new(30, LIMITS);
    source.make(Party::Person(1), vec![task(1, &[])]);
    source.claim(1, 1);
    source.terminal_cause(
        1,
        End::Finished { result: TaskResult::Report { words: Box::new([1]) }, cancel_delegates: false },
        Cause::Priced { cumulative: 1 },
    );
    let mut rows = source.records.values().cloned().collect::<Vec<_>>();
    for row in &mut rows {
        if let Stored::Ledger(ledger) = row
            && ledger.funder == (Funder::Period { project: 1, period: 0 })
        {
            ledger.numbers.spent_below = u64::MAX;
        }
    }
    let mut domain = Domain::new(&LIMITS, 30, Box::new([1]));
    let mut out = Queue::with_capacity(jig_core_tasks::max_out(&LIMITS));
    for record in rows {
        step(&mut domain, &source.env, Event::Restore { record }, &mut out);
    }
    step(&mut domain, &source.env, Event::Restored, &mut out);
    assert!(std::iter::from_fn(|| out.pop()).any(|request| matches!(request, Request::RestoreRefused { .. })));
}

#[test]
fn malformed_unfinished_dependencies_refuse_at_restore_entrance() {
    use jig_core_tasks::{Domain, Request, step};
    use skein_lib::Queue;
    let mut source = World::new(31, LIMITS);
    source.make(Party::Person(1), vec![task(1, &[])]);
    for waiting in [
        Box::new([99_u64]) as Box<[u64]>,
        (1..=u64::from(LIMITS.dependencies) + 1).collect::<Vec<_>>().into_boxed_slice(),
    ] {
        let mut record = source.record(1).clone();
        record.waiting_on = waiting;
        let mut domain = Domain::new(&LIMITS, 31, Box::new([1]));
        let mut out = Queue::with_capacity(jig_core_tasks::max_out(&LIMITS));
        step(&mut domain, &source.env, Event::Restore { record: Stored::Live(Box::new(record)) }, &mut out);
        assert!(
            matches!(out.pop(), Some(Request::RestoreRefused { .. })),
            "oversize/non-subset refused before Restored"
        );
        assert!(out.pop().is_none());
    }
}

#[test]
fn restore_refuses_a_missing_unfinished_live_dependency_before_activation() {
    use jig_core_tasks::{Domain, Request, step};
    use skein_lib::Queue;
    let mut source = World::new(32, LIMITS);
    source.make(Party::Person(1), vec![task(1, &[]), task(2, &[1])]);
    for phase in [Phase::Waiting, Phase::Active(Active::Due), Phase::Active(Active::Preparing)] {
        let mut rows = source.records.values().cloned().collect::<Vec<_>>();
        for row in &mut rows {
            if let Stored::Live(task) = row
                && task.number == 2
            {
                task.waiting_on = Box::new([]);
                task.phase = phase.clone();
            }
        }
        let mut domain = Domain::new(&LIMITS, 32, Box::new([1]));
        let mut out = Queue::with_capacity(jig_core_tasks::max_out(&LIMITS));
        for record in rows {
            step(&mut domain, &source.env, Event::Restore { record }, &mut out);
        }
        step(&mut domain, &source.env, Event::Restored, &mut out);
        let mut refused = false;
        while let Some(request) = out.pop() {
            assert!(!matches!(request, Request::Activate { .. } | Request::Adopt { .. }));
            refused |= matches!(request, Request::RestoreRefused { .. });
        }
        assert!(refused, "corrupt unfinished subset refuses before activation");
    }
}

#[test]
fn impossible_settled_live_and_inconsistent_ledger_states_refuse_at_restore_entrance() {
    use jig_core_tasks::{Domain, Funder, Request, Stage, step};
    use skein_lib::Queue;
    let mut source = World::new(33, LIMITS);
    source.open_period(1, 300);
    source.carve_pool(1, 200);
    let mut new = task(1, &[]);
    new.funder = Funder::Period { project: 1, period: 1 };
    source.make(Party::Person(1), vec![new]);
    source.claim(1, 1);
    source.finish(1);
    let mut task = source.record(1).clone();
    if let Phase::Closing(closing) = &mut task.phase {
        closing.stage = Stage::Settled;
    } else {
        panic!("closing fixture");
    }
    let mut invalid = vec![Stored::Live(Box::new(task))];
    for row in source.records.values() {
        if let Stored::Ledger(ledger) = row {
            let mut closed = *ledger;
            closed.closed = true;
            closed.numbers.reserved = closed.numbers.reserved.saturating_add(1);
            invalid.push(Stored::Ledger(closed));
            let mut spent = *ledger;
            spent.numbers.spent = 1;
            invalid.push(Stored::Ledger(spent));
        }
    }
    for record in invalid {
        let mut domain = Domain::new(&LIMITS, 33, Box::new([1]));
        let mut out = Queue::with_capacity(jig_core_tasks::max_out(&LIMITS));
        step(&mut domain, &source.env, Event::Restore { record }, &mut out);
        assert!(matches!(out.pop(), Some(Request::RestoreRefused { .. })));
        assert!(out.pop().is_none());
    }
}

#[test]
fn restore_refuses_task_funding_outside_its_requester_ancestry() {
    use jig_core_tasks::{Domain, Funder, Request, step};
    use skein_lib::Queue;
    let mut source = World::new(34, LIMITS);
    source.make(Party::Person(1), vec![task(1, &[]), task(2, &[])]);
    let mut rows = source.records.values().cloned().collect::<Vec<_>>();
    for row in &mut rows {
        match row {
            Stored::Live(task) if task.number == 1 => task.numbers.reserved = 100,
            Stored::Live(task) if task.number == 2 => task.funder = Funder::Task(1),
            Stored::Ledger(ledger) => ledger.numbers.reserved = 100,
            Stored::Live(_) | Stored::Ended(_) | Stored::Stub(_) | Stored::History(_) | Stored::PersonProposal(_) => {}
        }
    }
    let mut domain = Domain::new(&LIMITS, 34, Box::new([1]));
    let mut out = Queue::with_capacity(jig_core_tasks::max_out(&LIMITS));
    for record in rows {
        step(&mut domain, &source.env, Event::Restore { record }, &mut out);
    }
    step(&mut domain, &source.env, Event::Restored, &mut out);
    assert!(std::iter::from_fn(|| out.pop()).any(|request| matches!(request, Request::RestoreRefused { .. })));
}

#[test]
fn unresolved_or_inconsistent_escalation_rows_refuse_before_retention() {
    use jig_core_tasks::{Domain, Escalation, EscalationHolder, Hold, Request, Was, step};
    use skein_lib::Queue;
    let mut source = World::new(36, LIMITS);
    source.make(Party::Person(1), vec![task(1, &[])]);
    for corrupt in 0..6 {
        let mut task = source.record(1).clone();
        task.phase = Phase::Held { was: Was::Active(Active::Due), why: Hold::Deadline };
        task.escalation = match corrupt {
            0 => Escalation::Routing { revision: 1 },
            1 => Escalation::Unheld { revision: 0 },
            2 => Escalation::Waiting { revision: 0, holder: EscalationHolder::Person(1), entry: 1, since: Wall::EPOCH },
            3 => Escalation::Waiting { revision: 1, holder: EscalationHolder::Person(2), entry: 1, since: Wall::EPOCH },
            4 => Escalation::Waiting {
                revision: 1,
                holder: EscalationHolder::Role { project: 2, role: 0 },
                entry: 1,
                since: Wall::EPOCH,
            },
            5 => Escalation::Rejected {
                revision: 1,
                by: 1,
                reason: vec![b'x'; usize::try_from(LIMITS.result_bytes).expect("configured bounded task result") + 1]
                    .into_boxed_slice(),
            },
            _ => unreachable!(),
        };
        let mut domain = Domain::new(&LIMITS, 36, Box::new([1]));
        let mut out = Queue::with_capacity(jig_core_tasks::max_out(&LIMITS));
        step(&mut domain, &source.env, Event::Restore { record: Stored::Live(Box::new(task)) }, &mut out);
        assert!(matches!(out.pop(), Some(Request::RestoreRefused { .. })));
        assert!(out.pop().is_none());
    }
}

#[test]
fn waiting_restore_requests_recheck_once_and_identical_recipient_changes_nothing() {
    use jig_core_tasks::{Domain, Escalation, EscalationHolder, Hold, Request, Was, step};
    use skein_lib::Queue;
    let mut source = World::new(37, LIMITS);
    source.make(Party::Person(1), vec![task(1, &[])]);
    for rejected in [false, true] {
        let mut domain = Domain::new(&LIMITS, 37, Box::new([1]));
        let mut out = Queue::with_capacity(jig_core_tasks::max_out(&LIMITS));
        for record in source.records.values() {
            let mut record = record.clone();
            if let Stored::Live(task) = &mut record {
                task.phase = Phase::Held { was: Was::Active(Active::Due), why: Hold::Deadline };
                task.escalation = if rejected {
                    Escalation::Rejected { revision: 4, by: 1, reason: b"no".as_slice().into() }
                } else {
                    Escalation::Waiting {
                        revision: 4,
                        holder: EscalationHolder::Person(1),
                        entry: 1,
                        since: source.env.wall,
                    }
                };
            }
            step(&mut domain, &source.env, Event::Restore { record }, &mut out);
        }
        assert!(out.is_empty());
        step(&mut domain, &source.env, Event::Restored, &mut out);
        let requests = std::iter::from_fn(|| out.pop()).collect::<Vec<_>>();
        assert_eq!(
            requests.iter().filter(|row| matches!(row, Request::EscalationNeeded { .. })).count(),
            usize::from(!rejected)
        );
        assert!(!requests.iter().any(|row| matches!(row, Request::Save { .. } | Request::Activate { .. })));
        let before = format!("{domain:?}");
        step(
            &mut domain,
            &source.env,
            Event::RoutedEscalation { task: 1, revision: 4, holder: EscalationHolder::Person(1), entry: 1 },
            &mut out,
        );
        assert!(out.is_empty(), "same selected recipient emits no save");
        assert_eq!(format!("{domain:?}"), before);
    }
}
