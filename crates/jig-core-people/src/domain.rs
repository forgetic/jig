//! Bounded people state, request admission and restoration (domain/people.md, sections 3–5). `step` handles root events; `fire` expires sign-ins.
//! The child keeps identities, roles, sign-ins and keyed outcomes, routes each
//! admitted flight once and waits for `Decided`. Root owns policy, task creation
//! and durability; this child never performs IO or retains credential secrets.

use crate::{
    Ask, Entry, Event, Fact, Holding, Identity, IdentityKey, InitialOwner, Key, Kind, Limits, Outcome, Refusal, Reply,
    Request, RequestKey, ResultRef, Role, Seed, Stored, Whom,
};
use alloc::boxed::Box;
use skein_lib::{Deadlines, Env, Id, List, Map, Queue, ReplyTo, Slab, Time, Token, Wall};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct SignIn {
    person: u64,
    expires: Wall,
    due: Time,
}

#[derive(PartialEq, Eq, Hash, Debug)]
pub(crate) struct Answered {
    ask: Ask,
    outcome: Outcome,
    at: Wall,
    due: Time,
}

#[derive(Debug)]
pub(crate) struct Pending {
    key: RequestKey,
    ask: Ask,
    replies: List<ReplyTo>,
}

#[derive(PartialEq, Eq, Hash, Debug)]
pub(crate) struct OpenWatch {
    ask: Ask,
    watcher: Token,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Phase {
    Restoring,
    Ready,
    Failed,
}

/// Bounded secret-free identities, sign-ins/deadlines, project holdings, completed keys and
/// volatile flights; knows tasks only by returned number and starts in restoring phase.
/// (domain/people.md, sections 2–5).
#[derive(Debug)]
pub struct Domain {
    phase: Phase,
    deployment_provider: u16,
    people: Map<u64, Identity>,
    read_positions: Map<u64, u64>,
    unread: Map<u64, Box<[ResultRef]>>,
    waiting: Map<Whom, Box<[Entry]>>,
    identities: Map<IdentityKey, u64>,
    sign_ins: Map<u64, SignIn>,
    alarms: Deadlines<u64>,
    roles: Map<u32, Box<[Holding]>>,
    owners: Box<[InitialOwner]>,
    answers: Map<RequestKey, Answered>,
    open_watches: Map<RequestKey, OpenWatch>,
    answer_alarms: Deadlines<RequestKey>,
    pending: Slab<Pending>,
    flights: Map<RequestKey, Id<Pending>>,
    facts: Queue<Fact>,
    lost: u64,
}

impl Domain {
    /// Create restoring state with capacities from validated `limits` and at most
    /// `limits.initial_owners` owned bootstrap entries. Panics on unrepresentable/invalid limits,
    /// oversized owners or duplicate project/identity pairs; project roles must be initialized
    /// before sign-in.
    #[must_use]
    pub fn new(limits: &Limits, owners: Box<[InitialOwner]>, deployment_provider: u16) -> Domain {
        assert!(crate::worst_case(limits).is_some(), "people limits are valid");
        assert!(
            owners.len() <= usize::try_from(limits.initial_owners).expect("u32 fits usize"),
            "configured owners fit"
        );
        for (at, owner) in owners.iter().enumerate() {
            assert!(
                !owner.identity.subject.is_empty()
                    && owner.identity.subject.len() <= usize::try_from(limits.identity_bytes).expect("u32 fits usize")
                    && owner.identity.provider != deployment_provider,
                "configured first owners are bounded people"
            );
            for earlier in owners.iter().take(at) {
                assert!(
                    owner.project != earlier.project || owner.identity != earlier.identity,
                    "one initial owner identity per project"
                );
            }
        }
        Domain {
            phase: Phase::Restoring,
            deployment_provider,
            people: Map::with_capacity(limits.people),
            read_positions: Map::with_capacity(limits.people),
            unread: Map::with_capacity(limits.people),
            waiting: Map::with_capacity(
                limits
                    .people
                    .checked_add(limits.projects.checked_mul(limits.holdings).expect("role cache room"))
                    .expect("inbox cache room"),
            ),
            identities: Map::with_capacity(limits.people),
            sign_ins: Map::with_capacity(limits.sign_ins),
            alarms: Deadlines::with_capacity(limits.sign_ins),
            roles: Map::with_capacity(limits.projects),
            owners,
            answers: Map::with_capacity(limits.requests),
            open_watches: Map::with_capacity(limits.requests),
            answer_alarms: Deadlines::with_capacity(limits.requests),
            pending: Slab::with_capacity(limits.pending),
            flights: Map::with_capacity(limits.pending),
            facts: Queue::with_capacity(limits.facts),
            lost: 0,
        }
    }

    /// Pure scheduler query of the earliest armed monotonic sign-in expiry, or `None`; no
    /// expiration or output occurs here.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Time> {
        match (self.alarms.next(), self.answer_alarms.next()) {
            (Some(left), Some(right)) => Some(left.min(right)),
            (Some(left), None) => Some(left),
            (None, Some(right)) => Some(right),
            (None, None) => None,
        }
    }

    /// Pure scheduler query returning whether any armed expiry is at or before supplied `now`; no
    /// state change or output.
    #[must_use]
    pub fn is_due(&self, now: Time) -> bool {
        match self.next_deadline() {
            Some(at) => at <= now,
            None => false,
        }
    }

    /// Reclaim retired flight slots at the parent's iteration reclaim point after their outputs
    /// have been routed; no request or client reply is emitted.
    pub fn reclaim(&mut self) {
        self.pending.reclaim();
    }

    /// Remove one optional content-free observation from the bounded fact queue; keeping/dropping
    /// facts changes no decision or persistence output.
    pub fn pop_fact(&mut self) -> Option<Fact> {
        self.facts.pop()
    }

    /// Return the saturating count of observations dropped when `Limits::facts` was full;
    /// diagnostic only, not a lifecycle acknowledgement.
    #[must_use]
    pub const fn facts_lost(&self) -> u64 {
        self.lost
    }

    /// Pure bounded membership lookup used by root escalation routing and authenticated named
    /// reads; caller separately authenticates identities. Available during restore,
    /// allocating/emitting/mutating nothing.
    #[must_use]
    pub fn role(&self, person: u64, project: u32) -> Option<Role> {
        role(self, person, project)
    }

    /// Resolve a provider-authenticated collaborator to this project's role.
    #[must_use]
    pub fn role_for_identity(&self, key: IdentityKey, project: u32) -> Option<(u64, Role)> {
        let person = *self.identities.get(&key)?;
        Some((person, role(self, person, project)?))
    }

    /// Check that one whole adoption seed can commit without displacing current roles.
    #[must_use]
    pub fn can_seed(&self, limits: &Limits, project: u32, collaborators: &[Seed]) -> bool {
        if !self.ready() || collaborators.len() > usize::try_from(limits.holdings).expect("u32 fits usize") {
            return false;
        }
        let Some(holdings) = self.roles.get(&project) else { return false };
        let mut new_people = 0_usize;
        let mut new_roles = 0_usize;
        for (at, seed) in collaborators.iter().enumerate() {
            if seed.identity.subject.is_empty()
                || seed.identity.subject.len() > usize::try_from(limits.identity_bytes).expect("u32 fits usize")
                || seed.identity.provider == self.deployment_provider
                || seed.candidate == 0
                || self.people.contains_key(&seed.candidate)
            {
                return false;
            }
            for earlier in collaborators.iter().take(at) {
                if earlier.identity == seed.identity || earlier.candidate == seed.candidate {
                    return false;
                }
            }
            match self.identities.get(&seed.identity) {
                Some(person) => {
                    if !has_person(holdings, *person) {
                        new_roles = new_roles.saturating_add(1);
                    }
                }
                None => {
                    new_people = new_people.saturating_add(1);
                    new_roles = new_roles.saturating_add(1);
                }
            }
        }
        match (
            self.people.len().checked_add(u32::try_from(new_people).ok().unwrap_or(u32::MAX)),
            holdings.len().checked_add(new_roles),
        ) {
            (Some(people), Some(roles)) => {
                people <= limits.people && roles <= usize::try_from(limits.holdings).expect("u32 fits usize")
            }
            (None, _) | (_, None) => false,
        }
    }

