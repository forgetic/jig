//! Typed root/people messages and durable child records (domain/people.md, sections 3–5). The root supplies authenticated identities, roles and keyed
//! requests; people returns bounded routes and persistence intentions. Every
//! routed flight ends with `Decided`; the root commits before releasing replies.
//! These values contain no credential secrets, task internals or protocol bytes.

use alloc::boxed::Box;
use skein_lib::{ReplyTo, Token, Wall};

/// A task-derived recipient, retained only as a bounded inbox reference.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Whom {
    /// One authenticated deployment person.
    Person(u64),
    /// Every current holder of a project role may see the entry.
    Role { project: u32, role: u32 },
}

/// The reason a task currently appears in a person's inbox.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum EntryKind {
    /// A numbered question in a task requested by the person.
    Question { message: u64 },
    /// A pending action waiting for a decision.
    Proposal { number: u64 },
    /// A held task waiting for a decision at this revision.
    Escalation { revision: u64 },
    /// An active person-executed task.
    PersonTask,
    /// A reply in a task requested by the person.
    Reply { message: u64 },
}

/// One volatile reference derived from a live task; content remains in tasks.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Entry {
    pub task: u64,
    pub project: u32,
    pub whom: Whom,
    pub kind: EntryKind,
    /// Root-injected time of the change that made this entry visible.
    pub at: Wall,
}

/// A provider and its stable subject identify one party. Display fields are not keys.
/// The application numbers providers; no credential is retained (domain/people.md, section 3).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct IdentityKey {
    /// The application's configured sign-in provider number.
    pub provider: u16,
    /// The provider's bounded stable identifier, independent of its display login.
    pub subject: Box<[u8]>,
}

/// Authenticated identity and display data, admitted under one identity-byte limit.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Identity {
    /// Authenticated stable provider/subject pair.
    pub key: IdentityKey,
    /// Display login bytes; combined with the subject and name, at most `Limits::identity_bytes`.
    pub login: Box<[u8]>,
    /// Display-name bytes; combined with the subject and login, at most `Limits::identity_bytes`.
    pub name: Box<[u8]>,
}

/// Whether the authenticated party is a person or a deployment service.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Kind {
    /// A person authenticated by an application provider.
    Person,
    /// A service made by a project owner and authenticated by the deployment.
    Service,
}

/// Root-configured first-owner grant used only when this identity's party record is first made;
/// bootstrap matches are bounded. (domain/people.md, section 3).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct InitialOwner {
    /// Project whose roles must already be initialized before first sign-in.
    pub project: u32,
    /// Configured provider/subject pair to grant `Owner` on its first person creation.
    pub identity: IdentityKey,
}

/// People's project membership label; the root translates it to authority policy and performs the
/// actual action checks. (domain/people.md, section 4).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Role {
    /// `Owner` membership; may start chats here, while broader owner actions remain root policy.
    Owner,
    /// Maintainer membership; may start chats here, with authority checked by the root.
    Maintainer,
    /// Member membership; may start chats here within the root's policy and funding check.
    Member,
    /// Observer membership; `StartChat` is refused and its keyed role outcome is saved.
    Observer,
    /// A role numbered by this project's policy, beyond the four defaults.
    Policy { role: u32 },
}

impl Role {
    /// Number used by authority and role-addressed task entries.
    #[must_use]
    pub const fn number(self) -> u32 {
        match self {
            Self::Owner => 0,
            Self::Maintainer => 1,
            Self::Member => 2,
            Self::Observer => 3,
            Self::Policy { role } => role,
        }
    }

    /// Resolve a configured policy role to a membership label.
    #[must_use]
    pub const fn from_number(number: u32) -> Self {
        match number {
            0 => Self::Owner,
            1 => Self::Maintainer,
            2 => Self::Member,
            3 => Self::Observer,
            role => Self::Policy { role },
        }
    }
}

/// One person's authoritative project role; a roles record permits at most one holding per person.
/// (domain/people.md, section 4).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Holding {
    /// Deployment person number, checked for restored references when restoration finishes.
    pub person: u64,
    pub role: Role,
}
/// One connector-known party mapped to a project role at adoption.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Seed {
    pub identity: IdentityKey,
    pub candidate: u64,
    pub role: Role,
}

/// One derived unread result reference; its words remain in the ended task.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ResultRef {
    pub task: u64,
    /// Root-issued order in which task results committed.
    pub position: u64,
}

