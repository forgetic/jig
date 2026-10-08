# Testing the domain

Provisional, 2026-10-07. How jig's domain is tested, and how it tests an
application's: the worlds of jig's own parts, the core's world, and the
conformance world every application runs its domain on, with the referee
that holds it to jig's promises. It applies skein's `testing-strategy.md`
to jig's domain layer; the tiers below the domain (protocol, simulated,
the real loop) come with jig's protocol layers. What is still open is
listed in section 10.

## 1. In one page

- **Domain worlds first,** as testing-strategy.md, 2.2 has them: every
  child domain of the core has its world, the core has its own, and both
  hosts have theirs, all before any protocol layer exists.
- **The conformance world is jig's gift to applications.** It runs an
  application's whole engine domain, its root over jig's core and its
  connectors, against jig's fakes for the store, the hosts and the
  parties and the application's fakes for its systems; it crashes the
  engine at every commit, holds and fails commits, loses hosts; and
  jig's referee checks, from outside, every promise of core.md,
  section 11.
- **Generic in the world, concrete in the domain.** Worlds are ordinary
  Rust (programming-model.md, 10.2), so the conformance world is generic
  over the application, through a trait of the test kit's. No step code
  is.
- **A test connector** stands in for an application in jig's own worlds:
  resources, holds and pools, priced effects, requirements, procedures,
  topics and faults, all drawn from a seed.
- **Faults everywhere, limits tiny,** replayed from a seed, on skein's
  world harness, within the suites' time budgets.

## 2. jig's tiers

| Tier | Real | Faked |
|---|---|---|
| step tests | one step function; authority's checks | its events, by hand |
| a child's world | one child domain of the core, or the hub | its parent, scripted; its neighbours |
| the core's world | the core with every child, under a root of jig's own | the test connector, scripted hosts, scripted parties, the store |
| the conformance world | an application's engine domain, root and all | jig's fakes, the application's fakes, the faults |
| an application's system worlds | its engine, its workers and its agents' domains together | its systems' fakes, at their domain faces |

Every world runs on skein's world harness: one loop, one seed, the
referee in the loop, the heap counted at every iteration
(testing-strategy.md, sections 6 and 7).

## 3. The children's worlds

- **tasks:** batches, lifecycle, holds and queues, inboxes, wakes,
  proposals (tasks.md, section 12).
- **authority:** step tests only, as a decision over data (authority.md,
  section 11).
- **people:** sign-ins, roles, requests, inboxes (people.md,
  section 11).
- **fleet:** hosts of both kinds, placement, fencing, graces, adoption.
- **brief:** sections planned, gathered, cut by size, a required one
  missing; connectors' sections as sizes and tokens only.
- **notes, views, accounts:** each its own concern.
- **the hub,** with agents of both kinds, processes and the inline agent:
  hosts.md, section 11.

Each child's world plays its parent and the store as engine.md, 5.6
says: it keeps saved records, commits at the end of each step, and
restarts by restoring them, so restart is tested in each child before the
core exists.

## 4. The core's world

The core with every child beneath it, under a root of jig's own for
testing, against fakes that share none of its types:

- **jig's test connector** (section 7);
- **one to three scripted workers and the engine's own slots,** whose
  runs play scripts: turns, calls, waits, parks, answers, crashes and
  silence;
- **scripted parties,** making requests and answering what waits for
  them;
- **the store,** skein-kv's in-memory mode, slow, holding commits, or
  failing one, which stops the engine;
- **restarts,** cold, at drawn moments or at one a scenario chooses, from
  the store's last durable commit.

Its stories are engine.md, section 15's; its referee is section 6's.

## 5. The conformance world

What every application runs, and what makes jig's promises its own.

- **What jig gives:**
  - the harness, generic over the application: it builds the
    application's engine domain from its configuration, feeds it events,
    takes what it asks for from the journal, and plays its peers;
  - the fakes for jig's peers (section 7);
  - the faults: a crash after any commit, the engine restarted cold from
    the last durable one; commits held, so the engine runs ahead; a
    commit failed; hosts lost, slow, or back after their grace; parties
    sending a request twice;
  - the adversarial scenarios every application runs, with tiny limits:
    - an effect's copy arriving late, after its retry deadline and after
      a retry; a target state another hand reached too;
    - the engine restarting again and again while an effect is
      uncertain, before its deadline passes;
    - a judge's facts changing between its verdict and the effect;
    - a pool shrinking while its slots are held and a task waits;
    - the policy narrowing while a proposal waits, an entry is committed
      and a run is live;
    - a standing task running across many periods;
    - many sessions and sub-agents of one run near the end of its
      budget;

    Each runs with the application's own kinds. One the application has
    no kind for (a pool, where its connectors take no pooled holds) is
    reported as not applicable, never faked with a different mechanism;
    jig's own worlds still cover it;
  - the referee (section 6).