    /// Pure lookup of whether the bounded role table contains `project`, including an empty
    /// holdings set. Available during restoration so the root preserves restored roles and seeds
    /// only missing bootstrap projects; it implies neither readiness nor person membership. The
    /// table is bounded by `Limits::projects`; this query allocates nothing, emits no output and
    /// grants no role.
    #[must_use]
    pub fn has_project(&self, project: u32) -> bool {
        self.roles.contains_key(&project)
    }

    /// Pure authenticated lookup for the root-issued `sign_in` at supplied `now` and `wall`.
    /// Returns `None` before readiness, for an absent sign-in, or when either its projected
    /// monotonic deadline is at or before `now` or its saved wall expiry is at or before `wall`. A
    /// backward wall correction cannot revive a monotonic-expired session, even before fire.
    /// Otherwise returns the stable requester person number. The sign-in table is bounded by
    /// `Limits::sign_ins`; this read allocates nothing, emits no output and changes no sign-in or
    /// role state. Root uses it for named task result reads.
    #[must_use]
    pub fn person(&self, sign_in: u64, now: Time, wall: Wall) -> Option<u64> {
        if !self.ready() {
            return None;
        }
        let session = self.sign_ins.get(&sign_in)?;
        if session.expires <= wall || session.due <= now {
            return None;
        }
        Some(session.person)
    }

    /// Last committed result read position for an existing person, or zero before any read.
    #[must_use]
    pub fn read_position(&self, person: u64) -> Option<u64> {
        if !self.ready() || !self.people.contains_key(&person) {
            return None;
        }
        Some(*self.read_positions.get(&person).unwrap_or(&0))
    }

    /// Currently cached unread result references, newest first; missing older entries are loaded from ended tasks.
    #[must_use]
    pub fn cached_results(&self, person: u64) -> Option<&[ResultRef]> {
        if !self.ready() || !self.people.contains_key(&person) {
            return None;
        }
        match self.unread.get(&person) {
            Some(entries) => Some(entries.as_ref()),
            None => Some(&[]),
        }
    }

    /// Bounded newest-first task-derived references for a current person and exact project roles.
    /// Older references are obtained by the root from stored task rows.
    #[must_use]
    pub fn cached_waiting(&self, limits: &Limits, person: u64) -> Option<Box<[Entry]>> {
        if !self.ready() || !self.people.contains_key(&person) {
            return None;
        }
        let mut visible = List::with_capacity(limits.inbox_entries);
        if let Some(entries) = self.waiting.get(&Whom::Person(person)) {
            for &entry in entries {
                insert_newest(&mut visible, entry);
            }
        }
        for (project, holdings) in &self.roles {
            for holding in holdings {
                if holding.person == person
                    && let Some(entries) =
                        self.waiting.get(&Whom::Role { project: *project, role: role_number(holding.role) })
                {
                    for &entry in entries {
                        insert_newest(&mut visible, entry);
                    }
                }
            }
        }
        Some(visible.into_boxed())
    }

    /// Advance one authenticated person's position inside the root decision that replies.
    /// The returned people row is saved with the reply's commit.
    pub fn advance_read_position(&mut self, person: u64, position: u64) -> Option<Stored> {
        let old = self.read_position(person)?;
        if position <= old {
            return None;
        }
        let saved = self.read_positions.insert(person, position);
        assert!(saved.is_ok(), "one position per admitted person");
        if let Some(cached) = self.unread.get(&person) {
            let mut retained = List::with_capacity(u32::try_from(cached.len()).expect("cached result bound"));
            for &entry in cached {
                if entry.position > position {
                    retained.push(entry).expect("subset of cached entries");
                }
            }
            let saved = self.unread.insert(person, retained.into_boxed());
            assert!(saved.is_ok(), "cached person already admitted");
        }
        Some(Stored::ReadPosition { person, position })
    }

    /// Cache up to the configured per-person entry bound; the store pages the rest.
    pub fn remember_result(&mut self, limits: &Limits, person: u64, entry: ResultRef) {
        if !self.ready()
            || !self.people.contains_key(&person)
            || entry.position <= self.read_position(person).unwrap_or(0)
        {
            return;
        }
        let cap = limits.inbox_entries;
        if cap == 0 {
            return;
        }
        let mut entries = List::with_capacity(cap);
        if let Some(old) = self.unread.get(&person) {
            for cached in old {
                if cached.task == entry.task || cached.position == entry.position {
                    return;
                }
            }
            let mut inserted = false;
            for &cached in old {
                if !inserted && entry.position > cached.position {
                    entries.push(entry).expect("new entry fits before a cached entry");
                    inserted = true;
                }
                if entries.room() > 0 {
                    entries.push(cached).expect("cache has room");
                }
            }
            if !inserted && entries.room() > 0 {
                entries.push(entry).expect("cache has room for latest entry");
            }
        } else {
            entries.push(entry).expect("nonzero cache bound");
        }
        let saved = self.unread.insert(person, entries.into_boxed());
        assert!(saved.is_ok(), "one cache per admitted person");
    }

    /// Pure query of completed, successful restoration; false during restoring and permanently
    /// after restore failure.
    #[must_use]
    pub fn ready(&self) -> bool {
        match self.phase {
            Phase::Ready => true,
            Phase::Restoring | Phase::Failed => false,
        }
    }
}

/// Bootstrap writes one roles record per configured owner match, alongside person/sign-in records
/// and reply; completion answers every bounded waiter. Required free Request slots for one
/// step/fire under validated `limits`: maximum of bootstrap owner matches plus three, waiters plus
/// one and sign-ins plus one. `worst_case` validates those additions before use; caller counts
/// output payload bytes separately.
#[must_use]
pub fn max_out(limits: &Limits) -> u32 {
    limits
        .initial_owners
        .saturating_add(4)
        .max(limits.waiters.saturating_add(1))
        .max(limits.sign_ins.saturating_add(1))
        .max(limits.holdings.saturating_add(1))
}

/// Apply one root-issued `event` with iteration clocks and immutable configured bounds in `env`;
/// caller reserves at least `max_out(&env.limits)` free `out` slots. Emits typed
/// routing/persistence/replies, never IO; root completes each `Route` once and withholds replies
/// until required atomic writes are durable. Restore admission validates role-success shape;
/// `Restored` checks its project/person references without rechecking current membership or
/// requiring the historical roster to equal the current one. Refused role asks retain their invalid
/// targets for keyed replay.
pub fn step(domain: &mut Domain, env: &Env<Limits>, event: Event, out: &mut Queue<Request>) {
    match event {
        Event::WatchClosed { watcher } => {
            let mut closed = None;
            for (key, open) in &domain.open_watches {
                if open.watcher == watcher {
                    closed = Some(*key);
                    break;
                }
            }
            if let Some(key) = closed {
                domain.open_watches.remove(&key);
            }
        }
        Event::MakeService { request, person } => make_service(domain, env, request, person, out),
        Event::Seed { project, collaborators } => seed(domain, &env.limits, project, &collaborators, out),
        Event::Waiting { task, entries } => replace_waiting(domain, &env.limits, task, &entries),
        Event::ApplyRoles { reply_to, request } => {
            let result = apply_roles(domain, env, request, out);
            out.push(Request::RolesApplied { reply_to, request, result });
        }
        Event::Restore { record } => restore(domain, env, record, out),
        Event::Restored => restored(domain, env, out),
        Event::SignedIn { reply_to, person, sign_in, identity, kind } => {
            signin(domain, env, reply_to, person, sign_in, identity, kind, out);
        }
        Event::SignOut { reply_to, sign_in } => {
            if !domain.ready() {
                return refused(reply_to, Refusal::NotReady, out);
            }
            end_signin(domain, sign_in, out);
            out.push(Request::Reply { to: reply_to, reply: Reply::SignedOut });
        }
        Event::Roles { project, holdings } => {
            if domain.phase == Phase::Failed {
                return;
            }
            match valid_roles(domain, &env.limits, project, &holdings) {
                Ok(()) => {
                    out.push(Request::Save { record: Stored::Roles { project, holdings: holdings.clone() } });
                    let saved = domain.roles.insert(project, holdings);
                    assert!(saved.is_ok(), "roles admitted before mutation");
                    prune_unheld_roles(domain, project);
                }
                Err(refusal) => out.push(Request::RolesRefused { project, refusal }),
            }
        }
        Event::Ask { reply_to, sign_in, key, ask } => admit_ask(domain, env, reply_to, sign_in, key, ask, out),
        Event::Decided { request, outcome } => decided(domain, env, Id::<Pending>::from_token(request), outcome, out),
    }
}

fn role_number(role: Role) -> u32 {
    role.number()
}