/// A connector's resource path, opaque to people (domain/connectors.md, section 12).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ResourceName {
    pub connector: u16,
    pub path: Box<[Box<[u8]>]>,
}

/// A resource and its connector-specific options, opaque to people. The whole
/// path and options payload fits `Limits::amendment_bytes` at admission.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Adoption {
    pub resource: ResourceName,
    pub role: ResourceRole,
    /// Bounded connector-specific fields, interpreted by the application's root.
    pub options: Box<[Box<[u8]>]>,
}

/// How an adopted resource participates in the project.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ResourceRole {
    Owned,
    Fork,
    Context,
}

/// Note scope named by a party; the parent translates it to the notes child.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum NoteScope {
    /// Shared deployment guidance.
    Deployment,
    /// Guidance for one project.
    Project,
    /// Guidance for one goal in the request's project.
    Goal { goal: u64 },
    /// Guidance for one connector resource pattern in the request's project.
    Resources { connector: u16, pattern: crate::Pattern },
}

/// A person's correction or deletion of a recalled note revision.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum NoteChange {
    /// Replace an entry's bounded description, body and task references.
    Correct { description: Box<[u8]>, body: Box<[u8]>, references: Box<[u64]>, recalled: u32 },
    /// Delete the entry at the revision the person saw.
    Delete { recalled: u32 },
}

/// A live view's subject, named without depending on the sibling views child.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum WatchSubject {
    /// One run attempt's stream.
    Run { task: u64, attempt: u64 },
    /// One task and its delegate tree.
    Tree { task: u64 },
    /// Current goals of one project.
    Goals,
    /// The requesting party's inbox.
    Inbox { party: u64 },
}

/// Authenticated keyed requests admitted by the people child and routed by
/// the root after role checks (domain/people.md, section 5.1).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Ask {
    /// Open one role-checked live view, retained only while the view is open.
    Watch { project: u32, subject: WatchSubject },
    /// Correct or delete a note whose scope the party's project role permits.
    EditNote { project: u32, name: u64, scope: Box<NoteScope>, change: Box<NoteChange> },
    /// Make a deployment service in a project with a role and display name.
    MakeService { project: u32, name: Box<[u8]>, role: Role },
    /// Adopt a connector resource as a durable keyed owner request.
    Adopt { project: u32, adoption: Adoption },
    /// Start a tracked goal at the requested charter, budget and priority.
    SetGoal { project: u32, spec: Box<[u8]>, charter: u32, budget: u64, priority: u32 },
    /// Hold a live run for this person's decision, cancelling its current claim.
    Stop { project: u32, task: u64 },
    /// Cancel a task and its descendants with a bounded reason.
    Cancel { project: u32, task: u64, reason: Box<[u8]> },
    /// Lift a held task's cause and rejudge it.
    Release { project: u32, task: u64 },
    /// Claim one role-addressed person task for this authenticated person.
    TakePerson { project: u32, task: u64 },
    /// Give this person's current role task claim back to its role.
    HandBackPerson { project: u32, task: u64 },
    /// Supply the bounded result required by an addressed person task.
    AnswerPerson { project: u32, task: u64, result: PersonResult },
    /// Adopt a live task as this authenticated person's own goal, retaining its old requester's reference.
    Move { project: u32, task: u64, reason: Box<[u8]> },
    /// Authenticated keyed decision for a proposal waiting at this person or a policy role.
    DecideProposal { project: u32, proposer: u64, proposal: u64, decision: ProposalDecision },
    /// Authenticated requester's whole words to an existing chat.
    Say { project: u32, task: u64, words: Box<[u8]> },
    /// Answer a numbered question waiting in a task requested by this person.
    AnswerQuestion { project: u32, task: u64, question: u64, words: Box<[u8]> },
    /// Set the priorities of a bounded set of current project goals together.
    Prioritise { project: u32, goals: Box<[(u64, u32)]> },
    /// Amend a live task this person may steer, after root policy and funding checks.
    Amend { project: u32, task: u64, amendment: crate::Amendment },
    /// Authenticated Owner's complete project roster replacement; root checks Policy permission and
    /// held-recipient preflight before applying it.
    SetRoles {
        /// Existing configured project; unknown projects refuse without mutation.
        project: u32,
        /// At most `Limits::holdings` unique positive existing people; validated by this child
        /// before replacement.
        holdings: Box<[Holding]>,
    },
    /// Owner changes one mutable part of the project policy for future decisions.
    ChangePolicy { project: u32, change: crate::PolicyChange },
    /// Owner changes one person's current-period pool, preserving already spent and reserved funds.
    SetPool { project: u32, person: u64, budget: u64 },
    /// Decide one held chat's exact semantic revision. Root verifies current waiting recipient and
    /// authority; admission authenticates the session and reserves keyed-answer room.
    DecideEscalation {
        /// Actual policy project, checked against the held task by root.
        project: u32,
        /// Positive held chat identity.
        task: u64,
        /// Positive task-local waiting revision, never a transport key.
        revision: u64,
        /// One bounded semantic choice; root authorizes before mutation.
        decision: EscalationDecision,
    },
    /// Request a chat task; admitted member-or-higher asks route once per key to the root.
    StartChat {
        /** Project in which the root should create the chat task. */
        project: u32,
        /** Opening words, bounded by `Limits::words` before any route or saved answer. */
        words: Box<[u8]>,
    },
}

