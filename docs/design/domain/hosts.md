# Agents and their hosts

Provisional, 2026-10-07. Where an application's agents run, and what
their hosts do: the contract between the core and a host, the three
shapes an application may take, jig's host inside the engine, jig's
worker host, the application's workspaces, and how a run's charter,
tools, contract and delivery become smith's. The agent is smith, whose
design is its own (smith's `docs/design/domain/`, cited as smith's
`run.md`, `session.md`, `tools.md`, `host.md`). The core's side of runs
is engine.md, sections 7 and 8. What is still open is listed in
section 12.

## 1. In one page

- **smith is the agent.** Nothing in smith assumes code, repositories or
  a workspace, so the same agent serves every application, told apart by
  its charters. jig builds no code into it.
- **The core needs a host for each run, not a worker** (section 2). A
  host starts a run with an assignment, relays messages and calls, keeps
  turns and the answer until the core acknowledges them, and hears how
  the run ends. smith's host contract carries the same (smith's
  `host.md`).
- **Three shapes** (section 3): no agents; agents inside the engine;
  agents on workers. An application may mix the last two, a charter
  saying which hosts may run it.
- **The host inside the engine** (section 5) is jig's local host: a
  child of the application's root, composing smith's domain once per run
  in flight. No process, no channel: for agents whose tools are the
  engine's and the connectors' reads.
- **The worker host** (section 6) is jig's hub of hosted runs on a
  worker: the link to the engine, fenced attempts, turns and answers kept
  until acknowledged, graces, supervision through smith's host domain,
  one process per run. The worker's root is the application's.
- **Workspaces are the application's** (section 7): a child of the
  worker's root that prepares what the connectors named, saves unfinished
  work, delivers what a run hands over, and says what the run left. jig
  defines the vocabulary; temper's git checkouts are one workspace.
- **The core never depends on smith.** It keeps turns as bytes and its
  charter in its own terms; the protocol layer, or the local host, puts
  them into smith's (section 8). Only the hosts, and a client's view of
  conversations, link smith.
- **Nothing a run sends is lost.** Turns and answers are kept until the
  core acknowledges them, once committed, and sent again after every
  hello.

## 2. The contract

Between the core's fleet and a host, whatever its shape:

- **Down, from the core:**
  - an **assignment:** the run and its attempt; the workspace's items and
    its workstream key; the charter; the transcript and the calls
    committed since its last turn, when it resumes; credential grants
    (engine.md, 7.1);
  - **messages** for the run, named, in order (tasks.md, 8.2);
  - **answers** to the run's relayed calls;
  - **grants** refreshed while the run lives;
  - **acknowledgements** of turns and of the answer, once committed;
  - at most one **cancel.**
- **Up, from the host:**
  - a **hello:** the host's slots, how long it may take to stop a run it
    lost contact for, the workstreams it holds, the runs it hosts and
    those whose turns or answers wait for acknowledgement;
  - **turns,** numbered within their attempt, each with its spend and the
    last message it read;
  - **calls,** named by the run, each answered once;
  - **facts,** best effort: progress, streaming text, tool calls as they
    happen;
  - one **answer:** finished with its result, parked, or failed with a
    typed failure; with the turns it took, what it spent, what it left on
    resources (the application's part), and how its save went;
  - **refusals:** busy, invalid, a duplicate.
- **Fenced.** Every message names the run's task and attempt; the core
  drops what a stale attempt sends, and a host drops an attempt the core
  has finished with.

## 3. Three shapes

```
no agents           procedures and people only             no host, no smith, no LLM accounts
agents in-engine    smith's domain inside the engine       no workspace; tools are host tools only
agents on workers   worker processes, agent processes      workspaces, commands, many machines
```

1. **No agents.** An approval pipeline, an operations runbook of
   procedures. The deployment configures no charters; a batch naming an
   agent executor is refused, and the fleet and accounts stay idle.
   Nothing links smith.
2. **Agents in the engine.** A triager, a researcher, a chat assistant,
   whose tools are the engine's and its connectors' reads. The engine's
   root composes jig's local host (section 5). The costs:
   - no process containment, which is fine when nothing runs commands;
   - the engine's worst case includes its runs;
   - LLM traffic goes through the engine's protocol layer, on its loop.
3. **Agents on workers.** When runs execute commands, hold workspaces, or
   need many machines. Each run is its own agent process, so stopping a
   run ends its process tree.

A charter says which kinds of host may run it, and the fleet places by it
(engine.md, section 8). `ops` is the second shape; temper is the third.

## 4. Structure

In the engine:

```
<application>-domain           the application's root
├── core                       jig's core
├── jig-local-host             the host inside the engine
│   └── smith-domain …         smith's run, one per run in flight on the engine's slots
└── …
```

On a worker:

```
<application>-worker           the worker's root: the application's; routes among its children
├── jig-worker-host            hosted runs: the engine link's vocabulary, admit, prepare, start, relay, park or end
├── smith-host-domain          agent processes: spawn, channel, watchdog, cancel then kill
└── <workspace>                the application's: prepare, save, deliver, release (section 7)
```

Each tree follows programming-model.md, 4.5. On a worker, `host` is the
hub: it knows a hosted run's lifecycle and nothing of the application's
workspaces, processes or channels; the other two are capabilities, which
the worker's root translates to and from with small total functions. A
hand-off goes from a capability to the host, from the host to a
capability, and at most once more back and forth, so each entry point
completes them before it returns. The worker's root has the one shape of
root.md, section 3, with no store: a worker keeps nothing durable.

## 5. The host inside the engine

`jig-local-host` hosts runs on the engine's own slots, a configured
number:

- **One smith run per run in flight,** each smith's domain composed as a
  child, in smith's one-process form (smith's `host.md`, section 9): the
  contract crosses as entities, translated by small total functions, with
  no pipe and no frames.
