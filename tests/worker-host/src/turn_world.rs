//! A version-two peer world for the worker host (domain/hosts.md, sections
//! 6.4-6.7 and 11). The parent plays a workspace and agent; the engine's
//! fake store commits turns before acknowledging them. The link may lose
//! contact and replays the host's retained turns before its held answer.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use jig_worker_host::{
    self as host, AnswerV2, AnsweredCall, Ask, Assignment, AssignmentTyped, AssignmentV2, Delivery, DeliveryOutcome,
    EndingV2, Event, FinishV2, Limits, Reason, Reply, Request, Turn, Workspace,
};
use skein_lib::{Duration, Env, Queue, ReplyTo, Time, Token, Wall};

const RUN: Token = Token::new(31);
const ATTEMPT: Token = Token::new(932);
const SPACE: Token = Token::new(400);
const AGENT: Token = Token::new(500);

/// What the peers saw, counted independently of the host's private state.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Stats {
    pub transmissions: u32,
    pub commits: u32,
    pub turn_acks: u32,
    pub answers: u32,
    pub answer_acks: u32,
    pub deliveries: u32,
    pub relays: u32,
    pub replies: u32,
    pub saves: u32,
    pub releases: u32,
    pub facts: u32,
}

/// The committed conversation state received by the scripted agent.
pub struct StartedState {
    pub activation: u64,
    pub turns: Box<[Box<[u8]>]>,
    pub answered: Box<[AnsweredCall]>,
}

/// The call the scripted engine received from the host.
pub struct RelayedCall {
    pub name: Box<[u8]>,
    pub tool: Box<[u8]>,
    pub writes: bool,
    pub input: Box<[u8]>,
    pub deadline: Duration,
}

/// The message the scripted agent received from the host.
pub struct ReceivedMessage {
    pub name: Token,
    pub sender: Box<[u8]>,
    pub words: Box<[u8]>,
}

/// One host and its scripted capabilities and committed engine observations.
#[expect(clippy::struct_excessive_bools, reason = "independent link, agent, workspace and acknowledgement states")]
pub struct World {
    limits: Limits,
    env: Env<Limits>,
    host: host::Domain,
    out: Queue<Request>,
    events: VecDeque<Event>,
    connected: bool,
    owner: Option<Token>,
    reading: bool,
    agent_live: bool,
    stop_waits: bool,
    stop_pending: bool,
    has_workspace: bool,
    space_live: bool,
    answer: Option<AnswerV2>,
    answer_seen: Option<String>,
    answer_acked: bool,
    seen: BTreeMap<u32, Turn>,
    committed: BTreeSet<u32>,
    acked: BTreeSet<u32>,
    stats: Stats,
    typed_start: Option<StartedState>,
    typed_relay: Option<RelayedCall>,
    typed_reply: Option<Box<[u8]>>,
    typed_message: Option<ReceivedMessage>,
}

impl World {
    /// A small window that can hold two maximum bodies.
    #[must_use]
    pub fn new() -> Self {
        let mut limits = crate::Settings::calm(1).host;
        limits.slots = 1;
        limits.transcript_bytes = 512;
        limits.delivery_evidence_bytes = 128;
        limits.turn_bytes = 32;
        limits.turns = 2;
        limits.turn_queue_bytes = 64;
        limits.told = 2;
        assert!(host::worst_case(&limits).is_some());
        Self {
            limits,
            env: Env { now: Time::ZERO, wall: Wall::EPOCH, limits },
            host: host::Domain::new(&limits),
            out: Queue::with_capacity(host::max_out(&limits)),
            events: VecDeque::new(),
            connected: true,
            owner: None,
            reading: false,
            agent_live: false,
            stop_waits: false,
            stop_pending: false,
            has_workspace: false,
            space_live: false,
            answer: None,
            answer_seen: None,
            answer_acked: false,
            seen: BTreeMap::new(),
            committed: BTreeSet::new(),
            acked: BTreeSet::new(),
            stats: Stats::default(),
            typed_start: None,
            typed_relay: None,
            typed_reply: None,
            typed_message: None,
        }
    }

