# The example application

Provisional, 2026-10-06. This is jig's example application, `ops`: a
small system for running services in production, on two connectors, with
its agents inside its engine. It exists to be tested and to be copied.

- Its root is the reference root every application starts from
  (`README.md`, 6.7).
- Its domain is the subject of jig's conformance world (`README.md`,
  6.6).
- As jig's second application, it is what keeps jig's vocabulary from
  being temper's in disguise.

What is still open is listed in section 11.

## 1. In one page

- **Production management, kept small.**
  - Agents watch services' logs and metrics, triage alerts, and run
    incidents.
  - Procedures restart, scale and provision.
  - People approve what is risky, and ask for environments in a chat.
- **Chosen for what it shares with temper, which is nothing:**
  - no forge, no code, no workspaces;
  - two connectors where temper has one;
  - a stream of events, mostly noise, where temper gets occasional
    hints;
  - effects on shared and scarce resources that cost money, where
    temper's are on objects it owns;
  - agents inside the engine, where temper's are on workers.
- **Two connectors:**
  - **observability:** services' logs, metrics and alerts;
  - **infrastructure:** services, environments and quotas.
- **Agents run inside the engine** (`README.md`, section 9, the second
  shape). Their tools are reads, so `ops` has no worker. One story has no
  agent at all (the first shape).
- **Its systems are fakes** first: one fake production that both
  connectors see. A real backend comes later, and may be the local machine's own
  services.
- **It stays an example.** It is small, its systems are fakes, and no
  application imports it. temper may one day drive production too, with
  connectors of its own.

## 2. Layout

```
examples/ops/domain                   jig-ops-domain                   the engine's root: the core and two connectors, routed
examples/ops/domain-observability     jig-ops-domain-observability     the observability connector
examples/ops/domain-infrastructure    jig-ops-domain-infrastructure    the infrastructure connector
examples/ops/client-domain            jig-ops-client-domain            the client's root: jig's client domain, incidents, environments
examples/ops/client-view              jig-ops-client-view              the views of incidents and environments
testing/jig-ops-fake-production       jig-ops-fake-production          the fake production: services, quotas, logs, metrics
tests/ops                             jig-ops-world                    its worlds, on jig's conformance world
```

The protocol layers, `iterate` and `main` come with a real backend
(section 9). Until then `ops` is its domains and its worlds, as an
application is before its protocol layer (`README.md`, section 11).

## 3. Its connectors

### 3.1 Observability

- **Resources:**
  - services, named by environment and service;
  - their log and metric streams;
  - alert rules.
- **Topics:** a service's alerts, and its health. The connector
  classifies each alert for each subscriber: it wakes the subscriber, is
  kept in its inbox, or is dropped. A storm of alerts reaches a task as
  one wake, batched by its wake policy.
- **Reads for agents:**
  - a service's logs, filtered, in a window of time;
  - a metric's series;
  - the alerts that fired.

  Each answer is bounded in bytes and charged to the connector's request
  budget.
- **Facts:** a service's error rate and health. They are read afresh
  before any decision that depends on them, including another
  connector's (3.3).
- **Effects:** silencing an alert for a while. It is keyed, and owned by
  the task that made it.

### 3.2 Infrastructure

- **Resources:**
  - services: their replicas, version, and state;
  - environments, named by pool and name;
  - quotas: a pool's capacity, a team's budget in money.
- **Effects:**
  - restarting a service;
  - scaling it;
  - rolling it back to a version;
  - creating an environment, which is a keyed creation;
  - tearing an environment down.
- **Write holds:** a service being remediated is held by the task that
  remediates it, so two incidents never restart it at once.
- **Scarce resources:** a pool's environments are a quota. A task that
  asks for a slot when the pool is full waits for one, rather than being
  refused (`domain/tasks.md`, section 6).
- **Procedures:**
  - **remediate:** make the effect, then wait until the service is
    healthy or a deadline passes;
  - **provision:** create the environment, then wait until it is ready;
  - **tear down:** at release, when the task holding an environment
    closes, the environment is torn down.

  Each is level-triggered: it decides from the facts as they are.
- **Drift:** someone restarts or scales a service by hand, or deletes an
  environment `ops` made. Each is found by reading afresh, and is news
  to the task that relies on it. Nothing is written over.

### 3.3 Between the two

The infrastructure connector's effects need observability's facts:

- a restart is verified by the service's health;
- a scale-down is allowed only while its load is below a threshold.

This is a requirement whose facts are one connector's while the effect
is another's (`domain/authority.md`, section 10). The policy names
observability as the judge of infrastructure's restarts and scale-downs;
the core asks it for its verdict through the root, and observability
judges from its own facts (`domain/connectors.md`, section 7).

## 4. Its root

```
jig-ops-domain                    the engine's root
├── core                          jig's core
├── observability                 jig-ops-domain-observability
├── infrastructure                jig-ops-domain-infrastructure
└── local host                    jig-local-host: smith's domain, one per run in flight (domain/hosts.md, section 5)
```

- **It has the one shape** of `README.md`, 6.3. Every write and every
  output goes into the journal.
- **It routes, and nothing else:**
  - between the core and each connector, in their vocabularies;
  - between the core and jig's local host, which hosts the agents;
  - the verdicts the core asks of observability on infrastructure's
    effects (3.3).
- **It is the reference root.** Its routes, its translations and its
  sum of worst cases are written to be copied, and are commented as
  such.

## 5. Its executors

