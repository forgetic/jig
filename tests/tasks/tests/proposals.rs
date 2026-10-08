use jig_core_tasks::{
    Event, Funder, MessageKind, Party, Proposal, ProposalAction, ProposalDecision, ProposalHolder, ProposalKind,
    ProposalState,
};
use jig_tasks_world::{LIMITS, Reply, World, task};

#[test]
fn a_proposal_reroute_only_applies_to_the_holder_and_revision_it_checked() {
    let mut world = World::new(801, LIMITS);
    assert_eq!(world.make(Party::Person(9), vec![task(1, &[])]), Reply::Made(vec![1]));
    assert_eq!(world.make(Party::Task(1), vec![task(2, &[])]), Reply::Made(vec![2]));
    let mut member = task(3, &[]);
    member.funder = Funder::Task(2);
    let proposal = Proposal {
        number: 100,
        proposer: 2,
        project: 1,
        action: ProposalAction::Batch(Box::new([member])),
        reason: Box::from(&b"need one delegate"[..]),
        as_holder: false,
        state: ProposalState::Pending { holder: ProposalHolder::Task(1), since: world.env.wall },
    };
    let reply_to = world.to();
    world.send(Event::Propose { reply_to, proposal });
    let revision = world.record(2).revision;
    let original = world.record(2).proposal.clone();
    world.send(Event::StalledProposal {
        proposer: 2,
        proposal: 100,
        from: ProposalHolder::Task(1),
        revision: revision - 1,
        holder: ProposalHolder::Person(9),
    });
    assert_eq!(world.record(2).proposal, original);
    world.send(Event::StalledProposal {
        proposer: 2,
        proposal: 100,
        from: ProposalHolder::Task(1),
        revision,
        holder: ProposalHolder::Person(9),
    });
    assert!(matches!(
        world.record(2).proposal.as_ref().expect("pending").state,
        ProposalState::Pending { holder: ProposalHolder::Person(9), .. }
    ));
    world.send(Event::StalledProposal {
        proposer: 2,
        proposal: 100,
        from: ProposalHolder::Task(1),
        revision,
        holder: ProposalHolder::Policy { project: 1, kind: ProposalKind::Batch },
    });
    assert!(matches!(
        world.record(2).proposal.as_ref().expect("pending").state,
        ProposalState::Pending { holder: ProposalHolder::Person(9), .. }
    ));
    let reply_to = world.to();
    world.send(Event::DecideProposal {
        reply_to,
        proposer: 2,
        proposal: 100,
        message: Some(101),
        by: Party::Person(9),
        decision: ProposalDecision::Reject { reason: Box::from(&b"not now"[..]) },
    });
    assert!(world.record(2).proposal.is_none());
    assert!(
        world
            .record(2)
            .inbox
            .iter()
            .any(|word| word.kind == MessageKind::ProposalDecision { proposal: 100, accepted: false })
    );
    world.restart();
}
