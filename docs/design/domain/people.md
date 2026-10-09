# Parties

Provisional, 2026-10-07. How people, and the services that act like
them, take part: who they are, what their role in a project lets them
do, what they ask of the engine and what waits for them, the chats they
hold with agents, the tasks they carry out themselves, and how they hear
of what waits. Parties are requesters and executors, as tasks are
(core.md, section 9); this document is that party in depth. The clients
they use are jig's client and the application's (`../README.md`,
section 10). What is still open is listed in section 12.

## 1. In one page

- **A party is a person or a service.** People talk to agents, set
  goals, accept proposals, answer questions, steer and watch through a
  client. A service does what its role allows through the same requests,
  with no conversation of its own.
- **Known by signing in through a provider of the application's.** The
  core keeps who a party is, as a provider and a subject there; it does
  not act on any system as them.
- **Roles are per project,** the core's own, possibly seeded from what a
  connector knows of people on an adopted resource. A role is an
  authority and the proposals it may decide (authority.md, section 6).
- **Requests are decisions.** Starting a chat, setting a goal, writing to
  a task, accepting a proposal, stopping a run: each is checked against
  the party's role, committed, and answered once durable, keyed so that a
  request sent twice is made once.
- **A party's inbox** holds what waits for them: questions, proposals,
  escalations, the tasks they carry out, results and replies. What is
  addressed to a role waits for every party in it until one acts.
- **A chat** is an agent's task whose requester is a person: each of
  their words wakes it, and it resumes its transcript.
- **The client is generic.** It knows tasks as a tree, conversations,
  what waits for a party, and live runs; not the shapes of work built
  from them. An application adds its own kinds of object.

## 2. Structure

The people child domain is a child of the core (engine.md, section 3), a
capability. It keeps:

- **parties:** the identities of those signed in or holding a role, and
  their sign-ins;
- **roles:** each live project's roles and who holds them, loaded at
  start and changed by commits;
- **requests in flight:** each party's pending requests, by their keys,
  until answered;
- **inboxes:** for each party with something waiting, what waits, by
  reference to the task or proposal it is (section 6);
- **live connections,** to which it pushes what changes in their
  inboxes, through the views (engine.md, section 11).

It knows nothing of tasks beyond their numbers and what the core tells
it waits for whom; every request goes to the core, which routes it to
authority and to the tasks.

## 3. Parties and identity

