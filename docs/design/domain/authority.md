# Authority

Provisional, 2026-10-07. What a task may do, and how the core holds it
there: authority as a value, its order and how delegation narrows it,
where it comes from, budgets and what is spent, the checks every action
passes, what a proposal needs and what accepting it funds, and the
requirements no task loosens. It is the fourth primitive of core.md
(3.4) in depth. Proposals travel the tree as tasks.md, section 9 says;
parties' roles are people.md, section 4. What is still open is listed in
section 12.

## 1. In one page

- **Authority is a value with an order.** It names tools, effects and
  reads on resources, what may be delegated and what may be spent. One
  authority is at most another when every part of it is: a preorder,
  decidable from the two values alone. Whether a delegate's authority
  **fits** under its creator's is that order, checked against what the
  creator has left.
- **Resources are named by paths, grants by patterns.** Each connector
  names its resources as paths of segments; an effect is granted on
  every resource a pattern covers. Patterns order themselves, so the
  authority child domain knows no connector, and the deployment's rules
  are written once for all of them.
- **Budgets are carved, not copied.** What a task gives a delegate of
  its budget is reserved from its own; what the delegate leaves unspent
  goes back when it ends, and what it spent is counted up the chain that
  funded it. Spend is counted in one unit of the deployment's: LLM
  completions as runs price them, and effects as their connectors price
  them.
- **Recorded spend never exceeds a budget.** A run reserves each
  completion's maximum before making it, and a priced effect is charged
  its maximum when decided. The only spend beyond a budget is what a
  worker spent on turns it lost before they were committed, which is
  bounded (section 7).
- **Every action is checked** before it commits: a batch, an effect, a
  read, a tool call, a run starting, a party's request, an amendment, a
  note. A check answers allow, wait, propose, or refuse.
- **Requirements are verdicts.** The policy attaches requirements to
  kinds of effect; each names a connector, which judges it from its own
  facts: met, wait, or refuse. The connector making the effect need not
  be the one judging. A guarded requirement holds when the effect is
  applied; an observed one, when it is decided.
- **Beyond its authority, a task proposes.** The nearest holder above it
  whose authority covers what the action needs accepts and funds it, or
  rejects it.
- **The root is the deployment's rules and the project's policy,** as
  they stand when a decision is made. No task, and no party, loosens
  them; a narrowing reaches every task's later decisions.
- **Pure policy.** The authority child domain decides over values it is
  given and keeps nothing between calls but the rules and policies in
  force, so it is tested by its step tests.

## 2. Structure

The authority child domain is a child of the core (engine.md,
section 3). It is policy: a decision over data. It holds the
deployment's rules and each live project's policy, which change only by
a commit (people.md, 5.2). The numbers kept against each task's
authority (what it spent, what its ended delegates spent, what it
reserved, who funded it) are the tasks child domain's (tasks.md,
section 3); the core gives them with each question, and authority's
answer says what they become. Each child domain has its own type for
authority, which the core translates.

Its questions, each a pure function of what the core gathers:

- is this authority at most that one (section 5)?
- does this batch fit under its creator, and what do the numbers become
  (8.1)?
- may this task, or this procedure, make this effect, given the verdicts
  its requirements received (8.2)? Which verdicts does it need
  (section 10)?
- may this run start, and with what budget (8.3)?
- may this party do this, in this project (8.4)?
- what does this proposal need, and which of these holders covers it
  (section 9)?

## 3. What authority is

```
Authority
  tools       the tool families an agent's run may call
  grants      effects and reads, each a connector's kind on a pattern of resources
  delegation  executor kinds and charters it may delegate to; how many tasks; how deep
  budget      what it may spend, in the deployment's unit; by when
  notes       the scopes it may write notes in
```

- **Tools:** families, as an agent's charter names them (hosts.md,
  section 8): smith's own (inspecting the workspace, modifying it, the
  shell, sub-agents), each connector's reads, and the engine's tools,
  each its own family (delegate, message, amend and cancel, decide,
  propose, subscribe, effect, note, recall). A procedure has no tools; its
  authority's tools are empty.