/// A person's answer to a report, choice or failure contract; root translates it for tasks.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum PersonResult {
    /// Freeform words for a report contract.
    Report { words: Box<[u8]> },
    /// One named choice with optional explanation.
    Verdict { code: u32, words: Box<[u8]> },
    /// The person could not complete the task.
    Failure { reason: Box<[u8]> },
}

/// Keyed person's choice for a pending proposal.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum ProposalDecision {
    Accept,
    Reject { reason: Box<[u8]> },
    Pass,
}

/// Committed semantic proposal choice in a keyed person answer.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ProposalChoice {
    Accepted,
    Rejected,
    Passed,
    Withdrawn,
    Stale,
}

/// Authenticated person's held-chat choice. Rejection words are bounded by
/// people words; root also checks child/journal bounds (domain/people.md, section 5.1).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum EscalationDecision {
    /// Lift the held chat's cause and rejudge it under current conditions.
    Release,
    /// Decide once while preserving the hold.
    Reject {
        /// Bounded reason saved with the ask and held task.
        reason: Box<[u8]>,
    },
    /// Move from requester to final policy role; no further pass from that role.
    Pass,
}

/// Semantic kind of a committed decision, returned to a caller or historical
/// loser of a race; carries no second task state (domain/people.md, section 5.1).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum EscalationChoice {
    /// Hold released.
    Released,
    /// Task remains held with rejection reason.
    Rejected,
    /// Waiting moved to policy role.
    Passed,
}

/// Admission or root-decision reason; `Busy`/`NotReady` decisions release the flight without saving a
/// completed key, while other decided refusals are retained. (domain/people.md, section 5.1).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Refusal {
    /// The checked request awaits the core route provided by a later session.
    NotOffered,
    /// Final policy role cannot pass further; no state mutation.
    NoFurther,
    /// Caller is not the current waiting recipient.
    Standing,
    /// Restoration is unfinished/failed, or the root reports transient admission unavailability.
    NotReady,
    /// Sign-in is missing, expired or incompatible with the supplied identity.
    SignIn,
    /// Project membership does not permit the supported request.
    Role,
    /// Root reports authority insufficient for the action.
    Authority,
    /// Root names an unknown target, bootstrap lacks its initialized project, or restore references
    /// an unknown person.
    Unknown,
    /// Root reports the requested target has ended.
    Ended,
    /// Configured capacity is unavailable or the root reports transient admission pressure; a newly
    /// refused admission may retry.
    Busy,
    /// Payload, configuration-derived arithmetic or restored record validity exceeds the supported
    /// bound.
    Limit,
    /// Same person-scoped key names a different typed request.
    KeyConflict,
}

