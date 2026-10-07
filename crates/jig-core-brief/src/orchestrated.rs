//! Brief gathering across owners (domain/engine.md, section 9). The brief
//! keeps the core's typed text, but a connector's section is only its number,
//! kind, token and size. Its owner keeps the bytes through assignment.
//!
//! One deadline covers gathering and cuts. A required section that does not
//! arrive fails the brief; an optional one leaves a missing marker. Abandoning
//! a brief drops every connector token it was handed.

use alloc::boxed::Box;

use skein_lib::{Deadlines, Decimal, Env, List, Map, Queue, Time, Token, Writer};

use crate::limits::Limits;
use crate::planned::{self, ConnectorAction, Core, Planned};

/// Why a section is absent from a completed brief.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Missing {
    /// Its owner could not gather it.
    Failed,
    /// Gathering or cutting passed the brief's deadline.
    Late,
    /// It could not fit within the brief's byte budget.
    Budget,
}

/// A completed section. The root takes connector bytes from their owners.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Placed {
    /// Text retained and cut by the brief itself.
    Core { kind: Core, text: Box<[u8]> },
    /// Optional core text that was left out.
    CoreMissing { kind: Core, why: Missing },
    /// An opaque connector section, still held by that connector.
    Connector { connector: u16, kind: u16, token: Token, size: u32 },
    /// An optional section omitted, with its reason.
    Missing { connector: u16, kind: u16, why: Missing },
}

/// Inputs to the brief's gathering machine.
#[derive(PartialEq, Eq, Debug)]
pub enum Event {
    /// Start one brief, with the core's words and connector section tokens.
    Plan { brief: Token, budget: u32, deadline: Time, sections: Box<[Planned]> },
    /// A connector gathered a section and retains its bytes.
    Ready { brief: Token, section: Token, size: u32 },
    /// A connector could not gather a section.
    Missing { brief: Token, section: Token },
    /// A connector cut a section by its own rules and retains the result.
    Cut { brief: Token, section: Token, size: u32 },
    /// An amendment or cancellation withdrew this brief.
    Abandon { brief: Token },
}

/// Requests to the parent, which routes connector handoffs by number.
#[derive(PartialEq, Eq, Debug)]
pub enum Request {
    /// Gather one connector section under this read bound.
    Gather { connector: u16, section: Token, budget: u32 },
    /// Ask its owner to cut a retained section to this bound.
    CutTo { connector: u16, section: Token, size: u32 },
    /// Release a retained or outstanding connector section.
    Drop { connector: u16, section: Token },
    /// Take sections in order from their owners for the assignment.
    Complete { brief: Token, order: Box<[Placed]> },
    /// A required section could not be prepared.
    Failed { brief: Token, why: Missing },
    /// Admission refused an oversized or duplicate brief.
    Refused { brief: Token },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Status {
    Ready,
    Gathering,
    Cutting,
    Missing(Missing),
    Dropped(Missing),
}

#[derive(Debug)]
struct Active {
    sections: List<Planned>,
    status: List<Status>,
    allotted: List<u32>,
    budget: u32,
    waiting: u32,
}

/// Bounded active briefs and their single gathering deadline each.
#[derive(Debug)]
pub struct Domain {
    briefs: Map<Token, Active>,
    deadlines: Deadlines<Token>,
}

impl Domain {
    /// Allocate room for the configured number of concurrent briefs.
    #[must_use]
    pub fn new(limits: &Limits) -> Domain {
        Domain { briefs: Map::with_capacity(limits.briefs), deadlines: Deadlines::with_capacity(limits.briefs) }
    }

    /// The earliest gathering deadline.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Time> {
        self.deadlines.next()
    }

    /// Whether any brief is still gathering or being cut.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.briefs.is_empty()
    }
}

/// The most requests a step can emit: one per section plus a terminal.
#[must_use]
pub fn max_out(limits: &Limits) -> u32 {
    limits.sections.saturating_add(1)
}