fn newer(left: Entry, right: Entry) -> bool {
    left.at > right.at || (left.at == right.at && (left.task, left.kind) > (right.task, right.kind))
}

fn insert_newest(entries: &mut List<Entry>, entry: Entry) {
    if entries.capacity() == 0 {
        return;
    }
    let mut next = List::with_capacity(entries.capacity());
    let mut inserted = false;
    for &old in &*entries {
        if old.task == entry.task && old.kind == entry.kind {
            return;
        }
        if !inserted && newer(entry, old) {
            next.push(entry).expect("cache has room before older entry");
            inserted = true;
        }
        if next.room() > 0 {
            next.push(old).expect("cache has room for prior entry");
        }
    }
    if !inserted && next.room() > 0 {
        next.push(entry).expect("cache has room for new entry");
    }
    *entries = next;
}

fn replace_waiting(domain: &mut Domain, limits: &Limits, task: u64, entries: &[Entry]) {
    if task == 0 || domain.phase == Phase::Failed {
        return;
    }
    let mut recipients = List::with_capacity(domain.waiting.capacity());
    for (whom, _) in &domain.waiting {
        recipients.push(*whom).expect("one key per cached recipient");
    }
    for &whom in &recipients {
        let Some(old) = domain.waiting.remove(&whom) else { continue };
        let mut kept = List::with_capacity(limits.inbox_entries);
        for entry in old {
            if entry.task != task {
                kept.push(entry).expect("cached subset fits");
            }
        }
        if !kept.is_empty() {
            let saved = domain.waiting.insert(whom, kept.into_boxed());
            assert!(saved.is_ok(), "removed recipient slot remains available");
        }
    }
    for &entry in entries {
        if entry.task != task || entry.project == 0 {
            continue;
        }
        // A role with no holder has no party inbox to cache. The number of distinct held
        // roles is bounded by holdings, even when a policy defines more roles.
        match entry.whom {
            Whom::Role { project, role } => {
                if !held_role(domain, project, role) {
                    continue;
                }
            }
            Whom::Person(_) => {}
        }
        let old = domain.waiting.remove(&entry.whom);
        let mut cached = List::with_capacity(limits.inbox_entries);
        if let Some(old) = old {
            for prior in old {
                cached.push(prior).expect("cached recipient bound");
            }
        }
        insert_newest(&mut cached, entry);
        if !cached.is_empty() {
            drop(domain.waiting.insert(entry.whom, cached.into_boxed()));
        }
    }
}

fn prune_unheld_roles(domain: &mut Domain, project: u32) {
    let mut stale = List::with_capacity(domain.waiting.capacity());
    for (whom, _) in &domain.waiting {
        match *whom {
            Whom::Role { project: entry_project, role } => {
                if entry_project == project && !held_role(domain, project, role) {
                    stale.push(*whom).expect("one cached role per waiting key");
                }
            }
            Whom::Person(_) => {}
        }
    }
    for whom in &stale {
        domain.waiting.remove(whom);
    }
}

fn held_role(domain: &Domain, project: u32, role: u32) -> bool {
    let Some(holdings) = domain.roles.get(&project) else {
        return false;
    };
    for holding in holdings {
        if holding.role.number() == role {
            return true;
        }
    }
    false
}

/// One expiration per fire; other expirations stay due for later iterations. Expire at most one due
/// sign-in using `env.now`, emitting its `Erase`; no work before ready. Caller reserves
/// `max_out(&env.limits)` free slots and calls again in later iterations while due; parent owns
/// durability and secret cleanup.
pub fn fire(domain: &mut Domain, env: &Env<Limits>, out: &mut Queue<Request>) {
    if !domain.ready() {
        return;
    }
    if let Some(key) = domain.answer_alarms.expire(env.now) {
        let removed = domain.answers.remove(&key);
        assert!(removed.is_some(), "retained answer has its expiry alarm");
        out.push(Request::Erase { key: Key::Answer(key) });
    } else if let Some(number) = domain.alarms.expire(env.now) {
        end_signin(domain, number, out);
    }
}

fn answer_expiry(at: Wall, limits: &Limits) -> Wall {
    Wall::from_nanos(at.as_nanos().saturating_add(limits.request_retention.as_nanos()))
}

fn fact(domain: &mut Domain, observation: Fact) {
    if domain.facts.try_push(observation).is_err() {
        domain.lost = domain.lost.saturating_add(1);
    }
}

fn refused(to: ReplyTo, refusal: Refusal, out: &mut Queue<Request>) {
    out.push(Request::Reply { to, reply: Reply::Refused(refusal) });
}

fn valid_identity(limits: &Limits, identity: &Identity) -> bool {
    let Some(first) = identity.key.subject.len().checked_add(identity.login.len()) else {
        return false;
    };
    match first.checked_add(identity.name.len()) {
        Some(bytes) => {
            !identity.key.subject.is_empty() && bytes <= usize::try_from(limits.identity_bytes).expect("u32 fits usize")
        }
        None => false,
    }
}

fn apply_roles(
    domain: &mut Domain,
    env: &Env<Limits>,
    request: Token,
    out: &mut Queue<Request>,
) -> Result<(), Refusal> {
    if !domain.ready() {
        return Err(Refusal::NotReady);
    }
    let flight = domain.pending.get(Id::from_token(request)).ok_or(Refusal::Unknown)?;
    let project = match &flight.ask {
        Ask::SetRoles { project, .. } => *project,
        Ask::MakeService { .. }
        | Ask::Watch { .. }
        | Ask::EditNote { .. }
        | Ask::Adopt { .. }
        | Ask::ChangePolicy { .. }
        | Ask::SetPool { .. }
        | Ask::StartChat { .. }
        | Ask::DecideEscalation { .. }
        | Ask::DecideProposal { .. }
        | Ask::Say { .. }
        | Ask::AnswerQuestion { .. }
        | Ask::Prioritise { .. }
        | Ask::Amend { .. }
        | Ask::Move { .. }
        | Ask::TakePerson { .. }
        | Ask::HandBackPerson { .. }
        | Ask::AnswerPerson { .. }
        | Ask::Stop { .. }
        | Ask::Cancel { .. }
        | Ask::Release { .. }
        | Ask::SetGoal { .. } => {
            return Err(Refusal::Unknown);
        }
    };
    if !domain.roles.contains_key(&project) {
        return Err(Refusal::Unknown);
    }
    if role(domain, flight.key.person, project) != Some(Role::Owner) {
        return Err(Refusal::Role);
    }
    let holdings = match &flight.ask {
        Ask::SetRoles { holdings, .. } => {
            valid_roles(domain, &env.limits, project, holdings)?;
            for holding in holdings {
                if holding.person == 0 {
                    return Err(Refusal::Limit);
                }
                if !domain.people.contains_key(&holding.person) {
                    return Err(Refusal::Unknown);
                }
            }
            holdings.clone()
        }
        Ask::MakeService { .. }
        | Ask::Watch { .. }
        | Ask::EditNote { .. }
        | Ask::Adopt { .. }
        | Ask::ChangePolicy { .. }
        | Ask::SetPool { .. }
        | Ask::StartChat { .. }
        | Ask::DecideEscalation { .. }
        | Ask::DecideProposal { .. }
        | Ask::Say { .. }
        | Ask::AnswerQuestion { .. }
        | Ask::Prioritise { .. }
        | Ask::Amend { .. }
        | Ask::Move { .. }
        | Ask::TakePerson { .. }
        | Ask::HandBackPerson { .. }
        | Ask::AnswerPerson { .. }
        | Ask::Stop { .. }
        | Ask::Cancel { .. }
        | Ask::Release { .. }
        | Ask::SetGoal { .. } => {
            unreachable!("validated roster flight")
        }
    };
    out.push(Request::Save { record: Stored::Roles { project, holdings: holdings.clone() } });
    let saved = domain.roles.insert(project, holdings);
    assert!(saved.is_ok(), "existing role row replaces without consuming capacity");
    prune_unheld_roles(domain, project);
    Ok(())
}

fn valid_roles(domain: &Domain, limits: &Limits, project: u32, holdings: &[Holding]) -> Result<(), Refusal> {
    if holdings.len() > usize::try_from(limits.holdings).expect("u32 fits usize") {
        return Err(Refusal::Limit);
    }
    if !domain.roles.contains_key(&project) && domain.roles.len() >= limits.projects {
        return Err(Refusal::Busy);
    }
    for (at, holding) in holdings.iter().enumerate() {
        match holding.role {
            Role::Policy { role } if role <= 3 => return Err(Refusal::Limit),
            Role::Owner | Role::Maintainer | Role::Member | Role::Observer | Role::Policy { .. } => {}
        }
        for earlier in holdings.iter().take(at) {
            if earlier.person == holding.person {
                return Err(Refusal::Limit);
            }
        }
    }
    Ok(())
}

