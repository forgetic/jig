# The core

Provisional, 2026-10-07. What jig's core is, as a domain layer inside an
application's engine: its parts and how they fit, what crosses between
it and the application's root, how a decision reaches the store before
anything leaves the engine, how it restarts, how it runs agents, and the
capabilities it keeps (the fleet, briefs, notes, views, accounts). The
model is core.md; tasks are tasks.md, authority authority.md, connectors
connectors.md, parties people.md, hosts hosts.md; what the application's
root does with all of it is root.md. The mechanics are those of skein's
`programming-model.md`. What is still open is listed in section 16.

## 1. In one page

- **The core is where work is decided.** It keeps the tasks, checks
  every action against authority, starts agents' runs on hosts, asks
  parties, and tells the application's connectors what to make, judge,
  step, project and release. It is a child of the application's root,
  and the parent of its own children.
- **A parent and its children.** The tasks child domain is the hub;
  authority is policy; the fleet, people, accounts, briefs, notes and
  views are capabilities. The core routes between them and faces the
  root (section 3).
- **It speaks one vocabulary to the root** (section 4): the store's
  records, the hosts', the parties', the accounts', and the connectors'
  in jig's terms. It knows connectors by number.
- **A decision commits once,** and nothing leaves before it does
  (section 5). The core says, of every output, whether it waits for the
  decision's commit; the root's journal holds it until it is durable.
- **A restart is the core's script** (section 6): its own records, then
  each connector's, then the runs adopted, the connectors reading afresh
  and settling their outboxes, and only then decisions.
- **Agents run from their task** (section 7). A run is given its task's
  charter, a brief built for why it runs, its transcript if it resumes,
  and the engine's tools its authority allows. Each turn it takes is
  committed as it ends; every call that changed something is committed
  and answered once, however often it is asked.
- **Bounded.** The core holds live work only: live tasks, their inboxes
  and proposals, the runs in flight. Ended tasks, transcripts, history
  and notes are in the store, loaded when wanted.
- **The domain is complete** (programming-model.md, section 4): a world
  of domains and fakes runs everything the core does, with no protocol
  and no io (section 15).

## 2. The core in the engine

```
application root     the journal, routing, translation; faces the protocol layer
├── core             tasks, authority, people, fleet, accounts, brief, notes, views
├── <connectors>     the application's
├── local host       jig's host inside the engine, if the application runs agents there (hosts.md)
└── …                the application's own children
```

- **The root faces the protocol layer,** and the core faces the root.
  Everything the core hears arrives through the root, already in the
  core's vocabulary; everything it asks for leaves through the root,
  which translates it and puts it into the journal (root.md).
- **Hosts are reached through the root:** workers over the channel, which
  the protocol layer carries; the host inside the engine as a sibling the
  root routes to (hosts.md, section 5). The core's fleet does not tell
  them apart beyond what placement needs.
- **The engine never reads a system** but through its connectors; what a
  workspace holds is for the run to find.
- **The store is the engine's alone.** Nothing else reads or writes it,
  so the engine needs no compare-and-swap against anyone: its own
  ordering of commits is the truth.

## 3. Structure

```
jig-core                    the core: routing among its children, runs, calls, the restart script, numbers
├── jig-core-tasks          the hub: tasks, batches, lifecycle, holds, inboxes, wakes, proposals
├── jig-core-authority      authority's order and checks; the rules and the projects' policies
├── jig-core-people         parties: identities, sign-ins, roles, requests, inboxes
├── jig-core-fleet          hosts: slots, placement, attempts, relaying
├── jig-core-accounts       LLM accounts: refreshing, granting
├── jig-core-brief          a run's brief, from typed sections and connectors' sized ones, within a budget
├── jig-core-notes          notes: scopes, entries, indexes
└── jig-core-views          live streams out
```

The tree follows programming-model.md, 4.5: each child domain has its
own vocabulary, limits and world; a parent owns its children's state and
routes between them; siblings share no domain types, and the core
translates between them with small total functions.

- **`tasks` is the hub:** it knows a task's lifecycle and nothing of
  runs, connectors or clients.