- **The assignment** comes from the core through the root, and the local
  host gives smith its start: the charter in smith's terms, which the
  local host translates from the core's (section 8), the transcript, and
  no workspace.
- **Calls** go up to the root as the core's, or as a connector's read or
  effect; answers come back. **Turns** go up as they end, kept until the
  core acknowledges them, and a run with too many unacknowledged waits,
  as on a worker (6.4).
- **LLM completions** are requests smith's domain makes; the root routes
  them to the application's protocol layer, which runs them on skein's
  LLM client, and their answers back.
- **Cancel** is smith's cancel: the run winds down and answers. There is
  no process to kill; a run that does not wind down within the grace is
  dropped by the local host and answered as stopped.
- **Lost only with the engine.** A restart settles its claims at once
  (engine.md, section 6); its slots never dial in.
- **Bounded:** each run's worst case is smith's, and the local host's is
  its slots' sum, part of the root's.

## 6. The worker host

### 6.1 The link to the engine

- **The worker dials the engine** and keeps one channel open. The engine
  never connects to a worker, so a worker can run wherever it can reach
  the engine. It dials at once, and redials with a jittered backoff that
  doubles up to a ceiling, starting over once a channel has proved
  itself.
- **Hello first,** as section 2 says, on every channel. Facts go only
  after it.
- **The engine holds the claim.** It commits a claim before assigning a
  run, and decides what follows: commit its result, run it again on any
  host, or hold its task. The worker never retries a run and never decides
  that work is done.
- **Attempts are fenced.** Once the engine has cancelled or reassigned an
  attempt, whatever it still sends is dropped; on the worker, a cancelled
  run's calls are answered as unavailable, and an attempt assigned again
  while the worker hosts it is dropped. A fresh assignment for an attempt
  already hosted is refused as busy.

### 6.2 Hosted runs

```
admit ─► prepare ─► start ─► active ⇄ waiting
                               │  turns: kept until acknowledged
                               │  calls: delivery served here; the rest relayed to the engine
                               ▼
                         park or end ─► stop ─► save ─► release ─► answer
cancel, from any state ──────────────► stop ─► save ─► release ─► answer
```

1. **Admit** the assignment, or refuse it at the entrance: busy (every
   slot taken, an answer not yet acknowledged keeping its own; the run
   hosted under another attempt; the worker shutting down) or invalid
   (beyond the limits).
2. **Prepare** the workspace from its items (section 7).
3. **Start** an agent process with the charter, and the transcript if
   there is one, through smith's host domain (6.8).
4. **Relay** while the run is live:
   - **messages** go down as they arrive, each named, a bounded few held
     until it is live; what does not fit, or comes as it ends, is bounced
     to the engine by name, which keeps it in the task's inbox;
   - **turns** come up as the run ends each, kept until acknowledged
     (6.4);
   - **calls** come up, a bounded number per run and one delivery at a
     time; deliveries are served by the workspace (section 9); every
     other call goes to the engine and its answer comes back. A call the
     run withdraws past its own deadline is answered as withdrawn, and
     may be asked again by the same name once its relay has ended;
   - a run that **waits** holds its slot until its next message, naming
     the last message it has read.
5. **Park or end,** as the run decides. A run parks only once every turn
   it took is sent; its transcript is its state.
6. **Stop:** answer the run's relayed calls as unavailable, and wait until
   the agent process and everything it started are gone, and until a
   delivery in flight has settled: a delivery cannot be abandoned half
   way, and what it made counts. Nothing touches the workspace while
   anything of the run is still running.
7. **Save** unfinished work, if the assignment asks: at park, at an
   unfinished end and at cancel, where the workspace's items say
   (section 7).