/// Root's terminal result for one routed flight; people replies to each bounded waiter and saves
/// nontransient outcomes with the parent's task decision. (domain/people.md, section 5.1).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Outcome {
    /// One live watch was opened; it writes no durable answer record.
    Watching { watcher: Token },
    /// A note correction or deletion was committed with its keyed answer.
    NoteEdited { name: u64 },
    /// A service party and its project role were committed together.
    ServiceMade { person: u64 },
    /// Resource adoption and its connector-known party seed committed together.
    Adopted { project: u32 },
    /// One tracked goal was durably created within the caller's allotment.
    GoalStarted { task: u64 },
    /// A tracked goal awaits the policy role's budget decision.
    GoalProposed { proposal: u64 },
    /// A person stopped one live task for a later release or decision.
    Stopped { task: u64 },
    /// A person cancelled one task and its descendants.
    Cancelled { task: u64 },
    /// A person released a held task.
    Released { task: u64 },
    /// One role-addressed task was durably claimed.
    PersonTaken { task: u64 },
    /// The claimant durably returned the task to its role.
    PersonHandedBack { task: u64 },
    /// An addressed person's result was durably admitted.
    PersonAnswered { task: u64 },
    /// The task now belongs to the requesting person, with its remaining funding transferred.
    Moved { task: u64 },
    /// Keyed answer to one current or historical proposal decision.
    ProposalDecided { proposer: u64, proposal: u64, by: u64, choice: ProposalChoice },
    /// Words were durably admitted under the root-issued message number.
    Said { task: u64, message: u64 },
    /// A named question was answered with a durable message.
    QuestionAnswered { task: u64, question: u64, message: u64 },
    /// All named goal priorities were durably changed together.
    Prioritised { project: u32 },
    /// The named task accepted this person's amendment.
    Amended { task: u64 },
    /// A widening amendment waits for an authorized proposal holder.
    AmendProposed { task: u64, proposal: u64 },
    /// Root accepted one roster and all affected `Waiting` recipients atomically; saved keyed
    /// replay does not apply the roster again.
    RolesSet {
        /// Project whose membership changed; no task authority or funding changed.
        project: u32,
    },
    /// The full committed policy now applies to later checks; existing grants remain.
    PolicyChanged { project: u32 },
    /// One person's finite pool was opened or resized in its original period.
    PoolSet { project: u32, person: u64 },
    /// Root's committed current or historical decision for one task revision; first commit decides
    /// a race.
    EscalationDecided {
        /// Named held chat.
        task: u64,
        revision: u64,
        /// Authenticated winning person, including historical races.
        by: u64,
        choice: EscalationChoice,
    },
    /// Root created the chat task; save the keyed answer with the task in one decision.
    Started {
        /** Chat task number returned by the root; the parent withholds the reply until durability and people holds no task state. */
        task: u64,
    },
    /// Request ended with a reason; permanent decisions are keyed, `Busy`/`NotReady` remain
    /// retryable.
    Refused(/** Root or role refusal; transient `Busy`/`NotReady` is not retained as a completed key. */ Refusal),
}

/// Child-to-client answer routed by the parent; state-changing answers are withheld externally
/// until their decision is durable. (domain/people.md, sections 3 and 5.1).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Reply {
    /// Sign-in succeeded or an existing active same-person sign-in was replayed.
    SignedIn {
        /** Stable deployment party number for the authenticated identity. */
        person: u64,
        /** Saved wall-time expiry for this sign-in; its monotonic deadline is retained internally. */
        expires: Wall,
    },
    /// Sign-out completed; an already absent number is also successful once ready.
    SignedOut,
    /// One terminal response for a keyed call or its duplicate waiter.
    Outcome(/** Terminal keyed result, or retryable root admission pressure for the current call. */ Outcome),
    /// Entrance refused before making a new answered-key record.
    Refused(/** Direct entrance refusal with no newly saved answered-key record. */ Refusal),
}

/// Person-scoped idempotency key shared across that person's sign-ins; a different ask under the
/// same key is a conflict. (domain/people.md, section 5.1).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct RequestKey {
    pub person: u64,
    /// Opaque web-selected 16-byte request key; the domain does not parse it.
    pub key: [u8; 16],
}

/// Typed logical store key for a people record; the protocol handles bytes and secrets outside this
/// domain. (domain/people.md, sections 3–5).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Key {
    /// Logical person-record key.
    Person(u64),
    /// One person's durable position in committed result order.
    ReadPosition(u64),
    /// Logical secret-free sign-in-record key.
    SignIn(/** Root-issued deployment sign-in number; cookie secrets and token digests stay below the domain. */ u64),
    /// Logical per-project roles-record key.
    Roles(/** Project whose authoritative holdings are stored. */ u32),
    /// Root-owned current project policy, paged with people records.
    Policy(u32),
    /// Logical completed-key record.
    Answer(RequestKey),
}