- **`authority` is policy:** decisions over data, keeping only the rules
  and policies in force.
- **The others are capabilities,** each knowing its own concern: the
  fleet the hosts, people the parties, accounts the LLM credentials, the
  brief a run's context, notes what agents learn, views who watches.
- **The core keeps little of its own:**
  - the deployment's numbers: the next task number and every other number
    the core gives, committed as they are used;
  - the runs in flight: for each, its task's number, its attempt, and
    what its assignment was made from, so an answer can be routed and a
    brief rebuilt (section 7);
  - the calls committed for each live task since its last committed turn,
    by attempt and name, with the core's part of their answers (7.3);
  - the decisions in progress, between the hand-offs the root completes
    within one step (4.3);
  - the restart script's state (section 6).

## 4. Its vocabulary toward the root

### 4.1 What it hears

- **from the store,** through the root: the answers to its loads, in
  pages; the records restored at a restart; that the loading it asked for
  is done. It does not hear commits' answers: the journal does (root.md,
  section 4), and feeds back what the core asked to resume after one;
- **from hosts:** a host's hello with its slots and what it hosts; turns,
  calls, answers and facts of runs; a host lost or back;
- **from parties:** sign-ins and their ends, keyed requests, reads of
  their inbox and of history;
- **from accounts' protocol:** a refresh's outcome;
- **from connectors,** each by number, in jig's terms (connectors.md,
  section 2): resources' roles and kinds of hold; pools' slots; effects'
  descriptions; verdicts; procedures' step decisions; outbox outcomes;
  classified news for subscribers; sections and workspace items ready,
  with their sizes; adoptions; drift; a restart step done;
- **from itself:** an output it asked to resume once released (4.2), and
  its children's timers, through `fire`.

### 4.2 What it asks for

Each request the core makes says which of three it is:

- **part of the decision:** a record saved or erased, which goes into
  the decision's commit; and the hand-offs the root completes before the
  step returns: describe this effect, judge this requirement, keep or
  drop this effect, take this procedure's parameters, gather or cut this
  section;
- **held:** what must wait for the decision's commit to be durable: an
  assignment, a message relayed to a run, a cancel, an acknowledgement,
  an answer to a call or to a party, a load (so that it sees every commit
  before it), an outbox entry to make, a resumption of the core's own;
- **now:** what decides nothing: facts to watchers, the answer to a read,
  a host's refusal before anything was admitted.

The root keeps each as the core said (root.md, section 3); the journal
releases held ones in order, and the conformance world checks that
nothing held left early (testing.md, section 5).

### 4.3 Decisions and hand-offs

A decision is a route through several children and connectors, completed
within one step of the root's (programming-model.md, 4.5):