- **Grants:** each is a connector, a kind of effect or read that
  connector defines, and a pattern of resource names (section 4):
  "restart the services under `staging`", "create environments in the
  pool `staging`", "read every service's logs". A task may make an effect
  or a read only if some grant covers it.
- **Delegation:**
  - *kinds:* which executors its delegates may have: agents with which
    charters, which procedures, people in which roles;
  - *tasks:* how many tasks its subtree may make over its life;
  - *depth:* how deep below it its subtree may go.
- **Budget:**
  - *spend:* how much its subtree may spend;
  - *deadline:* by when it must end, in wall time, if at all.
- **Notes:** the scopes it may write in (engine.md, section 10). It may
  read every scope its project reaches.

## 4. Resources and their names

- **A resource's name is a connector's number and a path** of segments,
  given by the connector: for `ops`'s infrastructure, `env`, the
  environment, `service` and the service's name; for a forge, the host,
  the repository, `branch` and the branch's own segments. Names are
  bytes, compared segment by segment.
- **A grant names a pattern:** segments matched exactly, then a last
  segment that is either exact or open, an open one covering every
  segment that begins with it, and every name beneath. One pattern is at
  most another when every name the first covers, the second covers too,
  which is decided segment by segment.
- **Names after a task.** A connector may name resources after the task
  that makes them (a branch per task, an environment per request), under
  a prefix of the deployment's. A batch names them symbolically ("its own
  environment", "its runs' branches") and resolves them once the task has
  its number, each with its grant, so a creator can narrow a delegate to
  its own resources without knowing its number.
- **Kinds are a connector's,** with an order of their own where one
  implies another: scaling a service may imply restarting it, while
  landing into a branch does not imply pushing to it. A kind's order is a
  table the connector gives, and the authority child domain holds as
  data.

## 5. Order and fitting

- **The order.** Authority A is at most B when:
  - A's tools, kinds of delegate and note scopes are subsets of B's;
  - every grant of A is covered by one of B's: the same connector, a kind
    at most B's, a pattern at most B's;
  - A's tasks, depth and spend are at most B's, and A's deadline no later
    than B's (no deadline being the latest).

  It is reflexive and transitive, a preorder: two authorities with the
  same grants written differently may each be at most the other. The
  step tests check its laws (section 11).
- **Fitting.** A delegate's authority A fits under its creator B, given
  the numbers kept against B, when A is at most B with B's depth less
  one and B's tasks and spend replaced by what B has left. Fitting is
  what a batch, an amendment and an acceptance are checked against.
- **Narrowing** is giving a delegate an authority that fits. It is
  checked, never applied silently: a batch asking for more than fits is
  refused, naming what it lacked, and may be proposed (section 9); it is
  never trimmed to fit.
- **What is consumed and what is not.** Tools, grants, kinds, depth,
  deadlines and scopes are shared: a delegate given a grant does not take
  it from its creator. Spend and tasks are consumed: what a delegate is
  given is reserved from its creator (section 7).

## 6. Where it comes from

```
deployment's rules       the most any project may have; requirements on effects
  project's policy       the most any task in the project may have; its roles; its requirements
    a party's role       what a party may give, accept and do in the project
      a root task        what its creator gave it, fitting under the creator
        its delegates    narrowed at every level
```

- **The deployment's rules** are configuration: the connectors and the
  kinds of effect any project may be granted, the deployment's spend per
  period, the charters and procedures it has, the limits, and the
  requirements every project's effects meet. A rule may forbid what no
  project may grant at all.
- **A project's policy** is in the store, changed by the parties whose
  role allows it, within the deployment's rules:
  - its ceiling: the authority no task in the project exceeds, which
    follows from its resources' roles (owned, fork, context) and from
    what the application was given on each at adoption (connectors.md,
    section 12);
  - its spend per period, from which every root task's budget is carved;
  - its roles: for each, the authority a party in it has, the spend a
    party in it may allot per period, and the kinds of proposal it may
    decide (people.md, section 4);
  - its requirements: per kind of effect and pattern of resources, what
    an effect there needs (section 10).
- **A party's authority** in a project is their role's, and their
  allotment is a pool per period with its own amount left. A party who
  creates a task (a chat, a goal), or accepts a proposal, funds it from
  that pool, which is itself carved from the project's spend for the
  period.