- **An identity is a provider and a subject.** A provider is one of the
  application's ways of signing in, numbered in its configuration: an
  OAuth server (a forge, a company's directory), or the deployment
  itself, for services' credentials. The subject is the provider's
  stable id for the party; a login and a name come with it, for display
  only.
- **Signing in** is the protocol layer's: it runs the provider's
  exchange and reads who the party is. The domain sees an authenticated
  identity, and the kind of party it is.
- **A party record** is made the first time someone signs in, and kept.
  Signing in gives the application no right to act on any system as that
  party: what the exchange returns is used to read who they are, and
  dropped (section 12 keeps the alternative open).
- **Sign-ins** are kept in the store, so a restart signs nobody out:
  their tokens, or digests of them, are written by the protocol layer
  into the store's secret records, and the domain names a sign-in only by
  its number. Each expires after a configured time, and a party may end
  theirs.
- **Services** are parties whose provider is the deployment: an owner
  makes one in a project, with a role, and the protocol layer issues its
  credential. A service makes requests (section 5) but starts no chat,
  and its inbox is read through the client's protocol, not shown.
- **The first owners** of a deployment are named in its configuration;
  everyone else gets a role from a project's owner, or by adoption's seed
  (section 4).

## 4. Roles

Each project has its roles, and its policy says what each may do
(authority.md, section 6). The default roles:

| Role | May |
|---|---|
| owner | everything a maintainer may; change the project's policy and roles; adopt resources; make services |
| maintainer | start and steer any task in the project; decide any proposal within the project's ceiling; release, cancel, stop |
| member | start chats and goals within the allotment their role gives; steer and cancel what they started; answer what is addressed to them |
| observer | read and watch |

- **A role is an authority** (authority.md, section 3): what a party in
  it may give a task they create, how much of the project's spend they
  may allot, and which kinds of proposal they may decide. The policy may
  narrow or widen each, and add roles of its own (the person on call),
  within the deployment's rules.
- **Seeded at adoption.** When a resource is adopted, its connector may
  say which parties it knows there and with what permission (a
  repository's collaborators, a team's members). The project's policy
  maps permissions to roles, and those parties are given them, unless
  they already have a role in the project. The party who made the project
  is its owner. After that, roles are the core's own: a permission that
  changes on the system does not change a role.
- **Proposals past the root** of a tree go to the role the policy names
  for that kind of proposal (tasks.md, section 9), maintainers by
  default.

## 5. What a party does

### 5.1 Requests

Every request is keyed by its client (a double click makes one request),
checked against the party's role (authority.md, 8.4), committed, and
answered once durable (engine.md, 5.2):

| Request | Needs | Becomes |
|---|---|---|
| start a chat | a person, member | a task with a chat's charter, its requester the person, its budget the role's default for chats (section 7) |
| set a goal | member, within their allotment; beyond it, a proposal | a tracked task, with the charter or procedure, budget and priority asked for |
| write to a task | the task in their tree, or maintainer | words in its inbox (tasks.md, 8.1) |
| answer | addressed to them or their role | an answer to a question, or a person task's result (section 8) |
| decide a proposal or an escalation | waiting for them, as the requester of its tree or in a role the policy names | accepted, if their authority covers it; rejected, with a reason; or passed up (tasks.md, section 9) |
| amend, cancel, release | the task in their tree, or maintainer | tasks.md, sections 5 and 7 |
| make themselves a task's requester | they may amend it | tasks.md, section 7 |
| stop a run | the task in their tree, or maintainer | its run cancelled and its task held for a party's stop |
| prioritise goals | maintainer | the goals' priorities amended |
| adopt a resource; change the policy, roles or services | owner | the project changed (5.2) |
| edit or delete a note | the note's scope within their role | the note revised (engine.md, section 10) |
| watch | observer | a live view (engine.md, section 11) |

- **A request refused** says why: the role, the authority it lacked, a
  limit, a task since ended.
- **Two parties deciding one thing:** the first commit decides; the
  second is told it was decided, by whom, and how.
- **Requests about an application's own objects** (an environment's end
  moved, an incident's page) are the application's: its client sends
  them, its root routes them to the connector they concern, and what they
  change in the core is a request of the table above.

### 5.2 The project itself

A project, its resources and roles, its homes and its policy are records
in the store that owners change through requests like any other. A
policy change applies to every decision after it, the decisions of tasks
already made included: a narrowing makes their grants beyond it
unusable, and a widening does not widen them. What was committed before
it is made, and pending proposals are judged against it when accepted
(authority.md, section 6, effective authority). An owner who wants work
in flight stopped cancels it.

## 6. A party's inbox

What waits for a party, newest first, each entry naming the task or
proposal it is:

- **questions** from tasks they asked for, chats included;
- **proposals** routed to them, or to a role they hold;
- **escalations** that reached them: a held task nobody below could
  decide;
- **person tasks** addressed to them, or to a role they hold (section 8);
- **results** of the tasks they asked for, and chats' replies they have
  not read.

- **Derived from the tasks,** not kept apart: an entry is there while its
  proposal is pending, its person task is active, its question is
  unanswered, or its result unread. The only state of the inbox's own is
  each party's read position over results and replies, committed when
  they read.
- **What is addressed to a role** is in the inbox of every party holding
  it, until one of them acts, when it leaves every one.
- **Bounded:** the people child domain holds each party's entries up to a
  limit; a client pages through the rest, loaded from the store.

## 7. Chats

- **A chat is an agent's task** whose requester is a person and whose
  charter is a chat's (core.md, section 9): the project's instructions
  for chats, its models, the tools its role gives, and a wake policy that
  wakes on the person's words and on its delegates' results.
- **Turn by turn, live.** The person's words wake it, or reach its live
  run at once; its replies stream to the client as the run reports them,
  and each turn is committed as it ends (engine.md, 7.2), so the
  conversation the client shows is the transcript in the store.
- **Waiting, then parked.** A chat waits for the next words holding its
  host's slot; idle past its charter's threshold it parks, and the next
  words resume it from its transcript, on any host its charter allows.
- **What a chat does:** answers; delegates; hands work to a procedure; or
  proposes a goal, which its person accepts in the conversation, as a
  proposal in their inbox, and which is theirs once accepted, not the
  chat's (tasks.md, section 9).
- **Closing.** A person closes a chat, which asks it to finish (an
  amendment it is woken by) and cancels it if it has not within a grace.
  A chat with live delegates is closed only once the person says what
  becomes of them: cancelled, or made their own (tasks.md, section 7).

## 8. Person tasks

A task whose executor is a party, or a role (core.md, 3.2):

- **Made by** an agent's `delegate` (a choice between two reports), a
  procedure (an approval it waits for), or another party.
- **Its contract** is a choice among the options its spec lists, an
  approval of a state its spec names, or an answer in words.
- **Shown** with its spec, the results it depends on, and what its
  contract asks; the party's answer is its result.
- **Addressed to a role,** it waits for any party holding it; one who
  takes it holds it, so two do not answer at once, and may hand it back.
  A service may hold a role a person task is addressed to, and answer it
  through its requests.
- **Stalled,** past its bound, it is news for its requester, which may
  amend, cancel or address it to someone else.

## 9. Notifications

- **In a client,** live: a signed-in party's inbox changes reach their
  open connections through the views, as they commit.
- **Beyond the client:** a page to the person on call, a mail, a chat
  system's message, each as a connector of the application's whose
  effects are messages to parties, keyed and made once like any other
  (connectors.md). The core says what waits for whom; the application's
  policy says which entries notify, and through which connector.

## 10. Below the domain

What a client's protocol layer, on the engine's side, owes this domain:

- **Requests and pages** over HTTP, **live streams** for views and
  inbox changes, as jig's client documents and the application's.
- **Signing in:** each provider's exchange, the sign-ins' tokens held by
  the protocol layer, named in the domain by a sign-in's number;
  services' credentials issued and checked there.
- **Rendering** what the domain carries as bytes: transcripts' turns,
  results, specs.
- **Keys** for requests, made by the client, carried with each request.

## 11. The world

The people child domain has its world, with its parent scripted. In the
core's world (engine.md, section 15), scripted parties sign in and act
through requests: a member starts a chat, which proposes a goal they
cannot fund, routed to a maintainer who accepts it; a person answers a
choice between two reports; two maintainers decide one proposal, and the
second is told; a person stops a run and releases it; an observer is
refused a request; a service sets a goal its role allows and is refused
a chat; a request sent twice is made once across a restart; roles seeded
from an adopted resource's parties.

Its referee: every request is answered once, after its commit; no
request is acted on beyond the party's role; what waits for a role
leaves every inbox once one party acts on it; a person's words reach
their task, or the task ends.

## 12. Open questions

- **Acting as a person:** whether an approval should also be posted on a
  system as the person who gave it, with the credential they signed in
  with, so that the system's own protection requires it independently.
  It needs that credential kept, which today is dropped.
- **People across projects:** a deployment-wide view of every inbox a
  party has, and roles that span projects.
- **Guests:** someone with no identity at any provider, invited to one
  chat.