- **One event in,** from the root.
- **Hand-offs out and back:** the core asks the root for what only a
  connector can say (a description, a verdict, a section's size), and
  the root asks the connector and gives the answer back to the core,
  before its step returns. Hand-offs are bounded per decision and
  acyclic: the core never asks a connector something whose answer needs
  another of the core's answers.
- **One end:** the core says the decision is complete, and the root
  accepts it into the journal (root.md, section 3).

What must first wait for a load, a read or a host is a state of its own
(a task preparing, a brief gathering), and its decision is made in the
step its last answer arrives.

### 4.4 The usual routes

| What arrives | Route | Then |
|---|---|---|
| a run's turn | fleet (fenced) → tasks (the inbox taken) → authority (spend) → views | commit; the turn acknowledged |
| a run's call to an engine tool | fleet → authority (the check) → tasks or notes | commit; the call answered |
| a run's call for an effect | the connector describes it → authority, its judges asked → the connector keeps it | commit; answered when made, or past its deadline |
| a run's call for a read | fleet → authority (the grant) → the connector serves it | answered now; nothing committed |
| a run's answer | fleet → tasks (result checked, task closing or idle) → authority (spend); what it left → its connectors | commit; acknowledged; effects made |
| a procedure's step | its connector → authority (each effect, its judges; each batch) → tasks | commit; effects made; tasks started |
| a connector's news | classified by the connector → tasks (inboxes, wakes) | commit; due tasks run, procedures step |
| an outbox outcome | its connector → tasks (settled; closing advances) | commit |
| a party's request | people → authority (their role) → tasks | commit; the party answered |
| a task due | tasks → authority (the run may start) → brief, with its connectors gathering, and its transcript loading → tasks (claim) → fleet (place) | commit the claim; assign |
| a task closing | tasks → its connectors (release) | commit; releases made |
| a goal changing | tasks → its projecting connectors | commit; projections made |
| a timer | its child | as its child says |

## 5. Decisions and the store

### 5.1 Commits

- **One decision, one commit.** What a decision changes in every child
  and connector it touched is one commit: records written or removed, as
  one.
- **Numbered and in order.** The journal numbers commits and the store
  applies them in that order. Its answer is that a commit is durable,
  which makes every earlier one durable too.
- **The state runs ahead.** A child's state changes as the decision is
  made, not when it commits, so the next decision sees it.
- **Commits in flight are bounded,** and the store's answers are always
  taken, so the bound never stops the answer that frees it. Past the
  bound, a decision that would need a commit is not taken, and what asked
  for it is told so: a party's request is answered busy; a run's tool
  call is answered busy, and the run asks it again after a backoff
  (smith's `run.md`, 5.2); a turn or an answer is answered busy, and its
  host sends it again after a backoff; a connector keeps its events and
  its outbox's outcomes until there is room. The journal says whether
  there is room before any child changes (root.md, section 4).
- **A failed commit stops the engine** (core.md, section 4). Nothing
  waiting for that commit, or any later one, goes out. A restart starts
  from the last durable commit.

### 5.2 What waits for a commit

Every output that someone outside the engine could act on waits for the
commit it follows from:

- **effects:** an outbox entry is handed to its connector to make
  (connectors.md, section 4);
- **assignments:** a run is assigned only once its claim is durable, so
  an attempt never runs that a restart would not know of;
- **acknowledgements:** a host's answer, and a run's turn, are
  acknowledged once committed; until then the host keeps them;
- **answers to tool calls** that changed state, and to parties'
  requests;
- **messages relayed to a live run,** so a run never reads what a
  restart would not have;
- **loads,** so a load sees every commit before it.

What decides nothing waits for nothing: facts streamed to watchers
(section 11), and the answers to a run's reads.

### 5.3 Loads

- **What is loaded, not held:** ended tasks and their results;
  transcripts; history; notes' entries; anything a client pages through.
- **A load is a request** with a token and one terminal event, rows in
  bounded pages (programming-model.md, 4.4). What waits on it is in a
  state that says so: a brief gathering, a run being prepared to resume,
  a party's page.
- **Bounded:** the loads in flight, and the bytes a load brings, are
  limits; a load past its bytes is cut, and says how much.

### 5.4 What the store keeps of the core

| Record | Written by | Loaded |
|---|---|---|
| the deployment: its id, made at its first start, and its numbers | the core | at start |
| projects: resources and roles, homes, policy, parties' roles | people | at start |
| parties: identities, sign-ins | people | those with a live sign-in or a role, at start; others as they sign in |
| parties' requests' keys, with their answers, for a retention | people | at start, those within it |
| tasks: every field of tasks.md, section 3, holds and their queues, writer slots, the last turn committed for a live attempt | tasks | live ones at start; ended ones on demand |
| calls committed since each live task's last turn, by attempt and name, with the core's part of their answers | the core | with their task |
| funders' numbers: each party's pool and each project's spend, per period | tasks | at start |
| a task's history: made, amended, held, released, cancelled, by whom, why | tasks | on demand |
| inboxes: messages not yet taken | tasks | with their task |
| proposals: pending, and decided ones' outcome | tasks | pending ones at start |
| transcripts: turns, with their spend | the core | to resume; on demand |
| notes | notes | indexes per scope in use; entries on demand |
| secrets: LLM accounts' refresh tokens, sign-ins' tokens or their digests | the protocol layer, never the domain, which names them by account and generation, or by a sign-in's number | at start, by the protocol layer |

Each connector keeps its own records beside these (connectors.md,
section 2): its outbox, procedures' states and parameters, what it made
by key, its projections' states, and its own state. The application's
root wraps both in its store vocabulary (root.md, section 6).

**Times** the store keeps are wall times (`env.wall`), so a record means
the same after a restart; a wait the core arms is a deadline on its
monotonic clock (`env.now`), computed from them (programming-model.md,
section 9).

### 5.5 What the core asks of the store

The store is assumed (core.md, section 2); this is what the core relies
on, and all it relies on:

- a commit of several records is applied whole or not at all;
- commits are applied in the order sent, and durable once answered;
- a load sees every commit answered before it was sent;
- records are keyed by the domain's own keys, and listed by key ranges,
  in bounded pages; what is live (live tasks, pending proposals, the
  outboxes, live resources) is kept in key ranges of its own, so a
  restart reads it without reading history;
- what one engine wrote, the next reads: records carry the version of
  their shape;
- nothing else writes it.

### 5.6 How a child's records reach a commit

Each child that keeps durable state, and each connector, defines its own
records and ordered keys in its own vocabulary. Neither a sibling nor
the store chooses their shapes. The parent translates their keys and
carries their records in its own store vocabulary; the store's protocol
layer encodes each shape with a version (5.5).

- **`Save { record }`** keeps the record under its own key, and
  **`Erase { key }`** removes that key. These requests are part of the
  decision the parent is making. They have no terminal event and do not
  ask the child to wait: its state changes as it decides (5.1).
- **`Restore { record }`** supplies one record at restart, and
  **`Restored`** says every record in the child's live ranges has been
  supplied. The child then decides from that restored state.
- **`Load { owner, range }`** asks for one bounded page of the child's
  own key range. Its one terminal event is **`Loaded { owner, rows,
  more }`**. The token is echoed, and `more` says another page remains,
  which the child asks for with a load of its own.

A child never waits for a commit and never holds an output back for one.
Its outputs leave it as it decides, each saying which of the three of
4.2 it is, and its parent passes them up. A child's world plays its
parent and the store: it keeps saved records, applies erases, commits at
the end of each step, and restarts by feeding the durable records as
`Restore` followed by `Restored`. Restart is therefore tested before the
root is built.

## 6. Restart

A restart is a cold start that rebuilds everything a decision reads from
the store and the connectors, and makes nothing twice. The core runs it
as a script: it asks the root for one step at a time, and asks for the
next only once the root says this one is done (root.md, section 8).

1. **The core's records:** the deployment's numbers; projects, their
   policies and parties' roles; live tasks with their inboxes,
   subscriptions, holds and queues; pending proposals; the calls
   committed since each live task's last turn; the funders' numbers; LLM
   accounts. The core's limits bound what is live (tasks.md, section 11),
   so what is loaded fits; a live set past limits lowered since refuses
   the start, saying which.
2. **Each connector's records,** connector by connector: the root loads
   that connector's live ranges and restores them into it
   (connectors.md, section 14).
3. **The runs adopted.** Every task claimed or running names its attempt
   and the kind of host it was placed on. Claims placed in the engine
   died with it, and are settled at once as lost, or as not started if
   they committed nothing (tasks.md, 5.2). Claims on workers reach the
   fleet, which adopts each attempt a worker reports as it dials in,
   keeps waiting for one within its grace, and cancels any it does not
   name (section 8). Nothing new is assigned before every claim loaded has
   reached the fleet.
4. **Read afresh.** Each connector reads, for the live tasks that name
   them, the resources its system owns.
5. **Settle the outboxes.** Each connector resolves every entry not
   committed as made, as its recovery class says (connectors.md, 4.3):
   looked for by its key or the state it leads to, written again if it
   is a set; one not found waits for the retry deadline committed with
   its last attempt, which the restart keeps; one of a kind that cannot
   be recovered holds its task for a person.
6. **Decide afresh.** Every procedure steps once on the facts read;
   every agent task due gets a run; every inbox's messages are relayed or
   wake as their policy says.

Workers keep the answers and turns they were not acknowledged for, and
send them again after their hello; the core commits any it does not
have, and acknowledges each.

## 7. Agent executors

### 7.1 From a task to a run

When an agent task is due (tasks.md, 5.2), the core:

1. **Checks** that the run may start (authority.md, 8.3).
2. **Prepares it,** the task preparing over as many steps as it takes:
   gathers its brief (section 9), for why it runs, its first activation
   or the messages that woke it; has each connector that owns a resource
   the task names gather its workspace items (connectors.md, section 10);
   and loads its transcript, if its charter resumes and it has one within
   the resume limit (7.2). Either way it is given the calls committed
   after its task's last committed turn: a resumed run as a note after
   the transcript, a fresh one as a section of its brief.
3. **Commits the claim:** the task claimed, with its new attempt number,
   the host kind it is placed on, and the writer slots of the holds its
   workspace writes (tasks.md, 6.3).
4. **Places it** through the fleet, once the claim is durable, on a host
   its charter allows, with an assignment the root puts together
   (root.md, section 7):
   - its run and attempt, the core's names for them;
   - its workspace: the connectors' items, and a workstream key, the
     number of the task that holds the resources written, so that the
     runs of one piece of work find their workspace cached;
   - its charter, in the core's terms: the task's instructions, the
     brief, the tools its authority gives, the outcome its result
     contract asks for, its budget for the run, reserved from what its
     task has left in the claim's commit, and its models with their
     prices. The host's side translates it into smith's (hosts.md,
     section 8);
   - its transcript, when it resumes, and the calls committed since its
     last turn;
   - the credential grants its models' accounts need (section 12), and
     those its connectors' items need, which the protocol layer fills in.

### 7.2 Transcripts

- **A transcript belongs to its task,** whichever runs added to it.
- **Committed turn by turn.** A run sends each turn up as it ends: what
  the LLM said and the calls it made, their results, what it spent, and
  the last inbox message it read. The core commits the turn with its
  spend and with the messages up to that one taken, then acknowledges
  it. A host keeps each turn until it is acknowledged, and a run with too
  many unacknowledged turns waits (hosts.md, 6.4). Admission is whole:
  the hub validates the attempt, turn and offered read-through before
  changing its spend; a refused turn changes nothing, and a duplicate
  accepted turn changes nothing again.
- **The core never looks inside.** A turn is bytes of the agent's own
  conversation vocabulary, bounded, which a client's protocol layer
  renders. The core reads only its size, its spend and what it took.
- **Resuming.** A run that resumes is given the transcript whole, within
  the resume limit (the deployment's bound on a task's transcript bytes,
  across its attempts), and, as a note after it, the calls committed
  after its last turn, with their answers. One past the limit starts
  fresh from a brief that carries the transcript's tail. A transcript the
  agent cannot use fails its run as transient, saying so, and the next
  run is prepared fresh.
- **Turns are numbered within their attempt,** from one. The task keeps
  the last turn committed for its live attempt, restored with the fleet's
  adoption of the claim, so a copy already committed is acknowledged
  again and dropped, after a restart too.

### 7.3 The engine's tools

A run reaches the engine through smith's host tools, declared in its
charter, which its host relays; their schemas, decoding and rendering
are the protocol layer's (hosts.md, section 8). Each is checked against
its task's authority (authority.md, section 8), decided, committed, and
answered after the commit:

| Tool | What it does | Answers |
|---|---|---|
| `delegate` | makes a batch of tasks as its delegates (tasks.md, section 4) | their numbers; refused, and why; beyond authority, and what |
| `message` | words, a question or an answer to a task it references | sent; no reference; the inbox full |
| `amend`, `cancel`, `release` | its delegates (tasks.md, section 7) | done; refused, and why |
| `decide` | accepts, rejects or passes up a proposal or an escalation waiting for it (tasks.md, section 9) | done; beyond its authority, and passed up |
| `propose` | any of these, or an effect, or more authority for itself, with a reason | proposed, and to whom |
| `subscribe`, `unsubscribe` | a connector's topic, a referenced task's state, a timer | done; refused |
| `effect` | one of a connector's kinds of effect offered to agents (connectors.md, 4.5) | made, with what it made; committed and pending, past the call's deadline; waiting for a verdict; failed |
| `note`, `recall` | writes a note in a scope; reads entries by name or by a search (section 10) | written; the entries |
| a connector's reads | reads its system (connectors.md, section 9) | the answer, within the read's budget |

**Calls are named, and decided once.** Every call names its task's
attempt and the run's own name for it. The core commits a call that
changes state together with its part of the answer, and answers a call
it has seen before from that record, never deciding it twice; what a
connector adds to an answer (what an effect made) it keeps under the
call's key, and the root puts the answer together again (root.md,
section 7). The core keeps a task's records, by attempt and name, until a
turn of the task that carries their answers commits, or the task ends;
so calls an attempt made in a turn it lost are still there for the next
attempt to be told of (7.1). A run whose call was withdrawn, lost with
its channel, or answered busy asks it again with the same name while it
lives, so the LLM is told the outcome rather than trying anew; an
effect's key is derived from the call that asked for it (connectors.md,
4.4).