- **The deployment** is the requester of what configuration or a
  connector starts on a project's behalf (a recurring task, a standing
  watch), with the authority the project's policy gives such tasks,
  funded from the project's spend.
- **Effective authority.** What a task may do when it decides is its own
  authority within the deployment's rules, the project's policy and what
  adoption found the application may do, as all three stand then:
  - *a narrowing* applies to every decision after it, existing tasks'
    included: a grant beyond the new ceiling is unusable, not removed,
    and a check that meets it answers refuse;
  - *a widening* never widens an existing task: its authority is what it
    was given;
  - *effects already committed* are made: they were decided under the
    policy that stood then. A party who wants them stopped cancels their
    task, which withdraws what has not been sent (tasks.md, section 7);
  - *pending proposals* are judged again when accepted, against the
    policy as it stands then;
  - *a live run* whose workspace writes something the narrowing forbids
    is cancelled, and its task's next run starts within the new ceiling;
    its other calls are checked as they come.

## 7. Budgets and spend

- **Four numbers per funder:** its budget (what it was given), what it
  has spent itself, what the ended tasks it funded spent, and what it has
  reserved for the live tasks it funds. What it has left is its budget
  less the other three, and never below zero. A task is a funder; so is
  each party's pool and each project's spend, per period, whose numbers
  the tasks child domain keeps too, in the store (engine.md, 5.4).
- **Every reservation records its funder:** the task above, a party's
  pool, or a project's period. A batch reserves each delegate's budget
  from its creator in the commit that makes it; an accepted proposal
  reserves from its accepter.
- **When a task ends,** in the commit that ends it, its reservation is
  released: its unspent budget goes back to its funder, and what it and
  the tasks it funded spent is added to its funder's "spent by ended
  tasks". Spend climbs the chain one end at a time, to the pool or period
  at the top, and is never counted twice nor lost.
- **A task moved** to a new requester (tasks.md, section 7) is funded
  anew: its old funder is given back what the task has left and keeps
  what it spent; the new funder reserves what the task has left from its
  own.
- **No overrun is recorded.** Every charge is reserved before it is
  made (below), so what is recorded against a task never passes its
  budget. A task left with too little for its next run or effect waits,
  or is held for its budget (tasks.md, 5.5).
- **Periods.** A project's spend and a party's allotment are per period.
  A reservation counts in the period that funded it, and what it returns
  goes back to that period, so a reset frees nothing reserved. A
  recurring task's budget is carved again each period (tasks.md,
  section 10).
- **Standing work renews by period.** A standing task, recurring or a
  standing procedure (connectors.md, 6.6), has its tasks and its spend
  carved again from the project's spend each period, so a watch that
  runs for months never exhausts its allotment of tasks. Its delegates
  keep their reservations in the period that funded them, and return to
  it.
- **What is spent:**
  - *LLM completions,* priced as they are spent. A charter carries its
    models' prices, in the deployment's unit; a run prices each
    completion it makes, its sub-agents' included, and reports what it
    spent with each turn it commits (engine.md, 7.2), and the whole with
    its answer. The core charges each turn as it commits it.
  - *Priced effects.* A connector may price a kind of effect, in the
    deployment's unit: an environment for a week, a scale-up for a day.
    The price is the effect's maximum cost, part of its description when
    it is decided, checked against the task's budget as part of the
    effect's check (8.2), and charged in the commit that decides it.
    What the connector later finds it cost below that is returned when
    it says so; it never charges more.
- **A run's budget** is reserved from what its task has left in the
  commit that claims it, capped per run by the deployment. The agent
  enforces it exactly: before each completion, its sub-agents' included,
  it reserves that completion's maximum cost (the input it sends, and
  the output its `max_tokens` allows, at its model's prices) from the
  run's budget, and settles the difference when the completion ends. A
  completion whose maximum does not fit is not made, and the run ends
  for its budget (hosts.md, section 8). So a run never spends past its
  budget, and stops a little early rather than late.