    #[must_use]
    pub const fn stats(&self) -> Stats {
        self.stats
    }

    #[must_use]
    pub fn reading(&self) -> bool {
        self.reading
    }

    #[must_use]
    pub fn answer(&self) -> Option<&AnswerV2> {
        self.answer.as_ref()
    }

    #[must_use]
    pub fn lost_facts(&self) -> u64 {
        self.host.told_lost()
    }

    /// Assigns one attempt. Its workspace prepares and its agent starts
    /// through the parent's immediate scripted terminals.
    pub fn assign(&mut self, workspace: bool) {
        self.has_workspace = workspace;
        self.send(Event::Unacknowledged { answers: u32::from(self.answer.is_some() && !self.answer_acked) });
        let workspace = if workspace { Some(Workspace { workstream: 3, items: Token::new(4) }) } else { None };
        let assignment = Assignment {
            run: RUN,
            attempt: ATTEMPT,
            workspace,
            save: true,
            charter: Box::from(&b"charter"[..]),
            snapshot: None,
            grants: Box::new([]),
        };
        self.send(Event::AssignV2 {
            reply_to: ReplyTo::new(RUN),
            assignment: AssignmentV2 { assignment, transcript: Some(Box::from(&b"prior turn"[..])) },
        });
        assert!(self.owner.is_some() && self.agent_live && self.reading);
    }

    /// Assign a resumed run with each turn and its settled call tail intact.
    pub fn assign_typed(&mut self, turns: Box<[Box<[u8]>]>, answered: Box<[AnsweredCall]>) {
        self.send(Event::Unacknowledged { answers: 0 });
        let assignment = Assignment {
            run: RUN,
            attempt: ATTEMPT,
            workspace: None,
            save: false,
            charter: Box::from(&b"charter"[..]),
            snapshot: None,
            grants: Box::new([]),
        };
        self.send(Event::AssignTyped {
            reply_to: ReplyTo::new(RUN),
            assignment: AssignmentTyped { assignment, turns, answered },
        });
        assert!(self.owner.is_some() && self.agent_live && self.reading);
    }

    /// The typed activation and committed state the scripted agent received.
    #[must_use]
    pub fn typed_start(&self) -> Option<&StartedState> {
        self.typed_start.as_ref()
    }

    /// The typed call the engine saw.
    #[must_use]
    pub fn typed_relay(&self) -> Option<&RelayedCall> {
        self.typed_relay.as_ref()
    }

    /// The name under which the agent received the call's answer.
    #[must_use]
    pub fn typed_reply(&self) -> Option<&[u8]> {
        self.typed_reply.as_deref()
    }

    /// The typed message the agent saw.
    #[must_use]
    pub fn typed_message(&self) -> Option<&ReceivedMessage> {
        self.typed_message.as_ref()
    }

    pub fn turn(&mut self, number: u32, body: &[u8]) {
        assert!(self.reading, "the agent waits for host credit");
        self.reading = false;
        let turn = Turn { turn: number, spent: u64::from(number) * 17, read: None, body: Box::from(body) };
        self.send(Event::Turn { owner: self.owner.expect("assigned"), turn });
    }

    pub fn fact(&mut self, text: &[u8]) {
        self.send(Event::Facts { owner: self.owner.expect("assigned"), fact: Box::from(text) });
    }

    pub fn deliver(&mut self) {
        self.send(Event::Called {
            owner: self.owner.expect("assigned"),
            call: Token::new(71),
            ask: Ask::DeliverV2 { title: Box::from(&b"title"[..]), body: Box::from(&b"body"[..]) },
        });
    }

    pub fn relay(&mut self) {
        self.send(Event::Called {
            owner: self.owner.expect("assigned"),
            call: Token::new(72),
            ask: Ask::Relay { body: Box::from(&b"read"[..]) },
        });
    }

    /// The agent calls an engine host tool under its opaque name.
    pub fn relay_typed(&mut self, writes: bool) {
        self.send(Event::CalledTyped {
            owner: self.owner.expect("assigned"),
            call: Box::from(&b"call-one"[..]),
            ask: Ask::RelayTyped {
                tool: Box::from(&b"inspect"[..]),
                writes,
                input: Box::from(&b"input words"[..]),
                deadline: Duration::from_nanos(37),
            },
        });
    }