Three tools a run has are not the engine's: `wait`, which the run and its
host serve, holding the slot until a message arrives (7.4); `finish`,
which the run serves, checking its result against the contract (smith's
`run.md`, section 7), and whose result reaches the core as the run's
answer (tasks.md, 5.6); and `deliver`, which its host serves through the
application's workspace (hosts.md, section 9).

Beyond authority, a call is refused naming what it lacked; the run may
`propose` the same action with a reason. Calls never become proposals on
their own, so a person is never asked by accident.

### 7.4 Answers, waiting and parking

- **A run answers once:** finished, with its result; parked; or failed,
  with a typed failure. The core commits it, once it has every turn the
  answer counts, and acknowledges it; a finished task closes, a parked
  one is idle, a failed one retries or is held (tasks.md, 5.5). What the
  answer says the run left on resources goes to their connectors
  (connectors.md, section 10).
- **Waiting holds the slot; parking frees it.** A run that calls `wait`
  keeps its host's slot while it waits, so a chat answers at once. Idle
  past its charter's threshold, it parks: every turn it took is committed
  first, and it ends. A coordinator's threshold is zero: it parks as soon
  as it waits.
- **Woken again,** a parked task is due. Its charter says whether the
  next run resumes the transcript or starts fresh from a brief; by
  default a chat resumes and a coordinator starts fresh, its brief
  carrying its goal, its plan's state, its inbox and its notes.