/// The active inventory and its typed core text, including a whole input
/// inventory temporarily held during admission.
#[must_use]
pub fn worst_case(limits: &Limits) -> Option<u64> {
    if limits.briefs == 0 || limits.sections == 0 || limits.read_bytes == 0 || limits.brief_bytes == 0 {
        return None;
    }
    let rows = Map::<Token, Active>::worst_case(limits.briefs)?;
    let sections = List::<Planned>::worst_case(limits.sections)?;
    let status = List::<Status>::worst_case(limits.sections)?;
    let allotted = List::<u32>::worst_case(limits.sections)?;
    let payload = u64::from(limits.sections).checked_mul(u64::from(limits.brief_bytes))?;
    let one = sections.checked_add(status)?.checked_add(allotted)?.checked_add(payload)?;
    // Admission owns an input inventory while an active brief is live;
    // allocation copies the available inventory, and completion copies its
    // output while the original sections still exist.
    let in_hand = one.checked_mul(3)?;
    let planning = List::<bool>::worst_case(limits.sections)?
        .checked_add(List::<u32>::worst_case(limits.sections)?)?
        .checked_add(List::<planned::Placement>::worst_case(limits.sections)?)?
        .checked_add(List::<ConnectorAction>::worst_case(limits.sections)?)?;
    rows.checked_add(Deadlines::<Token>::worst_case(limits.briefs)?)?
        .checked_add(u64::from(limits.briefs).checked_mul(one)?)?
        .checked_add(in_hand)?
        .checked_add(planning)
}

/// Advance one brief or accept a new one.
pub fn step(domain: &mut Domain, env: &Env<Limits>, event: Event, out: &mut Queue<Request>) {
    match event {
        Event::Plan { brief, budget, deadline, sections } => begin(domain, env, brief, budget, deadline, sections, out),
        Event::Ready { brief, section, size } => update(domain, env, brief, section, Some(size), false, out),
        Event::Missing { brief, section } => update(domain, env, brief, section, None, false, out),
        Event::Cut { brief, section, size } => update(domain, env, brief, section, Some(size), true, out),
        Event::Abandon { brief } => {
            if let Some(active) = domain.briefs.remove(&brief) {
                domain.deadlines.cancel(brief);
                drop_tokens(&active, out);
            }
        }
    }
}

/// Expire the earliest due brief after the stage has taken today's inputs.
pub fn fire(domain: &mut Domain, env: &Env<Limits>, out: &mut Queue<Request>) {
    let Some(brief) = domain.deadlines.expire(env.now) else { return };
    let Some(mut active) = domain.briefs.remove(&brief) else { return };
    let mut required = false;
    let mut cutting = false;
    for index in 0..active.sections.len() {
        let status = active.status.get_mut(index).expect("section status");
        match status {
            Status::Gathering | Status::Cutting => {
                if *status == Status::Cutting {
                    cutting = true;
                }
                let section = active.sections.get(index).expect("section");
                if section.required() {
                    required = true;
                }
                *status = Status::Missing(Missing::Late);
            }
            Status::Ready | Status::Missing(_) | Status::Dropped(_) => {}
        }
    }
    active.waiting = 0;
    if required {
        fail(brief, &active, Missing::Late, out);
    } else if cutting {
        drop_missing(&mut active, out);
        complete(brief, active, out);
    } else {
        allocate(domain, brief, active, true, out);
    }
}