    /// The engine sends a named message with its label and words.
    pub fn message_typed(&mut self) {
        self.send(Event::InboundTyped {
            run: RUN,
            attempt: ATTEMPT,
            name: Token::new(17),
            sender: Box::from(&b"requester"[..]),
            words: Box::from(&b"please check"[..]),
        });
    }

    pub fn finish(&mut self, turns: u32, parked: bool) {
        let finish = if parked { FinishV2::Parked } else { FinishV2::Ended { outcome: Box::from(&b"done"[..]) } };
        self.send(Event::FinishedV2 {
            owner: self.owner.expect("assigned"),
            turns,
            spent: u64::from(turns) * 17,
            finish,
        });
    }

    /// A stop can be held to check that saving waits for the agent's exit.
    pub fn hold_stop(&mut self) {
        self.stop_waits = true;
    }

    pub fn gone(&mut self) {
        assert!(self.stop_pending && self.agent_live);
        self.stop_pending = false;
        self.agent_live = false;
        self.send(Event::Gone { owner: self.owner.expect("assigned"), detail: Box::new([]) });
    }

    pub fn cancel_contact(&mut self) {
        self.send(Event::CancelAll { reason: Reason::Contact });
        self.resume();
    }

    pub fn shutdown(&mut self) {
        self.send(Event::CancelAll { reason: Reason::Shutdown });
        self.resume();
    }

    /// A channel loss does not drop host-owned turn bodies or the link's answer.
    pub fn lose_contact(&mut self) {
        self.connected = false;
    }

    /// Hello reports the run; the link then sends turns before the answer.
    pub fn reconnect(&mut self) {
        self.connected = true;
        self.send(Event::Report);
        for index in 0..self.host.retained_turns() {
            let (run, attempt, turn) = self.host.turn_at(index).expect("retained position");
            assert_eq!((run, attempt), (RUN, ATTEMPT));
            self.receive_turn(turn);
        }
        if let Some(answer) = self.answer.take() {
            self.receive_answer(&answer);
            self.answer = Some(answer);
        }
        self.drain_facts();
    }

    /// The engine says busy; the link resends the same retained body after
    /// its own backoff, which is below this timer-free host.
    pub fn retry_busy(&mut self, number: u32) {
        let turn = self.host.turn(RUN, ATTEMPT, number).expect("busy turn retained");
        if self.connected {
            self.receive_turn(turn);
        }
    }

    /// Fake store commit, then its one acknowledgement.
    pub fn commit_turn(&mut self, number: u32) {
        assert!(self.seen.contains_key(&number), "only a received turn can commit");
        assert!(self.committed.insert(number), "one commit per turn");
        self.stats.commits += 1;
        assert!(self.acked.insert(number), "one ACK per committed turn");
        self.stats.turn_acks += 1;
        self.send(Event::AcknowledgeTurn { run: RUN, attempt: ATTEMPT, turn: number });
    }

    pub fn acknowledge_answer(&mut self) {
        assert!(self.answer_seen.is_some(), "the engine received the answer");
        assert_eq!(self.committed.len(), self.seen.len(), "all turns commit before the answer");
        assert!(!self.answer_acked, "one answer ACK");
        self.answer_acked = true;
        self.stats.answer_acks += 1;
        self.send(Event::Unacknowledged { answers: 0 });
    }

    /// The external referee: one commit and ACK per observed turn, one
    /// answer and ACK, and no live parent capability after settlement.
    pub fn assert_settled(&self) {
        assert!(!self.agent_live && !self.space_live && !self.stop_pending);
        assert_eq!(self.seen.len(), self.committed.len());
        assert_eq!(self.committed, self.acked);
        assert!(self.answer_seen.is_some() && self.answer_acked);
        assert_eq!((self.stats.answers, self.stats.answer_acks), (1, 1));
        assert_eq!(self.stats.releases, u32::from(self.has_workspace));
    }