- **Agents, in the engine.** Each charter has a model, reads, a budget
  and a result contract:
  - **triage:** a cheap model, with observability's reads; its result
    is a verdict, noise or incident, with a summary;
  - **incident lead:** a coordinator, with both connectors' reads; it
    delegates remediation and finishes with a report;
  - **environment chat:** a person's assistant, which plans an
    environment and asks for it.
- **Procedures:**
  - observability's **watch:** a standing task subscribed to a set of
    services' alerts, which makes a triage task from a template for each
    batch that wakes it;
  - infrastructure's **remediate, provision and tear down** (3.2);
  - the core's **recurring** tasks.
- **People:**
  - the person on call, who approves effects beyond policy and answers
    questions during an incident;
  - developers, who ask for environments.

## 6. Its policy

- **Staging:** agents' tasks may restart and scale.
- **Production:** every effect is beyond any agent's authority, so it
  becomes a proposal that the person on call accepts.
- **Money:** each team has a budget per month, spent by environments,
  and a person's pool within it.

These are policies as data, in authority's terms (`domain/authority.md`). `ops`
writes no rule of its own in code.

## 7. Its stories

### 7.1 An alert

```
P, the person on call
└─ W   watch the shop's services       procedure: observability's watch, subscribed to alerts
   ├─ T1  triage alerts 7–9            agent, triage: noise
   ├─ T2  triage alert 12              agent, triage: incident
   └─ I   incident: checkout errors    agent, an incident lead; tracked
      └─ R  restart checkout           infrastructure's remediate, holding the checkout service
                                       in production, so a proposal P accepted
```

- **The storm** of alerts 7 to 9 wakes W once. T1 reads the logs, finds
  a known, harmless error, and finishes: noise.
- **Alert 12** is triaged as an incident. W makes I, a tracked task.
  I reads logs and metrics, and decides on a restart.
- **The restart is in production,** beyond I's authority. It becomes a
  proposal. It passes over W, a procedure, to P, who accepts it.
- **R holds the service,** restarts it, and waits on observability's
  health (3.3). The service recovers. R's result reaches I, which
  finishes with a report. P reads it in the client.

### 7.2 A request

```
Q, a developer
└─ C   a chat with Q                   agent, an environment chat
   └─ E  staging for payments, a week  infrastructure's provision; holds a slot of the staging pool
                                       beyond C's budget, so a proposal Q accepted, funded from Q's pool
```

- **Q asks C** for a staging environment for a week. C plans its size
  and cost, and asks for E.
- **E's cost** is beyond C's budget. Q accepts it, and it is funded from
  Q's pool.
- **E provisions:** the creation is keyed, so a restart in the middle
  finds the environment rather than creating it twice. E waits until the
  environment is ready, then tells C, which tells Q.
- **A week later** a timer E subscribed to wakes it, and E finishes. Its
  closing releases the environment, and the release tears it down.

### 7.3 The others

- **A night with no agent:** a recurring task scales staging down at
  night, if its load allows (3.3), and up in the morning. This is the
  first shape of `README.md`, section 9: procedures only.
- **A queue for the pool:** two developers ask for an environment when
  one slot is left. The second request waits for the slot, and is
  provisioned once the first environment is torn down.
- **A hand on the service:** while R's proposal waits for P, someone
  restarts the service by hand. R reads afresh, finds it healthy, and
  finishes without restarting it.
- **An environment deleted by hand:** E's environment disappears. E is
  held and the client says why. Nothing is recreated without a decision.
- **Crashes at every commit:** jig's conformance world runs every story
  above, crashing the engine at each commit and failing commits. The
  referee checks jig's promises from outside: each creation was made
  once, each restart was made only after its proposal's acceptance was
  durable, and every effect was within authority.

## 8. Its client

- **jig's pages,** for tasks, conversations, proposals and a person's
  inbox, plus two kinds of object of its own:
  - incidents: their state, their timeline, the service's health;
  - environments: their pool, their cost, their end.
- **A person in the worlds** is jig's fake person, reading the view tree.
- **A terminal client** later, to show a client that is not a browser
  (`README.md`, section 10).

## 9. Its systems

- **One fake production, seen by both connectors,** so that restarting a
  service through infrastructure's API changes what observability's API
  reports:
  - services that log and emit metrics from a seeded model: baseline
    noise, injected incidents, recovery after a remediation;
  - pools with quotas, and environments that take time to be ready.
- **Its faults,** drawn from the seed:
  - API errors;
  - timeouts whose effect was made but whose answer was lost;
  - quotas exhausted;
  - slow provisioning;
  - drift: hands restarting and deleting.
- **A real backend later.** On Linux, a machine's own services would do:
  - logs from journald;
  - restarts and scaling through systemd units;
  - environments as local containers.

  No cloud account is needed. It comes with the protocol layers, the
  service and `main`.

## 10. Its worlds

- **Each connector's world,** with the fake production below it and the
  root scripted above it, as for any connector (`domain/connectors.md`).
- **The root's world:** the whole domain on jig's conformance world.
  Agents are smith's domain over skein's fake LLM, with scripts per
  charter; people are scripted; the store is skein-kv in memory.
- **Budgets:** its tests fit within the application's share of the
  suites' time (`domain/testing.md`, section 9).

## 11. Open questions

- **The watch:** whether it is a procedure that makes triage tasks, as
  section 5 has it, or a coordinator agent, woken rarely, that triages
  itself.
- **Triage before an LLM:** whether the connector's classification,
  with a wake policy batching what it lets through, is enough before a
  cheap model triages.