fn begin(
    domain: &mut Domain,
    env: &Env<Limits>,
    brief: Token,
    budget: u32,
    deadline: Time,
    sections: Box<[Planned]>,
    out: &mut Queue<Request>,
) {
    let Ok(count) = u32::try_from(sections.len()) else {
        out.push(Request::Refused { brief });
        return;
    };
    if domain.briefs.contains_key(&brief)
        || domain.briefs.len() == env.limits.briefs
        || count > env.limits.sections
        || budget > env.limits.brief_bytes
        || deadline <= env.now
        || !distinct(&sections)
    {
        out.push(Request::Refused { brief });
        return;
    }
    for section in &sections {
        match section {
            Planned::Core { text, .. } => {
                if text.len() > usize::try_from(env.limits.brief_bytes).expect("u32 fits usize") {
                    out.push(Request::Refused { brief });
                    return;
                }
            }
            Planned::Connector { .. } => {}
        }
    }
    let mut active = Active {
        sections: List::with_capacity(count),
        status: List::with_capacity(count),
        allotted: List::with_capacity(count),
        budget,
        waiting: 0,
    };
    for section in sections {
        let status = match &section {
            Planned::Core { .. } => Status::Ready,
            Planned::Connector { connector, token, .. } => {
                out.push(Request::Gather { connector: *connector, section: *token, budget: env.limits.read_bytes });
                active.waiting = active.waiting.checked_add(1).expect("bounded sections");
                Status::Gathering
            }
        };
        active.sections.push(section).expect("measured inventory");
        active.status.push(status).expect("measured inventory");
        active.allotted.push(0).expect("measured inventory");
    }
    if active.waiting == 0 {
        allocate(domain, brief, active, false, out);
    } else {
        domain.deadlines.arm(brief, deadline).expect("admitted brief deadline");
        domain.briefs.insert(brief, active).expect("admitted brief room");
    }
}

fn distinct(sections: &[Planned]) -> bool {
    for (index, section) in sections.iter().enumerate() {
        let token = match section {
            Planned::Connector { token, .. } => *token,
            Planned::Core { .. } => continue,
        };
        for previous in sections.get(..index).expect("prefix of inventory") {
            match previous {
                Planned::Connector { token: earlier, .. } if *earlier == token => return false,
                Planned::Connector { .. } | Planned::Core { .. } => {}
            }
        }
    }
    true
}

fn update(
    domain: &mut Domain,
    env: &Env<Limits>,
    brief: Token,
    section: Token,
    size: Option<u32>,
    cut: bool,
    out: &mut Queue<Request>,
) {
    let Some(mut active) = domain.briefs.remove(&brief) else { return };
    let mut found = false;
    let mut was_cutting = false;
    for index in 0..active.sections.len() {
        let row = active.sections.get(index).expect("section");
        let matches = match row {
            Planned::Connector { token, .. } => *token == section,
            Planned::Core { .. } => false,
        };
        if !matches {
            continue;
        }
        let status = active.status.get_mut(index).expect("section status");
        let expected = match *status {
            Status::Gathering => !cut,
            Status::Cutting => cut,
            Status::Ready | Status::Missing(_) | Status::Dropped(_) => false,
        };
        if !expected {
            break;
        }
        found = true;
        was_cutting = cut;
        active.waiting = active.waiting.checked_sub(1).expect("an outstanding section");
        match size {
            Some(actual)
                if actual <= env.limits.read_bytes
                    && (!cut || actual <= *active.allotted.get(index).expect("cut allotment")) =>
            {
                let row = active.sections.get_mut(index).expect("section");
                match row {
                    Planned::Connector { size: known, .. } => *known = actual,
                    Planned::Core { .. } => unreachable!("matched connector token"),
                }
                *status = Status::Ready;
            }
            Some(_) | None => {
                let required = row.required();
                if required {
                    *status = Status::Missing(Missing::Failed);
                    domain.deadlines.cancel(brief);
                    fail(brief, &active, Missing::Failed, out);
                    return;
                }
                *status = Status::Dropped(Missing::Failed);
                drop_one(row, out);
            }
        }
        break;
    }
    if !found {
        domain.briefs.insert(brief, active).expect("same active brief");
        return;
    }
    if active.waiting == 0 {
        if was_cutting {
            domain.deadlines.cancel(brief);
            complete(brief, active, out);
        } else {
            allocate(domain, brief, active, false, out);
        }
    } else {
        domain.briefs.insert(brief, active).expect("same active brief");
    }
}