- **Spend lost with a worker.** A turn's spend is recorded when the turn
  commits. A worker that dies loses the turns it had not had
  acknowledged, and what they spent was spent but never recorded; the
  next attempt is budgeted from what the store recorded. That spend is
  the only spend beyond a budget, bounded by the turns and bytes a run
  may keep unacknowledged (hosts.md, 6.4) at its models' prices.
- **Time is a deadline.** A task past its deadline is held, and its
  requester told; its delegates' deadlines are no later than its own.
- **Load is not spend.** A connector's requests to its system are its
  load (connectors.md, section 13).

## 8. Checks

Every check answers one of four, each saying why:

- **allow;**
- **wait** for verdicts a connector cannot give yet (a health it has yet
  to read, a check still running), which nobody clears by deciding;
- **propose:** beyond the task's authority, within what someone above it
  may accept;
- **refuse:** beyond the project's policy or the deployment's rules, or
  against a requirement a connector refused, which no acceptance
  changes.

A check answers the strictest of what it found.

### 8.1 Batches

Before a batch commits (tasks.md, section 4): each task's authority fits
under its creator (section 5), the creator's delegation allows each
task's executor kind and charter, and the batch's tasks, depth, budgets
and deadlines are within what its creator has left. Beyond any of these,
the answer is propose; beyond the project's ceiling, refuse. Allowed, the
answer gives the creator's numbers after the reservations.

### 8.2 Effects

When a decision asks for an effect, whether an agent's tool, a
procedure's step or an accepted proposal, the core checks the effect's
description, which its connector gives (connectors.md, 4.1):

- a grant of the task covers its kind and its resources, or the answer
  is propose;
- the project's ceiling and the deployment's rules allow it, and its
  resources' roles admit it, or the answer is refuse;
- its price, if it has one, is within what the task has left, or the
  answer is propose, as a widening of its budget;
- the verdicts its requirements need (section 10) were given for the
  exact state the effect names, observed ones within their freshness:
  wait while one is wait, refuse once one is refuse;
- every requirement the policy marks as one that must be guarded is
  guarded by the effect's kind, or the answer is refuse.

The effect carries its condition to the system where the system can
check it: a rollback made only if the service is still at the version
decided on, a merge made at exactly the head decided on, so the guarded
verdicts given for that state stay true while it is made. Observed
verdicts promise only what their judges saw when the effect was decided.
Every check is against effective authority (section 6).

### 8.3 Runs

Before a run is claimed and again as it starts: the task has budget left
above the deployment's minimum for a run, its deadline is not past, the
accounts its charter's models use are usable (engine.md, section 12), a
host the charter allows has room (engine.md, section 8), and every
resource its workspace writes, saved state included, is covered by a
grant of the task and held by the task or by the task above it that
holds it (tasks.md, section 6). A run that fails this check is not
started: its task waits for its budget (held, if nothing will free one),
its account, its host, or its writer slot.

### 8.4 Parties' requests

Before a party's request is acted on (people.md, section 5): their role
in the project allows the request and, for what gives authority (a task
created, a budget allotted, a proposal accepted), what it gives fits
under their role's authority and what their pool has left.

### 8.5 Tool calls and reads

Before an agent's call is served: its family is among the task's tools,
and a read it makes is covered by a read grant on the resource it names.
A message to a task needs a reference to it (tasks.md, 8.5); a note
needs its scope.

## 9. Proposals

What may be proposed, what it needs, and what an acceptance does; where
proposals wait and how they move is tasks.md, section 9.

- **What may be proposed:** a batch beyond its creator's authority; an
  effect beyond its grants or its budget; a widening of the proposer's
  own authority (more budget, more depth, another grant, a later
  deadline); an amendment beyond the amender's authority. Never what the
  project's policy or the deployment's rules refuse, nor an effect a
  requirement refused.
- **What it needs** is the least authority the action takes: for a
  batch, every tool, grant, kind and scope its tasks have, the sum of
  their tasks and budgets, their depth, their latest deadline; for an
  effect, the one grant that covers it, and its price; for a widening,
  what it asks for. The holder that accepts funds all of it, whatever
  the proposer has.
- **An escalation needs** nothing more than standing above the held task,
  unless releasing it needs what the task lacks (budget, time, a grant):
  then it needs that, as a widening would.
