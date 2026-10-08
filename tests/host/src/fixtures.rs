//! Host records used by the lifecycle scripts. Callback tokens are encoded
//! as opaque names; names are opaque bytes.
use jig_host::{Ask, Assignment, Event, Finish, RunAssignment};
use skein_lib::{Duration, Reader, ReplyTo, Token};
#[must_use]
pub fn assign(reply_to: ReplyTo, assignment: RunAssignment) -> Event {
    Event::Assign { reply_to, assignment: Assignment { assignment, turns: Box::new([]), answered: Box::new([]) } }
}
#[must_use]
pub fn inbound(run: Token, attempt: Token, name: Token, words: Box<[u8]>) -> Event {
    Event::Inbound { run, attempt, name, sender: Box::new([]), words }
}
#[must_use]
pub fn called(owner: Token, call: Token, ask: Ask) -> Event {
    Event::Called { owner, call: Box::from(call.raw().to_be_bytes()), ask }
}
#[must_use]
pub fn withdrawn(owner: Token, call: Token) -> Event {
    Event::Withdrawn { owner, call: Box::from(call.raw().to_be_bytes()) }
}
#[must_use]
pub fn finished(owner: Token, finish: Finish) -> Event {
    Event::Finished { owner, turns: 0, spent: 0, finish }
}
#[must_use]
pub fn deliver(title: Box<[u8]>) -> Ask {
    Ask::Deliver { title, body: Box::new([]) }
}
#[must_use]
pub fn relay(input: Box<[u8]>) -> Ask {
    Ask::Relay { tool: Box::new([]), writes: false, input, deadline: Duration::from_secs(1) }
}
#[must_use]
pub fn callback(name: &[u8]) -> Token {
    Token::new(Reader::new(name).u64().expect("encoded callback"))
}