fn allocate(domain: &mut Domain, brief: Token, mut active: Active, expired: bool, out: &mut Queue<Request>) {
    drop_missing(&mut active, out);
    let count = active.sections.len();
    let mut available = List::with_capacity(count);
    let mut positions = List::with_capacity(count);
    for (index, section) in active.sections.iter().enumerate() {
        let status = active.status.get(u32::try_from(index).expect("bounded index")).expect("section status");
        match status {
            Status::Ready => {
                available.push(section.clone()).expect("measured inventory");
                positions.push(u32::try_from(index).expect("bounded index")).expect("measured inventory");
            }
            Status::Missing(_) | Status::Dropped(_) => {}
            Status::Gathering | Status::Cutting => unreachable!("allocation follows handoffs"),
        }
    }
    let Some(plan) = planned::plan(available.as_slice(), active.budget) else {
        domain.deadlines.cancel(brief);
        fail(brief, &active, Missing::Budget, out);
        return;
    };
    for placement in &plan.order {
        let index = *positions.get(placement.index).expect("planned section");
        *active.allotted.get_mut(index).expect("planned section") = placement.size;
    }
    for index in 0..active.sections.len() {
        let section = active.sections.get(index).expect("section");
        let allotted = *active.allotted.get(index).expect("allotment");
        match section {
            Planned::Core { text, required, .. }
                if allotted < u32::try_from(text.len()).expect("admitted core text") =>
            {
                if *required && !can_cut(text, allotted) {
                    domain.deadlines.cancel(brief);
                    fail(brief, &active, Missing::Budget, out);
                    return;
                }
            }
            Planned::Core { .. } | Planned::Connector { .. } => {}
        }
    }
    if expired {
        for action in &plan.connectors {
            match action {
                ConnectorAction::CutTo { token, .. } => {
                    let index = locate(&active, *token).expect("planned connector token");
                    if active.sections.get(index).expect("section").required() {
                        domain.deadlines.cancel(brief);
                        fail(brief, &active, Missing::Late, out);
                        return;
                    }
                }
                ConnectorAction::Drop { .. } => {}
            }
        }
    }
    for action in plan.connectors {
        match action {
            ConnectorAction::CutTo { connector, token, size } => {
                let index = locate(&active, token).expect("planned connector token");
                if expired || size == 0 {
                    if active.sections.get(index).expect("section").required() {
                        domain.deadlines.cancel(brief);
                        fail(brief, &active, Missing::Budget, out);
                        return;
                    }
                    *active.status.get_mut(index).expect("section status") = Status::Dropped(Missing::Late);
                    out.push(Request::Drop { connector, section: token });
                } else {
                    *active.status.get_mut(index).expect("section status") = Status::Cutting;
                    active.waiting = active.waiting.checked_add(1).expect("bounded sections");
                    out.push(Request::CutTo { connector, section: token, size });
                }
            }
            ConnectorAction::Drop { connector, token } => {
                let index = locate(&active, token).expect("planned connector token");
                *active.status.get_mut(index).expect("section status") = Status::Dropped(Missing::Budget);
                out.push(Request::Drop { connector, section: token });
            }
        }
    }
    if active.waiting == 0 {
        domain.deadlines.cancel(brief);
        complete(brief, active, out);
    } else {
        domain.briefs.insert(brief, active).expect("same active brief");
    }
}

fn locate(active: &Active, token: Token) -> Option<u32> {
    for (index, section) in active.sections.iter().enumerate() {
        match section {
            Planned::Connector { token: found, .. } if *found == token => return u32::try_from(index).ok(),
            Planned::Connector { .. } | Planned::Core { .. } => {}
        }
    }
    None
}

fn drop_tokens(active: &Active, out: &mut Queue<Request>) {
    for (index, section) in active.sections.iter().enumerate() {
        let status = active.status.get(u32::try_from(index).expect("bounded index")).expect("section status");
        match status {
            Status::Dropped(_) => {}
            Status::Ready | Status::Gathering | Status::Cutting | Status::Missing(_) => drop_one(section, out),
        }
    }
}

fn drop_one(section: &Planned, out: &mut Queue<Request>) {
    match section {
        Planned::Connector { connector, token, .. } => {
            out.push(Request::Drop { connector: *connector, section: *token });
        }
        Planned::Core { .. } => {}
    }
}