- **Messages to a live run** are relayed as they arrive, named, and a turn
  that reads one takes it (7.2). What the run did not read before it
  ended stays in the inbox.
- **A cancel** ends the run: the host cancels it and answers; what it
  committed before stays committed.
- **Saved state.** A run that parks, fails or is cancelled with unfinished
  work has it saved by its host, where its connectors say, and the next
  run of its task starts from it (connectors.md, section 10).

## 8. The fleet

The fleet child domain knows the hosts and the runs they host:

- **Hosts of two kinds** (hosts.md, section 3): workers, which dial in
  with their slots and the workstreams their workspaces hold; and the
  engine's own slots, a configured number, for the host inside the
  engine. A charter says which kinds may run it. A run goes to a host
  with a free slot of a kind its charter allows, preferring a worker that
  holds its workstream. A refusal as busy is no failure.
- **Attempts are fenced.** Everything a host sends is dropped unless its
  attempt is its task's live one, but a cancelled attempt's answer. No
  attempt of a task is placed while the fleet knows a host may still run
  another.
- **Answers and turns are acknowledged once committed** (5.2), or once
  fenced off; a copy before then is dropped unacknowledged, one for an
  attempt the core has finished with is acknowledged again.
- **Losing contact.** A lost worker's runs are kept for a grace; one that
  reconnects says what it hosts, and the core keeps or cancels each. Past
  the grace they are lost, and their tasks retry. The engine's own slots
  are lost only with the engine (section 6).