fn seed(domain: &mut Domain, limits: &Limits, project: u32, collaborators: &[Seed], out: &mut Queue<Request>) {
    assert!(domain.can_seed(limits, project, collaborators), "root preflights an adoption seed");
    let old = domain.roles.get(&project).expect("preflighted project");
    let mut holdings = List::with_capacity(limits.holdings);
    for holding in &**old {
        holdings.push(*holding).expect("preflighted roster room");
    }
    let mut changed = false;
    for collaborator in collaborators {
        let person = match domain.identities.get(&collaborator.identity) {
            Some(person) => *person,
            None => {
                let person = collaborator.candidate;
                let identity = Identity { key: collaborator.identity.clone(), login: Box::new([]), name: Box::new([]) };
                domain.identities.insert(collaborator.identity.clone(), person).expect("preflighted identity room");
                domain.people.insert(person, identity.clone()).expect("preflighted person room");
                out.push(Request::Save { record: Stored::Person { number: person, identity } });
                person
            }
        };
        if !has_person(holdings.as_slice(), person) {
            holdings.push(Holding { person, role: collaborator.role }).expect("preflighted roster room");
            changed = true;
        }
    }
    if changed {
        let holdings = holdings.into_boxed();
        domain.roles.insert(project, holdings.clone()).expect("preflighted project");
        out.push(Request::Save { record: Stored::Roles { project, holdings } });
    }
}

fn has_person(holdings: &[Holding], person: u64) -> bool {
    for holding in holdings {
        if holding.person == person {
            return true;
        }
    }
    false
}

fn make_service(domain: &mut Domain, env: &Env<Limits>, request: Token, person: u64, out: &mut Queue<Request>) {
    if !domain.ready() {
        out.push(Request::ServiceMade { request, outcome: Outcome::Refused(Refusal::NotReady) });
        return;
    }
    let flight = domain.pending.get(Id::from_token(request)).expect("service creation names a live keyed flight");
    let (project, name, service_role) = match &flight.ask {
        Ask::MakeService { project, name, role } => (*project, name.clone(), *role),
        Ask::Watch { .. }
        | Ask::EditNote { .. }
        | Ask::Adopt { .. }
        | Ask::SetGoal { .. }
        | Ask::Stop { .. }
        | Ask::Cancel { .. }
        | Ask::Release { .. }
        | Ask::TakePerson { .. }
        | Ask::HandBackPerson { .. }
        | Ask::AnswerPerson { .. }
        | Ask::Move { .. }
        | Ask::DecideProposal { .. }
        | Ask::Say { .. }
        | Ask::AnswerQuestion { .. }
        | Ask::Prioritise { .. }
        | Ask::Amend { .. }
        | Ask::SetRoles { .. }
        | Ask::ChangePolicy { .. }
        | Ask::SetPool { .. }
        | Ask::DecideEscalation { .. }
        | Ask::StartChat { .. } => unreachable!("service creation is routed only from its ask"),
    };
    let identity = Identity {
        key: IdentityKey { provider: domain.deployment_provider, subject: person.to_be_bytes().into() },
        login: Box::new([]),
        name,
    };
    let holdings = domain.roles.get(&project);
    let outcome = if role(domain, flight.key.person, project) != Some(Role::Owner) {
        Outcome::Refused(Refusal::Role)
    } else if person == 0 || domain.people.contains_key(&person) || domain.identities.contains_key(&identity.key) {
        Outcome::Refused(Refusal::Unknown)
    } else if !valid_identity(&env.limits, &identity) {
        Outcome::Refused(Refusal::Limit)
    } else if domain.people.len() >= env.limits.people
        || match holdings {
            Some(rows) => rows.len() >= usize::try_from(env.limits.holdings).expect("u32 fits usize"),
            None => true,
        }
    {
        Outcome::Refused(Refusal::Busy)
    } else {
        let old = holdings.expect("project room checked");
        let mut next = List::with_capacity(env.limits.holdings);
        for holding in &**old {
            next.push(*holding).expect("preflighted role room");
        }
        next.push(Holding { person, role: service_role }).expect("preflighted role room");
        let next = next.into_boxed();
        let indexed = domain.identities.insert(identity.key.clone(), person);
        assert!(indexed == Ok(None), "service identity preflighted");
        let saved = domain.people.insert(person, identity.clone());
        assert!(saved == Ok(None), "service person preflighted");
        let saved = domain.roles.insert(project, next.clone());
        assert!(saved.is_ok(), "service project preflighted");
        out.push(Request::Save { record: Stored::Person { number: person, identity } });
        out.push(Request::Save { record: Stored::Roles { project, holdings: next } });
        Outcome::ServiceMade { person }
    };
    out.push(Request::ServiceMade { request, outcome });
}

#[expect(
    clippy::too_many_arguments,
    reason = "the authenticated sign-in has its identity, kind and root-issued numbers"
)]
fn signin(
    domain: &mut Domain,
    env: &Env<Limits>,
    to: ReplyTo,
    candidate: u64,
    number: u64,
    identity: Identity,
    kind: Kind,
    out: &mut Queue<Request>,
) {
    if !domain.ready() {
        return refused(to, Refusal::NotReady, out);
    }
    if !valid_identity(&env.limits, &identity) {
        return refused(to, Refusal::Limit, out);
    }
    match kind {
        Kind::Person if identity.key.provider == domain.deployment_provider => {
            return refused(to, Refusal::SignIn, out);
        }
        Kind::Service if identity.key.provider != domain.deployment_provider => {
            return refused(to, Refusal::SignIn, out);
        }
        Kind::Person | Kind::Service => {}
    }
    let known = domain.identities.get(&identity.key).copied();
    if kind == Kind::Service && known.is_none() {
        return refused(to, Refusal::SignIn, out);
    }
    let person = known.unwrap_or(candidate);
    if let Some(old) = domain.sign_ins.get(&number) {
        if old.person != person {
            return refused(to, Refusal::SignIn, out);
        }
        if old.expires <= env.wall || old.due <= env.now {
            return refused(to, Refusal::SignIn, out);
        }
        return out.push(Request::Reply { to, reply: Reply::SignedIn { person, expires: old.expires } });
    }
    if domain.sign_ins.len() >= env.limits.sign_ins {
        return refused(to, Refusal::Busy, out);
    }
    if known.is_none() {
        if domain.people.len() >= env.limits.people {
            return refused(to, Refusal::Busy, out);
        }
        assert!(!domain.people.contains_key(&person), "root gives a fresh candidate person number");
        // Reserve all bootstrap role slots before making either record.
        for owner in &domain.owners {
            if owner.identity == identity.key {
                let Some(holdings) = domain.roles.get(&owner.project) else {
                    return refused(to, Refusal::Unknown, out);
                };
                if holdings.len() >= usize::try_from(env.limits.holdings).expect("u32 fits usize")
                    && !has_person(holdings, person)
                {
                    return refused(to, Refusal::Busy, out);
                }
            }
        }
    }
    let Some(expires_nanos) = env.wall.as_nanos().checked_add(env.limits.sign_in_lifetime.as_nanos()) else {
        return refused(to, Refusal::Limit, out);
    };
    let expires = Wall::from_nanos(expires_nanos);
    if known.is_none() {
        let indexed = domain.identities.insert(identity.key.clone(), person);
        assert!(indexed == Ok(None), "new identity has room");
        // Each project occurs at most once for this identity in configuration.
        for owner in &domain.owners {
            if owner.identity != identity.key {
                continue;
            }
            let old = domain.roles.get(&owner.project).expect("bootstrap project admitted");
            let mut amended = List::with_capacity(env.limits.holdings);
            for holding in &**old {
                if holding.person != person {
                    amended.push(*holding).expect("bounded list room checked");
                }
            }
            amended.push(Holding { person, role: Role::Owner }).expect("bounded list room checked");
            let holdings = amended.into_boxed();
            out.push(Request::Save { record: Stored::Roles { project: owner.project, holdings: holdings.clone() } });
            let saved = domain.roles.insert(owner.project, holdings);
            assert!(saved.is_ok(), "existing bootstrap project");
        }
    }
    let changed = kind == Kind::Person
        && match domain.people.get(&person) {
            Some(old) => old != &identity,
            None => true,
        };
    if changed {
        out.push(Request::Save { record: Stored::Person { number: person, identity: identity.clone() } });
        let saved = domain.people.insert(person, identity);
        assert!(saved.is_ok(), "person admitted before mutation");
    }
    let saved = domain.sign_ins.insert(number, SignIn { person, expires, due: deadline(env, expires) });
    assert!(saved == Ok(None), "sign-in admitted before mutation");
    arm(domain, env, number, expires);
    out.push(Request::Save { record: Stored::SignIn { number, person, expires } });
    out.push(Request::Reply { to, reply: Reply::SignedIn { person, expires } });
    fact(domain, Fact::SignedIn { person, sign_in: number });
}