/// Owned typed durable record submitted to the parent or restored from it; capacities and payload
/// bytes are checked when admitted. (domain/people.md, sections 3–5).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Stored {
    /// Persistent party identity; provider/subject and party numbers must be unique on restore.
    Person {
        /** Stable person number, unique among restored people. */
        number: u64,
        /** Unique provider/subject identity and bounded display bytes. */
        identity: Identity,
    },
    /// Monotonic read position for the named person; no result is copied here.
    ReadPosition { person: u64, position: u64 },
    /// Persistent secret-free sign-in; expired restored entries are erased when restore completes.
    SignIn {
        /** Root-issued sign-in number; at most `Limits::sign_ins` sign-in records are retained. */
        number: u64,
        /** Existing person reference, validated after all restore rows arrive. */
        person: u64,
        /** Persisted wall-time expiry; restoration projects it onto the supplied startup monotonic clock. */
        expires: Wall,
    },
    /// Persistent complete project holdings with bounded unique people.
    Roles {
        /** Project whose live role set this record replaces or restores. */
        project: u32,
        /** At most `Limits::holdings` distinct people; restored references are checked at completion. */
        holdings: Box<[Holding]>,
    },
    /// Root-owned full mutable project policy; the people page carries it to authority.
    Policy { project: u32, value: crate::PolicyValue },
    /// Persistent request and outcome; same-key same-ask replay returns it without checking current
    /// roles anew.
    Answer {
        /** Person-scoped completed key; restoration validates its person reference. */
        key: RequestKey,
        /** Original typed request with words bounded by `Limits::words`, compared on every retry. */
        ask: Box<Ask>,
        /** Retained permanent outcome returned without remaking the decision. */
        outcome: Outcome,
        /** Wall time the answer was made; completed keys are retained for `Limits::request_retention`. */
        at: Wall,
    },
}

impl Stored {
    /// Pure projection of this typed record's logical key; allocates nothing, emits no request and
    /// changes no state.
    #[must_use]
    pub const fn key(&self) -> Key {
        match self {
            Stored::Person { number, .. } => Key::Person(*number),
            Stored::ReadPosition { person, .. } => Key::ReadPosition(*person),
            Stored::SignIn { number, .. } => Key::SignIn(*number),
            Stored::Roles { project, .. } => Key::Roles(*project),
            Stored::Policy { project, .. } => Key::Policy(*project),
            Stored::Answer { key, .. } => Key::Answer(*key),
        }
    }
}

/// Root-to-child inputs; authenticated identity, authoritative roles and typed restored records
/// cross this boundary without protocol secrets. (domain/people.md, sections 3–5).
#[derive(PartialEq, Eq, Debug)]
pub enum Event {
    /// Forget a volatile watch key when its stream closes.
    WatchClosed { watcher: Token },
    /// Apply an authorized owner request to make a service with a fresh party number.
    MakeService { request: Token, person: u64 },
    /// Add previously unknown collaborators and their first project role in one decision.
    Seed { project: u32, collaborators: Box<[Seed]> },
    /// Replace the volatile inbox projection of one durable task after its row changes.
    Waiting { task: u64, entries: Box<[Entry]> },
    /// Root's synchronous application after authority/revision/capacity preflight. Reads the
    /// original admitted roster, preserves membership on refusal and returns exactly one
    /// `RolesApplied`.
    ApplyRoles {
        /// Root's stage-local reply right, consumed by `RolesApplied`.
        reply_to: ReplyTo,
        /// Live `SetRoles` flight from `Route`; no synthetic or completed token is authorized.
        request: Token,
    },
    /// Root supplies a fresh person candidate and a fresh sign-in number. Only a new identity uses
    /// the person candidate; existing identities keep their durable person number, and unused
    /// person candidates leave gaps. Admit an authenticated sign-in after restoration; bootstrap
    /// first-owner, person and sign-in writes share one root decision.
    SignedIn {
        /// Opaque destination for the sign-in answer; the parent holds success until durability.
        reply_to: ReplyTo,
        /// Root-issued never-reused fresh candidate, used only for a new provider/subject identity;
        /// unused candidates leave gaps.
        person: u64,
        /// Root-issued fresh deployment sign-in number; replaying an active same-person number
        /// returns its existing expiry. Cookie secrets stay below the domain.
        sign_in: u64,
        /// Protocol-authenticated identity with bounded display bytes and no credential.
        identity: Identity,
        /// The protocol-authenticated party kind; a service must already exist.
        kind: Kind,
    },
    /// End one sign-in after restoration, emitting erase if present and one reply.
    SignOut {
        /// Destination for the terminal `SignedOut` reply.
        reply_to: ReplyTo,
        /// Root-issued sign-in number to end; absence is an idempotent success once ready.
        sign_in: u64,
    },
    /// Apply an already authorized root roles replacement; invalid/full updates emit `RolesRefused`
    /// and preserve old holdings. Failed restoration ignores this event.
    Roles {
        project: u32,
        /// Complete bounded holdings, with no duplicate person; root already authorized this
        /// change.
        holdings: Box<[Holding]>,
    },
    /// Admit one keyed call after restoration; saved retries reply, pending duplicates join bounded
    /// waiters, and a new eligible flight routes once.
    Ask {
        /// Destination admitted into at most `Limits::waiters` replies per flight.
        reply_to: ReplyTo,
        /// Root-issued active authenticated sign-in number; wall or monotonic expiry refuses
        /// entrance.
        sign_in: u64,
        /// Opaque web key, scoped to the sign-in's stable person.
        key: [u8; 16],
        /// Typed request with payload bounds checked before lookup, route or mutation.
        ask: Ask,
    },
    /// Complete a live `Route` exactly once, retire its flight and answer each waiter; permanent
    /// answer writes join the root's action commit.
    Decided {
        /// Opaque token from `Route`; the root must return this live flight exactly once, or the
        /// child panics.
        request: Token,
        /// Terminal root decision; persist permanent answers with task changes, but do not retain
        /// `Busy`/`NotReady` pressure.
        outcome: Outcome,
    },
    /// Load one saved row while restoring; invalid input emits one `RestoreRefused` and permanently
    /// fails this instance.
    Restore {
        /// One typed saved row, admitted once during restoring; duplicate/oversized rows fail
        /// restoration.
        record: Stored,
    },
    /// Finish restoration, validate all person references and reproject sign-in deadlines; erase
    /// expired entries. Repeating after ready/failed is ignored.
    Restored,
}