fn drop_missing(active: &mut Active, out: &mut Queue<Request>) {
    for index in 0..active.sections.len() {
        let status = active.status.get_mut(index).expect("section status");
        match status {
            Status::Missing(why) => {
                let reason = *why;
                drop_one(active.sections.get(index).expect("section"), out);
                *status = Status::Dropped(reason);
            }
            Status::Ready | Status::Gathering | Status::Cutting | Status::Dropped(_) => {}
        }
    }
}

fn fail(brief: Token, active: &Active, why: Missing, out: &mut Queue<Request>) {
    drop_tokens(active, out);
    out.push(Request::Failed { brief, why });
}

fn complete(brief: Token, active: Active, out: &mut Queue<Request>) {
    let mut order = List::with_capacity(active.sections.len());
    for (index, section) in active.sections.into_iter().enumerate() {
        let index = u32::try_from(index).expect("bounded section count");
        let status = *active.status.get(index).expect("section status");
        let allotted = *active.allotted.get(index).expect("section allotment");
        let placed = match section.clone() {
            Planned::Core { kind, text, required, .. } => {
                if (allotted == 0 && !required && !text.is_empty()) || !can_cut(&text, allotted) {
                    Placed::CoreMissing { kind, why: Missing::Budget }
                } else {
                    Placed::Core { kind, text: cut_core(kind, text, allotted) }
                }
            }
            Planned::Connector { connector, kind, token, size, .. } => match status {
                Status::Ready => Placed::Connector { connector, kind, token, size },
                Status::Missing(why) | Status::Dropped(why) => Placed::Missing { connector, kind, why },
                Status::Gathering | Status::Cutting => unreachable!("a brief completes after its handoffs"),
            },
        };
        order.push(placed).expect("one placement per section");
    }
    out.push(Request::Complete { brief, order: order.into_boxed() });
}

fn can_cut(text: &[u8], size: u32) -> bool {
    if text.len() <= usize::try_from(size).expect("u32 fits usize") {
        return true;
    }
    let lost = u64::try_from(text.len()).expect("bounded text");
    let marker = b"[".len().saturating_add(Decimal::of(lost).as_bytes().len()).saturating_add(b" bytes cut]\n".len());
    marker <= usize::try_from(size).expect("u32 fits usize")
}

fn cut_core(kind: Core, text: Box<[u8]>, size: u32) -> Box<[u8]> {
    let room = usize::try_from(size).expect("u32 fits usize");
    if text.len() <= room {
        return text;
    }
    let tail = match kind {
        Core::Attempts | Core::TranscriptTail => true,
        Core::Task
        | Core::Lineage
        | Core::Inbox
        | Core::Results
        | Core::Plan
        | Core::Calls
        | Core::Waiting
        | Core::NotesIndex => false,
    };
    let mut low = 0_usize;
    let mut high = text.len().min(room);
    for _ in 0..usize::BITS {
        if low >= high {
            break;
        }
        let mid = low.saturating_add(high.saturating_sub(low).div_ceil(2));
        let keep = edge(&text, mid, tail);
        let lost = text.len().saturating_sub(keep.len());
        let marker = usize::from(!tail && !keep.is_empty())
            .saturating_add(b"[".len())
            .saturating_add(Decimal::of(u64::try_from(lost).expect("bounded text")).as_bytes().len())
            .saturating_add(b" bytes cut]\n".len());
        if keep.len().saturating_add(marker) <= room {
            low = mid;
        } else {
            high = mid.saturating_sub(1);
        }
    }
    let keep = edge(&text, low, tail);
    let lost = text.len().saturating_sub(keep.len());
    let digits = Decimal::of(u64::try_from(lost).expect("bounded text"));
    let marker = usize::from(!tail && !keep.is_empty())
        .saturating_add(b"[".len())
        .saturating_add(digits.as_bytes().len())
        .saturating_add(b" bytes cut]\n".len());
    let mut writer = Writer::new(keep.len().saturating_add(marker));
    if tail {
        writer.put(b"[").expect("measured marker");
        writer.put(digits.as_bytes()).expect("measured count");
        writer.put(b" bytes cut]\n").expect("measured marker");
        writer.put(keep).expect("measured retained text");
    } else {
        writer.put(keep).expect("measured retained text");
        if !keep.is_empty() {
            writer.put(b"\n").expect("measured break");
        }
        writer.put(b"[").expect("measured marker");
        writer.put(digits.as_bytes()).expect("measured count");
        writer.put(b" bytes cut]\n").expect("measured marker");
    }
    writer.finish()
}

