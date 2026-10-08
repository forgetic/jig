//! Typed host records used by the lifecycle scripts. Callback tokens are encoded
//! as opaque names; the conversation replaces the scripts' former snapshots.
use jig_host::{Answer, AnswerV2, Ask, Assignment, AssignmentTyped, EndingV2, Event, Finish, FinishV2};
use skein_lib::{Duration, Reader, ReplyTo, Token};
#[must_use]
pub fn assign(reply_to: ReplyTo, mut assignment: Assignment) -> Event {
    let turns = match assignment.snapshot.take() {
        Some(body) => vec![body].into_boxed_slice(),
        None => Box::new([]),
    };
    Event::AssignTyped { reply_to, assignment: AssignmentTyped { assignment, turns, answered: Box::new([]) } }
}
#[must_use]
pub fn inbound(run: Token, attempt: Token, name: Token, words: Box<[u8]>) -> Event {
    Event::InboundTyped { run, attempt, name, sender: Box::new([]), words }
}
#[must_use]
pub fn called(owner: Token, call: Token, ask: Ask) -> Event {
    Event::CalledTyped { owner, call: Box::from(call.raw().to_be_bytes()), ask }
}
#[must_use]
pub fn withdrawn(owner: Token, call: Token) -> Event {
    Event::WithdrawnTyped { owner, call: Box::from(call.raw().to_be_bytes()) }
}
#[must_use]
pub fn finished(owner: Token, finish: Finish) -> Event {
    let finish = match finish {
        Finish::Ended { outcome } => FinishV2::Ended { outcome },
        Finish::Parked { .. } => FinishV2::Parked,
        Finish::Failed { failure } => FinishV2::Failed { failure },
    };
    Event::FinishedV2 { owner, turns: 0, spent: 0, finish }
}
#[must_use]
pub fn deliver(title: Box<[u8]>) -> Ask {
    Ask::DeliverV2 { title, body: Box::new([]) }
}
#[must_use]
pub fn relay(input: Box<[u8]>) -> Ask {
    Ask::RelayTyped { tool: Box::new([]), writes: false, input, deadline: Duration::from_secs(1) }
}
#[must_use]
pub fn callback(name: &[u8]) -> Token {
    Token::new(Reader::new(name).u64().expect("encoded callback"))
}
#[must_use]
pub fn plain(answer: &AnswerV2) -> Answer {
    match &answer.ending {
        EndingV2::Refused(refusal) => Answer::Refused(*refusal),
        EndingV2::Ended { outcome, work } => {
            Answer::Ended { outcome: outcome.clone(), work: jig_host::Work { left: work.left, saved: work.saved } }
        }
        EndingV2::Parked { work } => {
            Answer::Parked { snapshot: None, work: jig_host::Work { left: work.left, saved: work.saved } }
        }
        EndingV2::Failed { failure, detail, work } => Answer::Failed {
            failure: *failure,
            detail: detail.clone(),
            work: jig_host::Work { left: work.left, saved: work.saved },
        },
    }
}