fn deadline(env: &Env<Limits>, expires: Wall) -> Time {
    let left = skein_lib::Duration::from_nanos(expires.as_nanos().saturating_sub(env.wall.as_nanos()));
    env.now.saturating_add(left)
}

fn arm(domain: &mut Domain, env: &Env<Limits>, number: u64, expires: Wall) {
    let armed = domain.alarms.arm(number, deadline(env, expires));
    assert!(armed.is_ok(), "one alarm per admitted sign-in");
}

fn end_signin(domain: &mut Domain, number: u64, out: &mut Queue<Request>) {
    if domain.sign_ins.remove(&number).is_some() {
        domain.alarms.cancel(number);
        out.push(Request::Erase { key: Key::SignIn(number) });
        fact(domain, Fact::SignedOut { sign_in: number });
    }
}

fn project(ask: &Ask) -> u32 {
    match ask {
        Ask::Watch { project, .. }
        | Ask::EditNote { project, .. }
        | Ask::MakeService { project, .. }
        | Ask::Adopt { project, .. }
        | Ask::SetRoles { project, .. }
        | Ask::ChangePolicy { project, .. }
        | Ask::SetPool { project, .. }
        | Ask::StartChat { project, .. }
        | Ask::DecideEscalation { project, .. }
        | Ask::DecideProposal { project, .. }
        | Ask::Say { project, .. }
        | Ask::AnswerQuestion { project, .. }
        | Ask::Prioritise { project, .. }
        | Ask::Amend { project, .. }
        | Ask::Move { project, .. }
        | Ask::TakePerson { project, .. }
        | Ask::HandBackPerson { project, .. }
        | Ask::AnswerPerson { project, .. }
        | Ask::Stop { project, .. }
        | Ask::Cancel { project, .. }
        | Ask::Release { project, .. }
        | Ask::SetGoal { project, .. } => *project,
    }
}

fn watch_ask(ask: &Ask) -> bool {
    match ask {
        Ask::Watch { .. } => true,
        Ask::EditNote { .. }
        | Ask::MakeService { .. }
        | Ask::Adopt { .. }
        | Ask::SetRoles { .. }
        | Ask::ChangePolicy { .. }
        | Ask::SetPool { .. }
        | Ask::StartChat { .. }
        | Ask::DecideEscalation { .. }
        | Ask::DecideProposal { .. }
        | Ask::Say { .. }
        | Ask::AnswerQuestion { .. }
        | Ask::Prioritise { .. }
        | Ask::Amend { .. }
        | Ask::Move { .. }
        | Ask::TakePerson { .. }
        | Ask::HandBackPerson { .. }
        | Ask::AnswerPerson { .. }
        | Ask::Stop { .. }
        | Ask::Cancel { .. }
        | Ask::Release { .. }
        | Ask::SetGoal { .. } => false,
    }
}

fn valid_adoption(limits: &Limits, adoption: &crate::Adoption) -> bool {
    let Some(items) = adoption.resource.path.len().checked_add(adoption.options.len()) else { return false };
    let Some(mut bytes) = items.checked_mul(size_of::<Box<[u8]>>()) else { return false };
    for segment in &adoption.resource.path {
        let Some(next) = bytes.checked_add(segment.len()) else { return false };
        bytes = next;
    }
    for option in &adoption.options {
        let Some(next) = bytes.checked_add(option.len()) else { return false };
        bytes = next;
    }
    !adoption.resource.path.is_empty() && bytes <= usize::try_from(limits.amendment_bytes).expect("u32 fits usize")
}

fn note_pattern_bytes(pattern: &crate::Pattern) -> Option<usize> {
    let mut bytes = pattern.segments.len().checked_mul(size_of::<Box<[u8]>>())?;
    for segment in &pattern.segments {
        bytes = bytes.checked_add(segment.len())?;
    }
    let last = match &pattern.last {
        crate::Last::Exact(bytes) | crate::Last::Open(bytes) => bytes.len(),
    };
    bytes.checked_add(last)
}

fn valid_note(limits: &Limits, name: u64, scope: &crate::NoteScope, change: &crate::NoteChange) -> bool {
    if name == 0 {
        return false;
    }
    let scope_bytes = match scope {
        crate::NoteScope::Deployment | crate::NoteScope::Project => 0,
        crate::NoteScope::Goal { goal } => {
            if *goal == 0 {
                return false;
            }
            0
        }
        crate::NoteScope::Resources { pattern, .. } => {
            let Some(bytes) = note_pattern_bytes(pattern) else { return false };
            bytes
        }
    };
    let change_bytes = match change {
        crate::NoteChange::Correct { description, body, references, recalled } => {
            if *recalled == 0 {
                return false;
            }
            for number in references {
                if *number == 0 {
                    return false;
                }
            }
            let Some(bytes) = description.len().checked_add(body.len()) else { return false };
            let Some(ref_bytes) = references.len().checked_mul(size_of::<u64>()) else { return false };
            let Some(total) = bytes.checked_add(ref_bytes) else { return false };
            total
        }
        crate::NoteChange::Delete { recalled } => {
            if *recalled == 0 {
                return false;
            }
            0
        }
    };
    match scope_bytes.checked_add(change_bytes) {
        Some(bytes) => bytes <= usize::try_from(limits.amendment_bytes).expect("u32 fits usize"),
        None => false,
    }
}

fn valid_ask(limits: &Limits, ask: &Ask) -> bool {
    match ask {
        Ask::Watch { subject, .. } => match subject {
            crate::WatchSubject::Run { task, attempt } => *task != 0 && *attempt != 0,
            crate::WatchSubject::Tree { task } => *task != 0,
            crate::WatchSubject::Goals => true,
            crate::WatchSubject::Inbox { party } => *party != 0,
        },
        Ask::EditNote { name, scope, change, .. } => valid_note(limits, *name, scope, change),
        Ask::MakeService { name, .. } => {
            let Some(bytes) = name.len().checked_add(size_of::<u64>()) else {
                return false;
            };
            !name.is_empty()
                && name.len() <= usize::try_from(limits.words).expect("u32 fits usize")
                && bytes <= usize::try_from(limits.identity_bytes).expect("u32 fits usize")
        }
        Ask::Move { task, reason, .. } | Ask::Cancel { task, reason, .. } => {
            *task != 0 && reason.len() <= usize::try_from(limits.words).expect("u32 fits usize")
        }
        Ask::TakePerson { task, .. }
        | Ask::HandBackPerson { task, .. }
        | Ask::Stop { task, .. }
        | Ask::Release { task, .. } => *task != 0,
        Ask::AnswerPerson { task, result, .. } => {
            *task != 0
                && match result {
                    crate::PersonResult::Report { words } | crate::PersonResult::Verdict { words, .. } => {
                        words.len() <= usize::try_from(limits.words).expect("u32 fits usize")
                    }
                    crate::PersonResult::Failure { reason } => {
                        reason.len() <= usize::try_from(limits.words).expect("u32 fits usize")
                    }
                }
        }
        Ask::Adopt { adoption, .. } => valid_adoption(limits, adoption),
        Ask::SetRoles { holdings, .. } => holdings.len() <= usize::try_from(limits.holdings).expect("u32 fits usize"),
        Ask::ChangePolicy { change, .. } => match crate::policy_change_bytes(change) {
            Some(bytes) => bytes <= u64::from(limits.amendment_bytes),
            None => false,
        },
        Ask::SetPool { person, .. } => *person != 0,
        Ask::SetGoal { spec, charter, .. } => {
            *charter != 0 && !spec.is_empty() && spec.len() <= usize::try_from(limits.words).expect("u32 fits usize")
        }
        Ask::StartChat { words, .. } => words.len() <= usize::try_from(limits.words).expect("u32 fits usize"),
        Ask::Say { task, words, .. } => {
            *task != 0 && !words.is_empty() && words.len() <= usize::try_from(limits.words).expect("u32 fits usize")
        }
        Ask::AnswerQuestion { task, question, words, .. } => {
            *task != 0
                && *question != 0
                && !words.is_empty()
                && words.len() <= usize::try_from(limits.words).expect("u32 fits usize")
        }
        Ask::Prioritise { goals, .. } => {
            if goals.is_empty() || goals.len() > usize::try_from(limits.goals).expect("u32 fits usize") {
                return false;
            }
            for (task, _) in goals {
                if *task == 0 {
                    return false;
                }
            }
            true
        }
        Ask::Amend { task, amendment, .. } => {
            *task != 0
                && amendment.reason.len() <= usize::try_from(limits.words).expect("u32 fits usize")
                && match crate::amendment_bytes(amendment) {
                    Some(bytes) => bytes <= u64::from(limits.amendment_bytes),
                    None => false,
                }
        }
        Ask::DecideEscalation { task, revision, decision, .. } => {
            *task != 0
                && *revision != 0
                && match decision {
                    crate::EscalationDecision::Release | crate::EscalationDecision::Pass => true,
                    crate::EscalationDecision::Reject { reason } => {
                        reason.len() <= usize::try_from(limits.words).expect("u32 fits usize")
                    }
                }
        }
        Ask::DecideProposal { proposer, proposal, decision, .. } => {
            *proposer != 0
                && *proposal != 0
                && match decision {
                    crate::ProposalDecision::Accept | crate::ProposalDecision::Pass => true,
                    crate::ProposalDecision::Reject { reason } => {
                        reason.len() <= usize::try_from(limits.words).expect("u32 fits usize")
                    }
                }
        }
    }
}