    fn send(&mut self, event: Event) {
        self.events.push_back(event);
        self.settle();
    }

    fn settle(&mut self) {
        for _ in 0..128 {
            let Some(event) = self.events.pop_front() else {
                self.host.reclaim();
                self.drain_facts();
                return;
            };
            host::step(&mut self.host, &self.env, event, &mut self.out);
            while let Some(request) = self.out.pop() {
                self.route(request);
            }
        }
        panic!("the peer hand-offs did not settle");
    }

    fn resume(&mut self) {
        for _ in 0..self.limits.slots {
            if !self.host.is_ready() {
                return;
            }
            host::resume(&mut self.host, &self.env, &mut self.out);
            while let Some(request) = self.out.pop() {
                self.route(request);
            }
            self.settle();
        }
        assert!(!self.host.is_ready());
    }

    #[expect(clippy::too_many_lines, reason = "the scripted peer routes every host request")]
    fn route(&mut self, request: Request) {
        match request {
            Request::RelayTyped { run, attempt, call, delivery, tool, writes, input, deadline } => {
                assert_eq!((run, attempt), (RUN, ATTEMPT));
                self.typed_relay = Some(RelayedCall { name: call, tool, writes, input, deadline });
                self.stats.relays += 1;
                self.events.push_back(Event::Relayed {
                    run,
                    attempt,
                    call: delivery,
                    answer: Box::from(&b"reply"[..]),
                });
            }
            Request::ReplyTyped { agent, call, reply } => {
                assert_eq!(agent, AGENT);
                assert_eq!(reply, Reply::Relayed { answer: Box::from(&b"reply"[..]) });
                self.typed_reply = Some(call);
                self.stats.replies += 1;
            }
            Request::DeliverTyped { agent, name, sender, words } => {
                assert_eq!(agent, AGENT);
                self.typed_message = Some(ReceivedMessage { name, sender, words });
            }
            Request::StartTyped { owner, workspace, charter, activation, turns, answered, grants } => {
                self.start_typed(owner, workspace, &charter, activation, turns, answered, &grants);
            }
            Request::Prepare { owner, .. } => {
                self.owner = Some(owner);
                self.space_live = true;
                self.events.push_back(Event::Prepared { owner, workspace: SPACE });
            }
            Request::StartV2 { owner, workspace, .. } => {
                assert_eq!(workspace, if self.space_live { Some(SPACE) } else { None });
                self.owner = Some(owner);
                self.agent_live = true;
                self.reading = true;
                self.events.push_back(Event::Started { owner, agent: AGENT });
            }
            Request::Turn { run, attempt, turn, .. } => {
                assert_eq!((run, attempt), (RUN, ATTEMPT));
                if self.connected {
                    self.receive_turn(turn);
                }
            }
            Request::TurnCredit { agent, read } => {
                assert_eq!(agent, AGENT);
                self.reading = read;
            }
            Request::DeliverV2 { owner, workspace, .. } => {
                assert!(self.agent_live && self.space_live && workspace == SPACE);
                self.stats.deliveries += 1;
                self.events.push_back(Event::Delivered {
                    owner,
                    delivery: Delivery { outcome: DeliveryOutcome::Delivered, left: Token::new(7), changed: true },
                });
            }
            Request::RelayV2 { run, attempt, delivery, .. } => {
                assert_eq!((run, attempt), (RUN, ATTEMPT));
                self.stats.relays += 1;
                self.events.push_back(Event::Relayed {
                    run,
                    attempt,
                    call: delivery,
                    answer: Box::from(&b"reply"[..]),
                });
            }
            Request::Reply { agent, reply, .. } => {
                assert_eq!(agent, AGENT);
                match reply {
                    Reply::Delivered(_)
                    | Reply::Relayed { .. }
                    | Reply::Unavailable
                    | Reply::Busy
                    | Reply::Withdrawn => {}
                }
                self.stats.replies += 1;
            }
            Request::Stop { agent } => {
                assert_eq!(agent, AGENT);
                self.stop_pending = true;
                if !self.stop_waits {
                    self.stop_pending = false;
                    self.agent_live = false;
                    self.events.push_back(Event::Gone { owner: self.owner.expect("assigned"), detail: Box::new([]) });
                }
            }
            Request::Save { owner, workspace } => {
                assert!(!self.agent_live && self.space_live && workspace == SPACE);
                self.stats.saves += 1;
                self.events.push_back(Event::Saved { owner, at: Some(Token::new(8)) });
            }
            Request::Release { workspace } => {
                assert!(!self.agent_live && workspace == SPACE && self.space_live);
                self.space_live = false;
                self.stats.releases += 1;
            }
            Request::AnswerV2 { to, run, attempt, answer } => {
                assert_eq!((to, run, attempt), (ReplyTo::new(RUN), RUN, ATTEMPT));
                assert!(!self.agent_live && !self.space_live);
                assert!(self.answer.is_none(), "one answer");
                if self.connected {
                    self.receive_answer(&answer);
                }
                self.answer = Some(answer);
            }
            Request::Hosting { runs } => {
                assert_eq!(runs.len(), usize::from(self.agent_live));
            }
            Request::Answer { .. }
            | Request::Relay { .. }
            | Request::CancelRelay { .. }
            | Request::Bounced { .. }
            | Request::Start { .. }
            | Request::Deliver { .. }
            | Request::Grant { .. }
            | Request::Abort { .. }
            | Request::DeliverWorkspace { .. } => panic!("unscripted request: {request:?}"),
        }
    }