- **What the application gives,** through the harness's trait:
  - its engine domain, built from configuration;
  - its systems' fakes, as step machines, each reporting what it saw in
    the referee's terms: which effects reached it, under which keys, on
    which resources, of which kinds, at what condition. The fake reports
    what it observed in its own system's terms, mapped by the fake, never
    by the root, so a root that mistranslates an effect is caught when
    the observed effect differs from the one the core decided;
  - each kind of effect's recovery class, which the referee holds the
    fake system's observations to;
  - its scenarios: the parties' scripts, its systems' starting state and
    faults, its agents' scripts;
  - the policy each scenario runs under, which the referee reads as its
    independent statement of what is allowed.
- **Crashing at every commit.** For a scenario, the focused suite crashes
  after a few chosen commits and checks the story still ends as it
  should; the fuzzy suite sweeps every commit of the scenario, and draws
  cuts at random across random worlds.
- **What it catches,** where its scenarios reach: a root that sends a
  held output early, splits a decision across commits, admits after a
  child changed, restarts out of the core's order, lets an effect reach
  a connector unchecked, or translates one wrongly (root.md,
  section 11); and a connector that repeats an effect beyond its
  recovery class, gives a verdict on facts older than their freshness,
  forgets a retry deadline across a restart, or decides differently when
  stepped twice on the same facts.

## 6. The referee

jig's referee watches from outside (testing-strategy.md, section 7): what
the fakes saw, what the store made durable, and the facts the domain
emits, never a domain's state. Each promise of core.md, section 11
becomes observations:

- **Authority holds:** every effect a fake system saw is covered by the
  scenario's policy as it stood when the effect was decided, for the
  task that asked, or by a proposal whose acceptance the store made
  durable first; every effect whose kind has requirements saw, before
  it, a met verdict from each judge, each observed one within its
  freshness when decided, each guarded one still true when applied; the
  effect a fake observed is the effect the core decided, in kind and
  resources.
- **Spend is bounded:** recorded spend never passes a task's budget,
  and actual spend, the fake LLM's own count, passes it by no more than
  the turns lost with workers allow.
- **Once, as each kind allows:** a keyed or conditional effect takes
  effect on its fake system at most once, late copies included; an
  idempotent one ends in the decided state; an unrecoverable one whose
  outcome was uncertain is never repeated before a person decides; every
  batch is in the store whole or not at all; every uncertain entry is
  resolved, or held, within a bound however often the engine restarts.
- **Nothing ahead of its commit:** every effect a fake system saw, every
  assignment a host saw, every answer a party or a run saw followed a
  durable commit that holds it; after a crash, nothing seen is missing
  from the store.
- **Order:** no task started before its dependencies were done and
closed; no two runs held one resource's writer slot at once; no pool
admitted a holder beyond the slots it was known to have; no task ended
before its delegates.
- **Nothing lost:** liveness deadlines for every result to reach its
  requester, every person's words to reach their task or the task to end,
  every proposal to be decided, withdrawn, or waiting where a holder sees
  it.
- **Nothing written over:** no effect on an owned object that the
  application did not make it, but as a participant or through its
  connector's mechanics.
- **Bounded:** the heap within the worst case at every iteration, and
  every story ending within its bound.

## 7. Fakes and kits

What jig ships for tests, as ordinary Rust or as step machines where
more than one world needs them (testing-strategy.md, section 4):

- **the store:** skein-kv's in-memory mode, behind a fake that holds
  commits, fails one, and serves pages slowly;
- **scripted workers:** each a host that speaks the contract of
  hosts.md, section 2, playing a run's script, keeping turns and answers
  until acknowledged, losing its channel and coming back, or vanishing;
- **the inline agent's runs:** smith's real domain over skein's fake LLM,
  with a script per charter;
- **scripted parties:** sign-ins and requests, by script or at random
  within their role;
- **the test connector:** resources named by paths, kinds of hold
  (exclusive and pooled, refusing and waiting, pools that shrink),
  priced effects of every form and every recovery class, with keys,
  conditions, late copies and uncertainty, requirements guarded and
  observed, judged on its facts, procedures that step, wait, delegate
  and finish, topics with classified news, drift, and a fake system
  beneath it that reports what it saw. The core's world runs two of
  them, so that judges, resources and kinds cross connectors, with
  names and kinds generated from the seed;
- **the referee and the harness** of the conformance world.

They stay in jig, and an application's worlds reuse them. What jig finds
missing in skein's world harness goes into skein first.

## 8. What an application adds

- **its connectors' worlds,** each with its system's fake below and the
  root scripted above (connectors.md, section 15);
- **its conformance scenarios,** which are its stories: `ops`'s are its
  `examples.md`, section 7;
- **its system worlds,** if it has workers: its engine, its workers with
  their workspaces, and smith's agents, together;
- **its own tiers below the domain,** with its protocol layers.

## 9. Budgets

jig's tests fit the suites' budgets (testing-strategy.md, section 8):
the focused suite runs a few cuts and seeds per scenario, the fuzzy
suite sweeps. While jig is built inside temper, its tests count against
temper's budgets; an application's conformance scenarios count against
the application's.

## 10. Open questions

- **How many cuts the focused suite takes** per scenario, and which: the
  commits around an effect, a claim and an answer are the likeliest to
  show a bug.
- **Coverage of the root's arms:** whether the harness should report
  which of the root's routes a suite never exercised, as transition
  coverage does for state machines.