fn valid_answer_shape(ask: &Ask, outcome: Outcome) -> bool {
    #[derive(PartialEq, Eq)]
    enum Expected {
        Watch,
        Note(u64),
        Service,
        Goal,
        Stop(u64),
        Cancel(u64),
        Release(u64),
        Take(u64),
        HandBack(u64),
        AnswerPerson(u64),
        Move(u64),
        Proposal(u64, u64),
        Say(u64),
        AnswerQuestion(u64, u64),
        Prioritise(u32),
        Amend(u64),
        Roles(u32, bool),
        Policy(u32),
        Pool(u32, u64),
        Adoption(u32),
        Escalation,
        Chat,
    }

    let expected = match ask {
        Ask::Watch { .. } => Expected::Watch,
        Ask::EditNote { name, .. } => Expected::Note(*name),
        Ask::MakeService { .. } => Expected::Service,
        Ask::Adopt { project, .. } => Expected::Adoption(*project),
        Ask::SetGoal { .. } => Expected::Goal,
        Ask::Stop { task, .. } => Expected::Stop(*task),
        Ask::Cancel { task, .. } => Expected::Cancel(*task),
        Ask::Release { task, .. } => Expected::Release(*task),
        Ask::TakePerson { task, .. } => Expected::Take(*task),
        Ask::HandBackPerson { task, .. } => Expected::HandBack(*task),
        Ask::AnswerPerson { task, .. } => Expected::AnswerPerson(*task),
        Ask::Move { task, .. } => Expected::Move(*task),
        Ask::DecideProposal { proposer, proposal, .. } => Expected::Proposal(*proposer, *proposal),
        Ask::Say { task, .. } => Expected::Say(*task),
        Ask::AnswerQuestion { task, question, .. } => Expected::AnswerQuestion(*task, *question),
        Ask::Prioritise { project, .. } => Expected::Prioritise(*project),
        Ask::Amend { task, .. } => Expected::Amend(*task),
        Ask::SetRoles { project, holdings } => {
            let mut valid = true;
            for (at, holding) in holdings.iter().enumerate() {
                if holding.person == 0 {
                    valid = false;
                }
                for earlier in holdings.get(..at).expect("enumerated holding position is in bounds") {
                    if earlier.person == holding.person {
                        valid = false;
                    }
                }
            }
            Expected::Roles(*project, valid)
        }
        Ask::ChangePolicy { project, .. } => Expected::Policy(*project),
        Ask::SetPool { project, person, .. } => Expected::Pool(*project, *person),
        Ask::DecideEscalation { .. } => Expected::Escalation,
        Ask::StartChat { .. } => Expected::Chat,
    };

    match outcome {
        Outcome::Watching { .. } => expected == Expected::Watch,
        Outcome::NoteEdited { name } => expected == Expected::Note(name),
        Outcome::ServiceMade { person } => expected == Expected::Service && person != 0,
        Outcome::Adopted { project } => expected == Expected::Adoption(project),
        Outcome::GoalStarted { task } => expected == Expected::Goal && task != 0,
        Outcome::GoalProposed { proposal } => expected == Expected::Goal && proposal != 0,
        Outcome::Stopped { task } => expected == Expected::Stop(task),
        Outcome::Cancelled { task } => expected == Expected::Cancel(task),
        Outcome::Released { task } => expected == Expected::Release(task),
        Outcome::PersonTaken { task } => expected == Expected::Take(task),
        Outcome::PersonHandedBack { task } => expected == Expected::HandBack(task),
        Outcome::PersonAnswered { task } => expected == Expected::AnswerPerson(task),
        Outcome::Moved { task } => expected == Expected::Move(task),
        Outcome::ProposalDecided { proposer, proposal, .. } => expected == Expected::Proposal(proposer, proposal),
        Outcome::Said { task, message } => expected == Expected::Say(task) && message != 0,
        Outcome::QuestionAnswered { task, question, message } => {
            expected == Expected::AnswerQuestion(task, question) && message != 0
        }
        Outcome::Prioritised { project } => expected == Expected::Prioritise(project),
        Outcome::Amended { task } => expected == Expected::Amend(task),
        Outcome::AmendProposed { task, proposal } => expected == Expected::Amend(task) && proposal != 0,
        Outcome::RolesSet { project } => expected == Expected::Roles(project, true),
        Outcome::PolicyChanged { project } => expected == Expected::Policy(project),
        Outcome::PoolSet { project, person } => expected == Expected::Pool(project, person),
        Outcome::EscalationDecided { .. } => expected == Expected::Escalation,
        Outcome::Started { task } => expected == Expected::Chat && task != 0,
        Outcome::Refused(_) => true,
    }
}

fn role(domain: &Domain, person: u64, project: u32) -> Option<Role> {
    for holding in &**domain.roles.get(&project)? {
        if holding.person == person {
            return Some(holding.role);
        }
    }
    None
}