- **Relaying:** messages and cancels down, tool calls up and their
  answers back, each call answered once; a call the fleet cannot pass up
  is answered at once, unserved, never left without an answer.
- **The graces are ordered.** The engine's grace is longer than the
  longest a worker may take to stop a run it lost contact for, which the
  worker says at its hello (hosts.md, 6.6); a worker whose bound is equal
  to or longer than the engine's grace is refused. So the core places no
  next attempt while a worker may still host the last, and one run at
  most writes a resource at a time.
- **Every start and adoption ends once:** answered, lost, withdrawn (the
  core cancelled it), or refused (busy, a duplicate, or invalid).
- **Restarting** (section 6): the fleet adopts the claims the store
  names, then hears that the loading is done; a run a worker lists that
  no claim adopts is cancelled once a grace has passed.

## 9. Briefs

The brief child domain builds every run's brief from sections, each
within a budget, the brief within its own: sections follow why the run
runs, those it runs for are required, the rest optional; what does not
fit is cut in its kind's order and says how much; sections are gathered
within one deadline, while the task is preparing, and a required one
missing sends the task back to due after a backoff, no attempt spent
(tasks.md, 5.2).

Its sections come from two places:

- **The core's,** which it holds typed: the task (its spec and contract,
  and its lineage: its requesters' specs, cut); its inbox since its last
  run, people's words whole; its dependencies' and inputs' results in
  jig's terms; its delegates and their states, and the results that came
  since (the plan, for a coordinator); its earlier attempts and why they
  failed; the calls its task committed since its last turn, when it does
  not resume; the proposals and escalations waiting for its decision; the
  index of its notes; a transcript's tail, when it does not resume.