8. **Release** the workspace to the cache.
9. **Answer** the engine, exactly once (section 2). The engine decides
   what happens next, and its acknowledgement frees the slot.

A cancel from the engine, a lost channel or shutdown, or a fault of the
agent, takes the same tail: stop, save, release, answer. A run that says
how it finishes as it winds down has that as its answer; only a run that
says nothing is answered as the worker stopped it. The host arms no
timers: each wait is bounded below it.

### 6.3 Failures

Typed, so the engine can act on them without reading prose:

- **refused** at the entrance: busy, or invalid;
- **preparation failed:** transient (the system could not be reached,
  the worker's side failed or ran out of time, another run of the
  workstream holds its workspace), or permanent until something changes
  on a system, naming the resource the workspace says;
- **the run failed,** as it reports it (smith's `run.md`, section 10), or
  because an LLM account it needs is exhausted;
- **the agent failed:** it could not be started, it exited without
  answering, it broke the channel's rules, or the watchdog stopped it;
- **cancelled:** by the engine, for lost contact, or for shutdown.

Each carries bounded detail for operators; none of it is shown to an LLM.

### 6.4 Turns

- **A turn** is the bytes of one turn of the run's conversation, in
  smith's vocabulary (smith's `session.md`, section 3), numbered within
  its attempt, with what it spent and the last message it read. The
  worker reads only its number, its size and that name.
- **Kept until acknowledged,** like an answer: sent at once, sent again
  after every hello, and after a backoff when the engine answers it busy.
  A run may have a bounded number of turns, and bytes, unacknowledged;
  past it, the worker stops reading the run's channel, and the run waits,
  its watchdog paused.
- **Before the answer.** The answer says how many turns the run took,
  and is sent after them.
- **Lost with the worker.** A turn not acknowledged when the worker dies
  is lost with the run; the transcript ends at the last turn committed,
  and the next run starts from there.

### 6.5 Facts

What the run reports as it goes (smith's `run.md`, section 11) feeds the
watchdog and goes up to the engine for live views. Forwarding is best
effort: a bounded queue, what does not fit dropped and counted, sent on
an open channel only, after the hello. Nothing decides on a fact.

### 6.6 Losing contact

- **Runs go on for a grace** if the channel drops, and what they send
  waits, never dropped: turns, bounced messages, and relays, unless their
  call has closed meanwhile. On reconnecting, the worker says what it
  hosts and the engine keeps or cancels each run.
- **Past the grace,** the worker cancels them itself, saving their work
  first, and keeps their turns and answers for the next channel.
- **The stop bound** the worker says at its hello is the longest it may
  take to stop a run it lost contact for: its grace, then the longer of
  its cancel's grace and a delivery's deadline, then a save. The
  engine's grace is strictly longer (engine.md, section 8), so no next
  attempt starts while this one may still write.

### 6.7 Shutting down

Shutting down cancels every run and admits no more. The worker is done
once the engine has every turn and answer. It gives up what it keeps,
counted, only once no run is left and the engine is still out of reach
past the grace.

### 6.8 Agent processes

The agent child is smith's host domain (smith's `host.md`, section 4):

- **one process per run,** its process tree the run's containment
  boundary, its environment free of credentials, which come as grants on
  its channel;
- **one channel** over the process's pipes, whose rules smith's host
  domain enforces;
- **a watchdog on progress,** paused while the run waits for a message,
  for a call's answer, or for room to send a turn, and stretched to the
  span of a long operation the run reports; a separate bound on wall
  time;
- **cancel, then kill:** a cancel down the channel first, then the tree
  terminated, then killed, past a grace. Only an exited process and an
  empty tree release the slot.

## 7. Workspaces

A workspace is what a run works in, prepared from the items the
connectors named (connectors.md, section 10). Its kinds are the
application's: a child of the worker's root, speaking jig's workspace
vocabulary with the worker host, through the root:

- **prepare** these items under this workstream key: answered ready,
  with what the agent is to be told of it (directories, names, what may
  be written, a merge to resolve), or failed, transient or permanent,
  naming the resource;
- **deliver** what the run hands over, from its `deliver` or its
  `finish` (section 9): answered with the delivery's outcome in smith's
  terms, and, for the engine, what it left on the resources;
- **save** unfinished work where the items say: answered saved, with
  where, or failed;
- **release** the workspace to its cache, or **discard** it when it may
  be damaged.

- **A cache, keyed by workstream:** the runs of one piece of work find
  their workspace ready. A workspace is reused only if it holds exactly
  the items named, and is reset to their starting point: nothing local is
  authoritative.
- **One operation at a time** per workspace, each bounded by deadlines
  given as data; the workspace arms no timers.