- **Who covers it.** A holder covers a proposal when what it needs fits
  under the holder (section 5), its depth counted from the holder: a task
  above the proposer, or a party whose role decides that kind of
  proposal and whose pool has the budget left.
- **An acceptance funds the action** from the accepter: the budgets of a
  batch's tasks are reserved from the accepter, which is recorded as
  their funder; a widening moves budget or a grant from the accepter into
  the proposer; an effect is made with the accepter's grant, its price
  charged to the accepter. The action commits with the acceptance, as one
  decision; an effect's requirements are judged again then, as any
  effect's are.
- **An acceptance decides one action,** not a standing permission. A task
  that keeps needing more proposes widening its authority instead, once.
- **A rejection carries its reason** to the proposer, which may revise
  and propose again.

## 10. Requirements

Beyond what tasks may be given, the rules and the policy attach
**requirements** to kinds of effect, which hold whatever any task's
authority says.

- **A requirement** is attached to a connector's kind of effect on a
  pattern of resources, and names a judge: a connector and one of the
  requirements that connector defines, with the parameters it takes in
  that connector's terms, which the judging connector keeps as its own
  configuration. "Restarting a service in production needs observability's
  *healthy replica elsewhere*"; "scaling a service down needs
  observability's *load below 40% for 30 minutes*"; "landing into `main`
  needs the forge's *CI passed at this head*".
- **The authority child domain knows** only which judges each effect
  needs, whether each is guarded by the effect's kind, the freshness of
  each observed one, and what their verdicts were. It never knows what a
  requirement means.
- **Guarded or observed** (connectors.md, section 7). A requirement the
  effect's own system checks as it applies the effect is guarded, and
  holds when the effect is applied. Any other is observed: judged when
  the effect is decided, within a freshness the policy gives it, and it
  may stop holding before the effect is applied. A requirement that is
  safety-critical is marked as one that must be guarded, and an effect
  kind whose system cannot guard it is refused wherever the requirement
  applies.
- **Judged by its connector,** from that connector's own facts, for the
  exact state the effect names (connectors.md, section 7):
  - *met:* its facts, read afresh enough, hold;
  - *wait:* its facts are unknown or pending, saying what it waits for;
    it reads afresh, and the decision is asked again when they change;
  - *refuse:* a fact failed, saying which.

  The judge finds the subject of its facts from the effect's resource
  names, by its own configuration: the service a restart names is the
  service whose health observability reads.
- **Across connectors.** The connector judging a requirement need not be
  the one making the effect. The core asks each judge the effect needs,
  through the root, which routes each question to its connector and each
  verdict back (root.md, section 9).
- **Gates a plan adds** to one task (a review at a head, a person's
  approval) are its procedure's own, and add to these, never replace
  them.
- **Who decides what:** for each kind of proposal (a goal's budget past
  a threshold, an effect in production), the role that may accept it.
- **Requirements are checked twice:** by the procedure that waits for
  them, which is the mechanics, and by the core at the effect, which is
  the rule. A procedure that asks for an effect whose requirements do not
  hold is refused, which is a bug in the procedure, counted, and never an
  effect made.

## 11. The world

Authority, a decision over data, is tested by its step tests: the order
checked against an independent statement of it over generated values,
and its laws (reflexive, transitive, fitting never above the order);
carving and returning budgets over generated trees with funders, the
spend of a subtree never counted twice and never above what funded it,
with reservations of completions' maxima under many open sessions;
standing allotments renewed across periods with delegates live; every
check's answer over generated verdicts, freshness and prices, the
strictest always winning; effective authority as policy narrows and
widens around live tasks. The core's world exercises it in
place (engine.md, section 15), whose referee holds the first promise of
core.md, section 11.

## 12. Open questions

- **The deployment's unit,** and whether spend per period has a window
  that rolls or resets.
- **Costs over time:** an environment kept past what its effect was
  priced for, charged as it lasts rather than once.
- **Read grants:** whether reads within a project need grants at all, or
  are the project's for any of its tasks.
- **Delegated decisions:** whether a holder may give a delegate the right
  to decide some proposals from below it, as a coordinator might for its
  sub-coordinators.