- **Its connectors',** which it plans by kind, priority and budget but
  never holds (core.md, section 5): for each resource the task names, and
  for each dependency or input whose result is a connector's, the
  connector gathers the section, keeps it, and says its size; the brief
  child decides what fits, asks for cuts by size, and, when the brief is
  complete, the root takes each section from its connector to put the
  assignment together (connectors.md, section 9).

A section is loaded or read as it is cut, asking for no more than it
keeps. The host's protocol layer renders the whole as text, in order.

## 10. Notes

Notes are what agents learn and pass on: a service that is slow to warm
up, why an approach was dropped, which check is flaky, what a person
prefers. The notes child domain knows scopes, entries and indexes:

- **An entry** has a one-line description, a body, a scope, references to
  tasks, and who wrote it: a party, or a task.
- **Scopes:** the deployment; a project; a goal; a resource pattern of a
  connector's (a service, a repository), within a project.
- **In the store,** where parties read, correct and delete them through a
  client. Notes are hints: where a note and the facts disagree, the facts
  are right.
- **Written by a tool,** checked against the task's note scopes
  (authority.md, section 3), and committed like any decision. A revision
  names the revision its run recalled, and is refused as moved if the
  entry changed since.
- **Read in two steps.** A brief carries the index of the notes in its
  scope, a line per entry, saying how many more there are when they do
  not all fit; a run asks for the entries it wants with `recall`, by name
  or by a search of their descriptions, answered a page at a time.
  Indexes are kept per scope in use, and evicted least recently used;
  entries per scope are bounded.
- **Knowledge that belongs to a system** belongs there, such as a
  repository's guide: a note that matures may be proposed as an effect
  on it.

## 11. Views

The views child domain streams what runs report, and what tasks do, to
the parties watching:

- **Watches:** a run (its streaming text, its tool calls, its turns as
  they commit), a task's tree (phases as they change), a project's goals,
  a party's inbox. A watch starts from a snapshot the core gives,
  delivered first.
- **A slow watcher never holds the engine up:** one delivery in flight, a
  bounded backlog, what overflows dropped and the watcher told.
- **Nothing is stored:** what runs report as it happens is for watching
  only.
- **History is the store's.** What a client shows of the past (a task's
  results, its transcript, a plan's revisions and why) is loaded (5.3),
  paged, never held.

Nothing the core decides depends on a fact or a view arriving.

## 12. Accounts

The accounts child domain keeps the deployment's LLM accounts: refreshed
by the engine alone, their rotated refresh tokens made durable before
the access tokens they bought are granted, grants pushed to every live
attempt that uses an account, on whatever host, and a run whose account
is unusable not started. The refresh tokens are in the store's secret
records, which the protocol layer alone writes and the domain never
reads; the domain names an account by its number and a token's
generation.

## 13. Limits and working sets

What the core holds, each bounded by its limits and summed into its
worst case (programming-model.md, 6.3):