- **No credentials on disk:** what reaches a system names an identity,
  which the protocol layer maps to credentials per operation.
- **A run with no items** needs no workspace, and its host prepares
  nothing; a worker may still run it, for containment.

temper's workspace is git checkouts of a forge's repositories: prepare
clones and checks out a starting point or a merge in progress, deliver
commits the tree the run checked and pushes it as a fast-forward, save
pushes to a saved-work branch. It is an example, not part of jig.

## 8. Charters, tools and contracts

The core decides a run's charter in its own terms (engine.md, 7.1). The
translation into smith's (smith's `run.md`, 3.1) is in one place per
shape: the engine's protocol layer, for workers, which carry it as
bytes; the local host, for runs in the engine.

- **Instructions and brief:** the charter's instructions, and the brief's
  sections rendered as smith's titled sections, in order, the
  connectors' sections among them as their connectors render them.
- **Tools:** the task's tool families become smith's:

  | jig's family | In smith |
  |---|---|
  | inspecting, modifying, the shell | smith's inspect, modify and shell, when the run has a workspace |
  | sub-agents | smith's sub-agents |
  | the engine's tools (engine.md, 7.3) | host tools, each declared with its schema, its description and whether it writes |
  | each connector's reads and offered effects | host tools, declared by the application |
  | waiting, by its wake policy | smith's `wait` |
  | a delivery, by its result contract or mid-run | smith's `deliver`, when the run has a workspace that delivers |

  Each host tool's schema and description are written once, in the
  protocol layer that decodes its calls; a call's input is decoded into
  the core's or the connector's typed request, or answered as an error
  naming what is wrong.
- **Result contracts** become smith's (smith's `run.md`, 7.1): a report;
  a verdict, with its labels, fields and follow-ups; a delivery, which
  the run must make through its workspace before it finishes, with the
  fields its workspace asks for; a failure. The core judges a result
  again as it commits it (tasks.md, 5.6).
- **Budget, models, waiting:** what the task has left, capped per run, in
  the deployment's unit, with each model's prices; the LLM endpoints the
  accounts give, their credentials as grants; the charter's waiting time,
  and whether a run resumes its transcript or starts fresh.

## 9. Messages and delivery

- **Messages** go to smith as messages, rendered by the protocol layer
  (or the local host): a sender's label (its requester, a delegate by
  number, a party by name, a subscription's topic) and the message's
  words, whole. Proposals and escalations waiting for the task are
  relayed the same way. The run names the last it read with each turn
  and when it waits.
- **Delivery** is the workspace's: smith's `deliver` is served by the
  worker host through the application's workspace (section 7), which
  makes it, for temper, as a push. Its outcome is smith's (smith's
  `run.md`, 8.2): delivered, nothing, stale, refused, failed. A delivery
  that was made is the run's result even if the run was winding down;
  what it left on resources reaches their connectors with the answer.

## 10. Below the domain

- **The engine's channel,** jig's: framed, dialled by the worker and
  authenticated, carrying the contract of section 2, versioned.
- **Agents:** spawning smith's agent in a contained process tree, with
  proof that the tree is empty after it stops; smith's channel over its
  pipes (`smith-channel`, the host's half).
- **Workspaces' io:** the application's (for temper, git invocations and
  workspace directories).
- **In the engine:** LLM completions through skein's LLM client, in the
  engine's protocol layer.

## 11. The world

- **The worker host's world:** its parent scripted; an engine that
  assigns, relays, cancels and acknowledges once its fake store commits;
  a network whose channel drops for less and for more than the grace;
  smith's scripted agent, which progresses, calls, takes turns, waits,
  parks and ends, but also hangs, crashes, breaks rules and ignores
  cancels; and a scripted workspace that prepares, delivers, saves and
  fails. It checks that every turn and answer reaches the engine once and
  is acknowledged once, every relayed call is answered once, nothing
  touches a workspace while a run's agent may, and nothing is live once
  it settles.
- **The local host's world:** smith's real domain over skein's fake LLM,
  a scripted core above it: runs that answer, wait, park and resume,
  cancelled mid-turn, with turns kept until acknowledged.
- **The application's worlds** run its workers, with its own workspace,
  or its local host, against jig's core on the conformance world
  (testing.md, section 5).

## 12. Open questions

- **Fairness on the engine's loop:** whether runs in the engine need a
  share of each iteration, so many chats never slow the engine's
  decisions.
- **Saving periodically,** so a dying worker loses less than the work
  since the run's last save.
- **Before the first channel:** whether a worker's failed dials, before
  it ever reached the engine, want a grace of their own.
- **Agents other than smith:** the contract of section 2 is jig's; an
  agent that spoke it through a host of its own could run beside smith.
  Not wanted yet.