#[expect(clippy::too_many_lines, reason = "each keyed request is admitted with its role and replay check")]
fn admit_ask(
    domain: &mut Domain,
    env: &Env<Limits>,
    to: ReplyTo,
    sign_in: u64,
    bytes: [u8; 16],
    ask: Ask,
    out: &mut Queue<Request>,
) {
    if !domain.ready() {
        return refused(to, Refusal::NotReady, out);
    }
    let Some(session) = domain.sign_ins.get(&sign_in) else {
        return refused(to, Refusal::SignIn, out);
    };
    if session.expires <= env.wall || session.due <= env.now {
        return refused(to, Refusal::SignIn, out);
    }
    let key = RequestKey { person: session.person, key: bytes };
    if !valid_ask(&env.limits, &ask) {
        return refused(to, Refusal::Limit, out);
    }
    if let Some(answer) = domain.answers.get(&key)
        && (answer_expiry(answer.at, &env.limits) <= env.wall || answer.due <= env.now)
    {
        if watch_ask(&ask) {
            // Expiry housekeeping is a store decision of its own; opening a view writes nothing.
            return refused(to, Refusal::Busy, out);
        }
        let old = domain.answers.remove(&key);
        assert!(old.is_some(), "expired answer was retained");
        domain.answer_alarms.cancel(key);
        out.push(Request::Erase { key: Key::Answer(key) });
    }
    if let Some(answer) = domain.answers.get(&key) {
        if answer.ask != ask {
            return refused(to, Refusal::KeyConflict, out);
        }
        return out.push(Request::Reply { to, reply: Reply::Outcome(answer.outcome) });
    }
    if let Some(open) = domain.open_watches.get(&key) {
        if open.ask != ask {
            return refused(to, Refusal::KeyConflict, out);
        }
        return out.push(Request::Reply { to, reply: Reply::Outcome(Outcome::Watching { watcher: open.watcher }) });
    }
    if let Some(id) = domain.flights.get(&key).copied() {
        let flight = domain.pending.get_mut(id).expect("key index names live pending request");
        if flight.ask != ask {
            return refused(to, Refusal::KeyConflict, out);
        }
        if flight.replies.len() >= env.limits.waiters {
            return refused(to, Refusal::Busy, out);
        }
        flight.replies.push(to).expect("bounded list room checked");
        return;
    }
    // An accepted flight reserves one answered record, even while pending.
    if domain.answers.len().saturating_add(domain.flights.len()).saturating_add(domain.open_watches.len())
        >= env.limits.requests
    {
        return refused(to, Refusal::Busy, out);
    }
    let project = project(&ask);
    let role = role(domain, key.person, project);
    let refusal = match &ask {
        Ask::Watch { subject, .. } => match subject {
            crate::WatchSubject::Inbox { party } if *party == key.person => None,
            crate::WatchSubject::Inbox { .. } => Some(Refusal::Standing),
            crate::WatchSubject::Run { .. } | crate::WatchSubject::Tree { .. } | crate::WatchSubject::Goals => {
                if role.is_some() { None } else { Some(Refusal::Role) }
            }
        },
        Ask::EditNote { .. } => match role {
            Some(Role::Owner | Role::Maintainer | Role::Member | Role::Policy { .. }) => None,
            Some(Role::Observer) | None => Some(Refusal::Role),
        },
        Ask::MakeService { .. }
        | Ask::Adopt { .. }
        | Ask::SetRoles { .. }
        | Ask::ChangePolicy { .. }
        | Ask::SetPool { .. } => {
            if !domain.roles.contains_key(&project) {
                Some(Refusal::Unknown)
            } else if role == Some(Role::Owner) {
                None
            } else {
                Some(Refusal::Role)
            }
        }
        Ask::StartChat { .. } => {
            let service = match domain.people.get(&key.person) {
                Some(party) => party.key.provider == domain.deployment_provider,
                None => false,
            };
            if service {
                Some(Refusal::Role)
            } else {
                match role {
                    Some(Role::Owner | Role::Maintainer | Role::Member | Role::Policy { .. }) => None,
                    Some(Role::Observer) | None => Some(Refusal::Role),
                }
            }
        }
        Ask::SetGoal { .. } => match role {
            Some(Role::Owner | Role::Maintainer | Role::Member | Role::Policy { .. }) => None,
            Some(Role::Observer) | None => Some(Refusal::Role),
        },
        Ask::Say { .. } | Ask::AnswerQuestion { .. } => match role {
            Some(Role::Owner | Role::Maintainer | Role::Member | Role::Policy { .. }) => None,
            Some(Role::Observer) | None => Some(Refusal::Role),
        },
        Ask::Prioritise { .. } => match role {
            Some(Role::Owner | Role::Maintainer | Role::Policy { .. }) => None,
            Some(Role::Member | Role::Observer) | None => Some(Refusal::Role),
        },
        Ask::Amend { .. } => match role {
            Some(Role::Owner | Role::Maintainer | Role::Member | Role::Policy { .. }) => None,
            Some(Role::Observer) | None => Some(Refusal::Role),
        },
        Ask::DecideEscalation { .. }
        | Ask::DecideProposal { .. }
        | Ask::Move { .. }
        | Ask::TakePerson { .. }
        | Ask::HandBackPerson { .. }
        | Ask::AnswerPerson { .. }
        | Ask::Stop { .. }
        | Ask::Cancel { .. }
        | Ask::Release { .. } => None,
    };
    if let Some(refusal) = refusal {
        let outcome = Outcome::Refused(refusal);
        if !watch_ask(&ask) {
            save_answer(domain, env, key, ask, outcome, out);
        }
        return out.push(Request::Reply { to, reply: Reply::Outcome(outcome) });
    }
    if domain.pending.is_full() {
        return refused(to, Refusal::Busy, out);
    }
    let mut replies = List::with_capacity(env.limits.waiters);
    replies.push(to).expect("bounded list room checked");
    let flight = Pending { key, ask: ask.clone(), replies };
    let id = domain.pending.insert(flight).expect("pending request admitted");
    let indexed = domain.flights.insert(key, id);
    assert!(indexed == Ok(None), "one key per flight within pending capacity");
    out.push(Request::Route { request: id.token(), person: key.person, project, role, ask: Box::new(ask) });
    fact(domain, Fact::Routed { person: key.person });
}

fn save_answer(
    domain: &mut Domain,
    env: &Env<Limits>,
    key: RequestKey,
    ask: Ask,
    outcome: Outcome,
    out: &mut Queue<Request>,
) {
    out.push(Request::Save { record: Stored::Answer { key, ask: Box::new(ask.clone()), outcome, at: env.wall } });
    let due = deadline(env, answer_expiry(env.wall, &env.limits));
    let saved = domain.answers.insert(key, Answered { ask, outcome, at: env.wall, due });
    assert!(saved.is_ok(), "answer room reserved at request entrance");
    let armed = domain.answer_alarms.arm(key, due);
    assert!(armed.is_ok(), "one expiry per retained completed key");
    fact(domain, Fact::Answered { person: key.person });
}

fn decided(domain: &mut Domain, env: &Env<Limits>, id: Id<Pending>, outcome: Outcome, out: &mut Queue<Request>) {
    let flight = domain.pending.get_mut(id).expect("root returns each routed request exactly once");
    let key = flight.key;
    let ask = flight.ask.clone();
    let replies = core::mem::replace(&mut flight.replies, List::with_capacity(0));
    domain.pending.retire(id);
    let removed = domain.flights.remove(&key);
    assert!(removed == Some(id), "flight has one key index");
    if watch_ask(&ask) {
        match outcome {
            Outcome::Watching { watcher } => {
                let inserted = domain.open_watches.insert(key, OpenWatch { ask, watcher });
                assert!(inserted == Ok(None), "watch reserves its volatile key slot at admission");
            }
            Outcome::Refused(_) => {}
            Outcome::NoteEdited { .. }
            | Outcome::ServiceMade { .. }
            | Outcome::Adopted { .. }
            | Outcome::GoalStarted { .. }
            | Outcome::GoalProposed { .. }
            | Outcome::Stopped { .. }
            | Outcome::Cancelled { .. }
            | Outcome::Released { .. }
            | Outcome::PersonTaken { .. }
            | Outcome::PersonHandedBack { .. }
            | Outcome::PersonAnswered { .. }
            | Outcome::Moved { .. }
            | Outcome::ProposalDecided { .. }
            | Outcome::Said { .. }
            | Outcome::QuestionAnswered { .. }
            | Outcome::Prioritised { .. }
            | Outcome::Amended { .. }
            | Outcome::AmendProposed { .. }
            | Outcome::RolesSet { .. }
            | Outcome::PolicyChanged { .. }
            | Outcome::PoolSet { .. }
            | Outcome::EscalationDecided { .. }
            | Outcome::Started { .. } => unreachable!("watch has only an opened or refused terminal"),
        }
        for to in replies.into_boxed() {
            out.push(Request::Reply { to, reply: Reply::Outcome(outcome) });
        }
        return;
    }
    match outcome {
        Outcome::Watching { .. } => unreachable!("only a watch can open a view"),
        // A refused admission is retryable with the same key, including
        // pressure reported by tasks or another child through the root.
        Outcome::Refused(Refusal::Busy | Refusal::NotReady) => {}
        Outcome::NoteEdited { .. }
        | Outcome::ServiceMade { .. }
        | Outcome::Adopted { .. }
        | Outcome::RolesSet { .. }
        | Outcome::PolicyChanged { .. }
        | Outcome::PoolSet { .. }
        | Outcome::Started { .. }
        | Outcome::Said { .. }
        | Outcome::QuestionAnswered { .. }
        | Outcome::Prioritised { .. }
        | Outcome::Amended { .. }
        | Outcome::AmendProposed { .. }
        | Outcome::Moved { .. }
        | Outcome::Stopped { .. }
        | Outcome::Cancelled { .. }
        | Outcome::Released { .. }
        | Outcome::GoalStarted { .. }
        | Outcome::GoalProposed { .. }
        | Outcome::PersonTaken { .. }
        | Outcome::PersonHandedBack { .. }
        | Outcome::PersonAnswered { .. }
        | Outcome::EscalationDecided { .. }
        | Outcome::ProposalDecided { .. }
        | Outcome::Refused(
            Refusal::NotOffered
            | Refusal::NoFurther
            | Refusal::Standing
            | Refusal::SignIn
            | Refusal::Role
            | Refusal::Authority
            | Refusal::Unknown
            | Refusal::Ended
            | Refusal::Limit
            | Refusal::KeyConflict,
        ) => save_answer(domain, env, key, ask, outcome, out),
    }
    for to in replies.into_boxed() {
        out.push(Request::Reply { to, reply: Reply::Outcome(outcome) });
    }
}