- live tasks, with their inboxes, subscriptions, references, holds and
  queues; pending proposals (tasks.md, section 11);
- the rules and the live projects' policies, within limits on projects,
  resources per project and parties holding roles;
- parties signed in, and their pending requests;
- the calls committed since each live task's last turn, and the funders'
  numbers;
- hosts, their runs, and the calls and messages relayed;
- LLM accounts;
- decisions in progress and their hand-offs; loads in flight and their
  rows;
- briefs gathering, with the sizes of the connectors' sections, and the
  notes' indexes in use;
- watches and their backlogs.

None of it grows with the store's history. A new task, a party, a run or
a watch past its limit is refused at its entrance, and the asker told
why. The root adds the core's worst case to its connectors' and its own
(root.md, section 10).

## 14. Below the domain

What the protocol and io layers owe the core, through the application's
protocol layer, which composes jig's pieces with its own:

- **The store:** commits of typed records, numbered, answered once
  durable, and loads by key range in pages, over skein-kv; jig encodes
  the core's records, the application its connectors', each with a
  version per shape.
- **Workers:** jig's channel, the engine's side: assignments, messages,
  cancels, answers to relayed calls, acknowledgements down; hellos,
  turns, calls, answers and facts up (hosts.md, section 10).
- **Connectors:** each its own protocol, the application's.
- **Parties:** a client protocol carrying jig's documents and the
  application's, with sign-in by the application's providers
  (people.md, section 10).
- **Accounts:** OAuth refreshes with the LLM providers.
- **Configuration:** the deployment's rules, connectors and their
  systems' addresses and credentials, LLM endpoints, accounts and models'
  prices, workers' secrets, the engine's own slots, the charters the
  deployment offers and procedures' settings, limits. Projects and their
  policies are the store's, seeded from configuration on a first start.

## 15. The world

Each child domain has a world of its own, its parent and neighbours
scripted in it, its expectations in a referee; authority, which keeps no
state, is tested by its step tests.

The core's world runs the core with every child beneath it, under a root
of jig's own for testing, against fakes that share none of its types
(testing.md, section 4): jig's test connector, with resources, holds,
pools, priced effects, requirements, procedures and faults drawn from
the seed; one to three scripted workers and the engine's own slots,
whose runs play scripts; scripted parties; the store on skein-kv's
in-memory mode, slow or failing a commit; and the engine restarting
cold, at drawn moments or at one a scenario chooses.

Its stories: a chat that answers, parks and resumes; a plan of reports, a
choice and procedure tasks, revised mid-way, and done; a procedure's
effect waiting for another connector's verdict, then made; a delegate
held past its tries, escalated to a person and released; a goal
cancelled with runs live and effects in flight, closing deepest first; a
tool call whose answer is lost with the channel and the engine, asked
again after a restart and answered from its record, made once; a run
handing a held resource to a procedure while it still holds the writer
slot; two tasks queued for a pool's last slot, and the pool shrinking
under them; an uncertain effect across restarts made before its retry
deadline; an effect of a kind that cannot be recovered, uncertain and
held for a person; a judge's facts changing between its verdict and the
effect; a policy narrowed while a proposal waits, an entry is committed
and a run is live; a note written, corrected
by a person and recalled; a recurring task across a period's reset; a
person's words to a running run; drift holding a task, and the client
told why.

Its referee holds the core to core.md, section 11, seen from outside;
applications' worlds reuse it on jig's conformance world (testing.md,
section 5).

## 16. Open questions

- **Commits in flight:** how many, and whether the store should take
  several decisions as one commit when they wait together.
- **Briefs:** whether a brief's store loads and connectors' gathering
  share one budget; cancelling a brief while it gathers; how a missing
  section shows in what the agent reads.
- **Transcripts' limits:** whether runs compact a transcript (smith's
  `session.md`, section 8) before the resume limit is reached, and how
  long the store keeps them.
- **Views:** a limit on watchers per subject; how a watch of a task's
  tree is bounded when the tree is large.
- **Scaling out:** one engine per deployment until it binds; the store
  being the engine's alone is what would change first.