/// Child-to-root outputs; persistence joins the root's decision, and the root withholds external
/// replies until required writes are durable. (domain/people.md, section 5.1).
#[derive(PartialEq, Eq, Debug)]
pub enum Request {
    /// Terminal for one service creation after its person and role saves.
    ServiceMade { request: Token, outcome: Outcome },
    /// Terminal for `ApplyRoles`; success `Save` precedes this output, but the root completes the
    /// keyed flight only after task recheck.
    RolesApplied {
        /// Echoed root stage-local right, consumed exactly once.
        reply_to: ReplyTo,
        /// Echoed admitted flight; root returns `Decided` after the whole decision.
        request: Token,
        /// Success, or readiness/identity/roster/project/standing refusal before mutation; bounds
        /// use `Limits::holdings`.
        result: Result<(), Refusal>,
    },
    /// New eligible keyed flight for the root's authority/task decision; duplicate callers share
    /// this route.
    Route {
        /** Opaque live flight token; root owes exactly one matching `Decided`, including admission pressure. */
        request: Token,

        person: u64,
        /** Project selected by the typed request. */
        project: u32,
        /** Current membership at admission: Some is required for chat; an escalation decision may carry None so root can check named-person standing. Root still checks authority. */
        role: Option<Role>,
        /** Original bounded request, owned by the root for routing and atomic decision. */
        ask: Box<Ask>,
    },
    /// Terminal client output; the parent enforces durability before exposure.
    Reply {
        /** Opaque client destination; each admitted call or waiter receives one terminal reply. */
        to: ReplyTo,
        /** Answer to deliver only after any state-changing decision is durable. */
        reply: Reply,
    },
    /// Typed persistence output to join the parent's atomic decision, including task creation and
    /// keyed answer together.
    Save {
        /** Owned replacement row for the parent's current atomic decision, not a standalone IO submission. */
        record: Stored,
    },
    /// Typed removal output for an ended or expired sign-in, with durability handled by the parent.
    Erase {
        /** Logical row to remove with the parent's decision; secret cleanup remains below the domain. */
        key: Key,
    },
    /// Terminal role-update refusal with no role replacement.
    RolesRefused {
        /** Project whose role update was refused without replacing holdings. */
        project: u32,
        /** Bound or capacity reason for the refused role update. */
        refusal: Refusal,
    },
    /// Terminal restoration failure; a failed instance cannot become ready through more rows or
    /// `Restored`.
    RestoreRefused {
        /** Record key that made restoration fail. */
        key: Key,
        /** Restore admission/reference reason; this instance stays unready. */
        refusal: Refusal,
    },
}