fn restore(domain: &mut Domain, env: &Env<Limits>, record: Stored, out: &mut Queue<Request>) {
    match domain.phase {
        Phase::Ready => return restore_failed(domain, record.key(), Refusal::NotReady, out),
        Phase::Failed => return,
        Phase::Restoring => {}
    }
    let key = record.key();
    let valid = match &record {
        Stored::Person { number, identity } => {
            valid_identity(&env.limits, identity)
                && !domain.people.contains_key(number)
                && !domain.identities.contains_key(&identity.key)
                && domain.people.len() < env.limits.people
        }
        Stored::ReadPosition { person, position } => {
            person != &0
                && *position != 0
                && !domain.read_positions.contains_key(person)
                && domain.read_positions.len() < env.limits.people
        }
        Stored::SignIn { number, .. } => {
            !domain.sign_ins.contains_key(number) && domain.sign_ins.len() < env.limits.sign_ins
        }
        Stored::Roles { project, holdings } => {
            valid_roles(domain, &env.limits, *project, holdings).is_ok() && !domain.roles.contains_key(project)
        }
        Stored::Policy { .. } => false,
        Stored::Answer { key, ask, outcome, at } => {
            !watch_ask(ask)
                && valid_ask(&env.limits, ask)
                && valid_answer_shape(ask, *outcome)
                && !domain.answers.contains_key(key)
                && (answer_expiry(*at, &env.limits) <= env.wall || domain.answers.len() < env.limits.requests)
        }
    };
    if !valid {
        return restore_failed(domain, key, Refusal::Limit, out);
    }
    match record {
        Stored::Person { number, identity } => {
            let indexed = domain.identities.insert(identity.key.clone(), number);
            assert!(indexed == Ok(None), "restored identity admitted");
            let saved = domain.people.insert(number, identity);
            assert!(saved.is_ok(), "restored person admitted");
        }
        Stored::ReadPosition { person, position } => {
            let saved = domain.read_positions.insert(person, position);
            assert!(saved == Ok(None), "restored read position admitted");
        }
        Stored::SignIn { number, person, expires } => {
            let saved = domain.sign_ins.insert(number, SignIn { person, expires, due: deadline(env, expires) });
            assert!(saved == Ok(None), "restored sign-in admitted");
        }
        Stored::Roles { project, holdings } => {
            let saved = domain.roles.insert(project, holdings);
            assert!(saved.is_ok(), "restored roles admitted");
        }
        Stored::Policy { .. } => unreachable!("root restores policy into authority"),
        Stored::Answer { key, ask, outcome, at } => {
            if answer_expiry(at, &env.limits) <= env.wall {
                out.push(Request::Erase { key: Key::Answer(key) });
            } else {
                let due = deadline(env, answer_expiry(at, &env.limits));
                let saved = domain.answers.insert(key, Answered { ask: *ask, outcome, at, due });
                assert!(saved.is_ok(), "restored answer admitted");
                let armed = domain.answer_alarms.arm(key, due);
                assert!(armed.is_ok(), "one expiry per restored answer");
            }
        }
    }
}

fn restore_failed(domain: &mut Domain, key: Key, refusal: Refusal, out: &mut Queue<Request>) {
    domain.phase = Phase::Failed;
    out.push(Request::RestoreRefused { key, refusal });
}

#[expect(clippy::too_many_lines, reason = "all restored references are validated before requests open")]
fn restored(domain: &mut Domain, env: &Env<Limits>, out: &mut Queue<Request>) {
    match domain.phase {
        Phase::Ready | Phase::Failed => return,
        Phase::Restoring => {}
    }
    // Restore order is immaterial; validate references once all rows arrived.
    for (number, signin) in &domain.sign_ins {
        if !domain.people.contains_key(&signin.person) {
            return restore_failed(domain, Key::SignIn(*number), Refusal::Unknown, out);
        }
    }
    for (project, holdings) in &domain.roles {
        for holding in &**holdings {
            if !domain.people.contains_key(&holding.person) {
                return restore_failed(domain, Key::Roles(*project), Refusal::Unknown, out);
            }
        }
    }
    for (key, answer) in &domain.answers {
        if !domain.people.contains_key(&key.person) {
            return restore_failed(domain, Key::Answer(*key), Refusal::Unknown, out);
        }
        match answer.outcome {
            Outcome::Watching { .. } => unreachable!("open watches are never stored"),
            Outcome::ServiceMade { person } => {
                let Some(identity) = domain.people.get(&person) else {
                    return restore_failed(domain, Key::Answer(*key), Refusal::Unknown, out);
                };
                if identity.key.provider != domain.deployment_provider {
                    return restore_failed(domain, Key::Answer(*key), Refusal::Unknown, out);
                }
            }
            Outcome::RolesSet { .. } => match &answer.ask {
                Ask::SetRoles { project, holdings } => {
                    if !domain.roles.contains_key(project) {
                        return restore_failed(domain, Key::Answer(*key), Refusal::Unknown, out);
                    }
                    for holding in holdings {
                        if !domain.people.contains_key(&holding.person) {
                            return restore_failed(domain, Key::Answer(*key), Refusal::Unknown, out);
                        }
                    }
                }
                Ask::Watch { .. }
                | Ask::EditNote { .. }
                | Ask::MakeService { .. }
                | Ask::StartChat { .. }
                | Ask::Adopt { .. }
                | Ask::ChangePolicy { .. }
                | Ask::SetPool { .. }
                | Ask::DecideEscalation { .. }
                | Ask::DecideProposal { .. }
                | Ask::Say { .. }
                | Ask::AnswerQuestion { .. }
                | Ask::Prioritise { .. }
                | Ask::Amend { .. }
                | Ask::Move { .. }
                | Ask::TakePerson { .. }
                | Ask::HandBackPerson { .. }
                | Ask::AnswerPerson { .. }
                | Ask::Stop { .. }
                | Ask::Cancel { .. }
                | Ask::Release { .. }
                | Ask::SetGoal { .. } => {
                    unreachable!("restored role success has a matching roster ask");
                }
            },
            Outcome::NoteEdited { .. }
            | Outcome::Adopted { .. }
            | Outcome::Started { .. }
            | Outcome::PolicyChanged { .. }
            | Outcome::PoolSet { .. }
            | Outcome::Said { .. }
            | Outcome::QuestionAnswered { .. }
            | Outcome::Prioritised { .. }
            | Outcome::Amended { .. }
            | Outcome::AmendProposed { .. }
            | Outcome::Moved { .. }
            | Outcome::PersonTaken { .. }
            | Outcome::PersonHandedBack { .. }
            | Outcome::PersonAnswered { .. }
            | Outcome::Stopped { .. }
            | Outcome::Cancelled { .. }
            | Outcome::Released { .. }
            | Outcome::GoalStarted { .. }
            | Outcome::GoalProposed { .. }
            | Outcome::EscalationDecided { .. }
            | Outcome::ProposalDecided { .. }
            | Outcome::Refused(_) => {}
        }
    }
    for (person, _) in &domain.read_positions {
        if !domain.people.contains_key(person) {
            return restore_failed(domain, Key::ReadPosition(*person), Refusal::Unknown, out);
        }
    }
    domain.phase = Phase::Ready;
    // A bounded snapshot lets cleanup and projection mutate the table.
    let mut recovered = List::with_capacity(env.limits.sign_ins);
    for (number, signin) in &domain.sign_ins {
        recovered.push((*number, *signin)).expect("one snapshot row per admitted sign-in");
    }
    for (number, mut signin) in recovered.into_boxed() {
        if signin.expires <= env.wall {
            end_signin(domain, number, out);
        } else {
            signin.due = deadline(env, signin.expires);
            let saved = domain.sign_ins.insert(number, signin);
            assert!(saved.is_ok(), "restored sign-in already exists");
            arm(domain, env, number, signin.expires);
        }
    }
}