    #[expect(clippy::too_many_arguments, reason = "the scripted peer receives the complete start request")]
    fn start_typed(
        &mut self,
        owner: Token,
        workspace: Option<Token>,
        charter: &[u8],
        activation: u64,
        turns: Box<[Box<[u8]>]>,
        answered: Box<[AnsweredCall]>,
        grants: &[host::Grant],
    ) {
        assert_eq!(workspace, None);
        assert_eq!(charter, b"charter");
        assert!(grants.is_empty());
        self.owner = Some(owner);
        self.agent_live = true;
        self.reading = true;
        self.typed_start = Some(StartedState { activation, turns, answered });
        self.events.push_back(Event::Started { owner, agent: AGENT });
    }

    fn receive_turn(&mut self, turn: Turn) {
        if let Some(previous) = self.seen.get(&turn.turn) {
            assert_eq!(previous, &turn, "replay preserves the whole turn");
        } else {
            assert_eq!(turn.turn, u32::try_from(self.seen.len()).expect("bounded") + 1, "no turn gap");
            self.seen.insert(turn.turn, turn);
        }
        self.stats.transmissions += 1;
    }

    fn receive_answer(&mut self, answer: &AnswerV2) {
        assert_eq!(usize::try_from(answer.turns).expect("bounded"), self.seen.len(), "turns precede their answer");
        match &answer.ending {
            EndingV2::Ended { .. } | EndingV2::Parked { .. } | EndingV2::Failed { .. } | EndingV2::Refused(_) => {}
        }
        let copy = format!("{answer:?}");
        if let Some(previous) = &self.answer_seen {
            assert_eq!(previous, &copy, "answer replay is identical");
        } else {
            self.answer_seen = Some(copy);
            self.stats.answers += 1;
        }
    }

    fn drain_facts(&mut self) {
        if self.connected {
            while self.host.pop_told().is_some() {
                self.stats.facts += 1;
            }
        }
        while self.host.pop_fact().is_some() {}
    }
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod referee_tests {
    use super::{Turn, World};

    fn turn(number: u32, body: &[u8]) -> Turn {
        Turn { turn: number, spent: 17, read: None, body: Box::from(body) }
    }

    #[test]
    #[should_panic(expected = "replay preserves the whole turn")]
    fn changed_replay_is_rejected() {
        let mut world = World::new();
        world.receive_turn(turn(1, b"original"));
        world.receive_turn(turn(1, b"changed"));
    }

    #[test]
    #[should_panic(expected = "no turn gap")]
    fn unseen_prefix_is_rejected() {
        World::new().receive_turn(turn(2, b"gap"));
    }
}