fn edge(text: &[u8], wanted: usize, tail: bool) -> &[u8] {
    if tail {
        let mut start = text.len().saturating_sub(wanted);
        for _ in 0..3 {
            if continues(text.get(start)) {
                start = start.saturating_add(1);
            }
        }
        text.get(start..).expect("within text")
    } else {
        let mut end = wanted;
        for _ in 0..3 {
            if continues(text.get(end)) {
                end = end.saturating_sub(1);
            }
        }
        text.get(..end).expect("within text")
    }
}

fn continues(byte: Option<&u8>) -> bool {
    match byte {
        Some(byte) => byte & 0b1100_0000 == 0b1000_0000,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skein_lib::{Duration, Wall};

    fn env() -> Env<Limits> {
        let limits = Limits { briefs: 2, sections: 3, read_bytes: 256, brief_bytes: 400 };
        Env { now: Time::ZERO, wall: Wall::EPOCH, limits }
    }

    fn outputs(domain: &mut Domain, event: Event) -> Box<[Request]> {
        let env = env();
        let mut out = Queue::with_capacity(max_out(&env.limits));
        step(domain, &env, event, &mut out);
        let mut list = List::with_capacity(out.len());
        for _ in 0..out.len() {
            list.push(out.pop().expect("counted output")).expect("bounded output");
        }
        list.into_boxed()
    }

    #[test]
    fn a_connector_section_is_only_a_token_and_size_until_completion() {
        let env = env();
        let mut domain = Domain::new(&env.limits);
        let brief = Token::new(4);
        let section = Token::new(9);
        assert_eq!(
            outputs(
                &mut domain,
                Event::Plan {
                    brief,
                    budget: 20,
                    deadline: Time::ZERO.saturating_add(Duration::from_secs(1)),
                    sections: Box::new([
                        Planned::Core {
                            kind: Core::Task,
                            text: Box::from(&b"task"[..]),
                            limit: 20,
                            priority: 0,
                            required: true
                        },
                        Planned::Connector {
                            connector: 2,
                            kind: 7,
                            token: section,
                            size: 0,
                            limit: 20,
                            priority: 1,
                            required: false
                        },
                    ]),
                }
            )
            .as_ref(),
            [Request::Gather { connector: 2, section, budget: env.limits.read_bytes }]
        );
        assert_eq!(
            outputs(&mut domain, Event::Ready { brief, section, size: 5 }).as_ref(),
            [Request::Complete {
                brief,
                order: Box::new([
                    Placed::Core { kind: Core::Task, text: Box::from(&b"task"[..]) },
                    Placed::Connector { connector: 2, kind: 7, token: section, size: 5 },
                ]),
            }]
        );
    }

    #[test]
    fn a_required_missing_section_fails_and_drops_every_token_once() {
        let limits = env().limits;
        let mut domain = Domain::new(&limits);
        let brief = Token::new(7);
        let first = Token::new(1);
        let second = Token::new(2);
        let sections = Box::new([
            Planned::Connector {
                connector: 3,
                kind: 1,
                token: first,
                size: 0,
                limit: 100,
                priority: 0,
                required: true,
            },
            Planned::Connector {
                connector: 3,
                kind: 2,
                token: second,
                size: 0,
                limit: 100,
                priority: 1,
                required: false,
            },
        ]);
        assert_eq!(
            outputs(
                &mut domain,
                Event::Plan {
                    brief,
                    budget: 100,
                    deadline: Time::ZERO.saturating_add(Duration::from_secs(1)),
                    sections
                }
            )
            .as_ref(),
            [
                Request::Gather { connector: 3, section: first, budget: limits.read_bytes },
                Request::Gather { connector: 3, section: second, budget: limits.read_bytes },
            ]
        );
        assert_eq!(
            outputs(&mut domain, Event::Missing { brief, section: first }).as_ref(),
            [
                Request::Drop { connector: 3, section: first },
                Request::Drop { connector: 3, section: second },
                Request::Failed { brief, why: Missing::Failed },
            ]
        );
        assert!(domain.is_idle());
    }

    #[test]
    fn an_optional_late_section_is_dropped_and_marked_missing() {
        let mut env = env();
        let mut domain = Domain::new(&env.limits);
        let brief = Token::new(7);
        let section = Token::new(8);
        let deadline = Time::ZERO.saturating_add(Duration::from_secs(1));
        outputs(
            &mut domain,
            Event::Plan {
                brief,
                budget: 100,
                deadline,
                sections: Box::new([Planned::Connector {
                    connector: 4,
                    kind: 5,
                    token: section,
                    size: 0,
                    limit: 100,
                    priority: 0,
                    required: false,
                }]),
            },
        );
        env.now = deadline;
        let mut out = Queue::with_capacity(max_out(&env.limits));
        fire(&mut domain, &env, &mut out);
        assert_eq!(out.pop(), Some(Request::Drop { connector: 4, section }));
        assert_eq!(
            out.pop(),
            Some(Request::Complete {
                brief,
                order: Box::new([Placed::Missing { connector: 4, kind: 5, why: Missing::Late }]),
            })
        );
        assert_eq!(out.pop(), None);
    }

    #[test]
    fn a_connector_cut_keeps_its_token_with_the_owner() {
        let mut domain = Domain::new(&env().limits);
        let brief = Token::new(7);
        let section = Token::new(8);
        outputs(
            &mut domain,
            Event::Plan {
                brief,
                budget: 40,
                deadline: Time::ZERO.saturating_add(Duration::from_secs(1)),
                sections: Box::new([Planned::Connector {
                    connector: 4,
                    kind: 5,
                    token: section,
                    size: 0,
                    limit: 40,
                    priority: 0,
                    required: true,
                }]),
            },
        );
        assert_eq!(
            outputs(&mut domain, Event::Ready { brief, section, size: 50 }).as_ref(),
            [Request::CutTo { connector: 4, section, size: 40 }]
        );
        assert_eq!(
            outputs(&mut domain, Event::Cut { brief, section, size: 38 }).as_ref(),
            [Request::Complete {
                brief,
                order: Box::new([Placed::Connector { connector: 4, kind: 5, token: section, size: 38 }]),
            }]
        );
    }

    #[test]
    fn abandoning_gathering_drops_every_connector_token() {
        let mut domain = Domain::new(&env().limits);
        let brief = Token::new(7);
        let section = Token::new(8);
        outputs(
            &mut domain,
            Event::Plan {
                brief,
                budget: 40,
                deadline: Time::ZERO.saturating_add(Duration::from_secs(1)),
                sections: Box::new([Planned::Connector {
                    connector: 4,
                    kind: 5,
                    token: section,
                    size: 0,
                    limit: 40,
                    priority: 0,
                    required: true,
                }]),
            },
        );
        assert_eq!(outputs(&mut domain, Event::Abandon { brief }).as_ref(), [Request::Drop { connector: 4, section }]);
        assert!(domain.is_idle());
    }

    #[test]
    fn a_core_cut_says_how_many_utf8_bytes_it_lost() {
        let text = Box::from("aébcdefghijklmnopqrstuvwxyz".as_bytes());
        let cut = cut_core(Core::Task, text, 18);
        assert!(cut.len() <= 18);
        assert_eq!(cut.as_ref(), b"a\n[27 bytes cut]\n");
    }

    #[test]
    fn a_transcript_cut_keeps_its_end_after_the_cut_marker() {
        let text = Box::from(&b"abcdefghijklmnopqrstuvwxyyz"[..]);
        let cut = cut_core(Core::TranscriptTail, text, 18);
        assert!(cut.len() <= 18);
        assert!(cut.starts_with(b"["));
        assert!(cut.ends_with(b"yz"));
    }
}
