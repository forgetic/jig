//! One engine, one ordered store and independently scripted neighbours.
//! Applications translate boundaries; this loop owns commit application and
//! checks observations after every iteration (`domain/testing.md`, 5–6).
use crate::referee::{Observed, Policy, Recovery, Referee, Violation};
use jig_fake_store::{Store, Write};
use std::collections::VecDeque;
use std::fmt::Debug;

/// Simulated time supplied to every application and fake.
#[derive(Clone, Copy, Debug)]
pub struct Clock {
    pub now: u64,
}

/// An input from a peer or the ordered store.
#[derive(Debug)]
pub enum Input<E, R> {
    /// An application's translated peer input.
    Event(E),
    /// One durable transaction's completion.
    Committed(u64),
    /// A transaction failed; later outputs must stop.
    Failed(u64),
    /// A row returned by the core's requested cold-load script.
    Restore(R),
}

/// What the application's journal released, without interpreting its records.
#[derive(Debug)]
pub enum Output<K, R, D> {
    /// One whole decision submitted to the store.
    Commit { number: u64, writes: Box<[Write<K, R>]> },
    /// A released peer output or a core-scripted load.
    Deliver(D),
    /// The failed store stopped the engine.
    Stop,
}

/// The concrete engine boundary and its outside translations. Implementations
/// keep system observations in the fake; no method observes engine state.
/// Configuration is reconstructed on cold start, never copied from live state.
pub trait Application {
    /// The concrete application engine root.
    type Domain;
    /// Scenario configuration retained for cold starts.
    type Config;
    /// Independent system fakes.
    type Systems;
    /// Scripted hosts, parties and outside correlation state.
    type Peers;
    /// Application store addresses.
    type Key: Ord + Clone + Debug;
    /// Application durable rows.
    type Record: Clone + Debug;
    /// Application boundary events.
    type Event: Debug;
    /// Application released outputs.
    type Delivery: Debug;

    /// Build the engine from its original scenario configuration.
    fn build(config: &Self::Config) -> Self::Domain;
    /// Construct independent system fakes from the scenario seed.
    fn systems(config: &Self::Config, seed: u64) -> Self::Systems;
    /// Construct scripted hosts, parties and observation translations.
    fn peers(config: &Self::Config, seed: u64) -> Self::Peers;
    /// Read the scenario policy, including independently requested edits.
    fn policy(peers: &Self::Peers) -> Policy;
    /// Declare each configured effect kind’s recovery class.
    fn recovery(config: &Self::Config, connector: u16, kind: u16) -> Recovery;
    /// Prepare the initial durable fixture and begin the cold-load script.
    fn start(config: &Self::Config, store: &mut Store<Self::Key, Self::Record>) -> Vec<Self::Event>;
    /// Reconnect retained peers and observe the last durable store on a crash.
    fn cold(peers: &mut Self::Peers, store: &Store<Self::Key, Self::Record>, clock: Clock);
    /// Translate the application’s fleet, task and connector timers.
    fn timers(config: &Self::Config) -> Vec<Self::Event>;
    /// Feed one event or store row to the concrete engine.
    fn step(domain: &mut Self::Domain, config: &Self::Config, clock: Clock, input: Input<Self::Event, Self::Record>);
    /// Drain the journal in release order without changing its payloads.
    fn release(
        domain: &mut Self::Domain,
        config: &Self::Config,
        clock: Clock,
    ) -> Vec<Output<Self::Key, Self::Record, Self::Delivery>>;
    /// Record a peer’s request before the root receives it.
    fn inbound(peers: &mut Self::Peers, store: &Store<Self::Key, Self::Record>, clock: Clock, input: &mut Self::Event);
    /// Translate a released output to the peer that owns it.
    fn deliver(
        peers: &mut Self::Peers,
        systems: &mut Self::Systems,
        store: &mut Store<Self::Key, Self::Record>,
        clock: Clock,
        delivery: Self::Delivery,
    ) -> Vec<Input<Self::Event, Self::Record>>;
    /// Advance fakes and return their independently produced events.
    fn poll(
        peers: &mut Self::Peers,
        systems: &mut Self::Systems,
        store: &mut Store<Self::Key, Self::Record>,
        clock: Clock,
        ready: bool,
    ) -> Vec<Input<Self::Event, Self::Record>>;
    /// Map new system, host and durable-store evidence in arrival order.
    fn observed(
        peers: &mut Self::Peers,
        systems: &Self::Systems,
        store: &Store<Self::Key, Self::Record>,
        clock: Clock,
    ) -> Vec<Observed>;
    /// Report whether the root’s restart script has opened admission.
    fn ready(domain: &Self::Domain) -> bool;
    /// Report whether the root has immediate work remaining.
    fn quiescent(domain: &Self::Domain) -> bool;
    /// Run the root’s iteration reclamation point.
    fn reclaim(domain: &mut Self::Domain);
    /// Declare the engine’s worst-case heap before construction.
    fn maximum(config: &Self::Config) -> u64;
    /// Price ownership transferred from pre-existing configuration.
    fn configuration_heap(config: &Self::Config) -> u64;
}

/// A finite run's evidence, retained independently of the engine.
#[derive(Debug)]
pub struct Outcome {
    pub commits: u64,
    pub iterations: u64,
    pub stopped: bool,
    pub crashes: Vec<u64>,
}

/// A chosen crash just after durable application and before its acknowledgement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cut {
    /// Run without a selected crash.
    None,
    /// Crash after this numbered transaction becomes durable.
    AfterCommit(u64),
    /// Draw one cut from commit arrivals, replayable from its seed.
    Random { seed: u64 },
}

/// The application engine, its independent peers, and durable store.
/// The heap meter begins before engine construction; counting the test peers
/// too is a conservative engine bound. Configuration ownership is added back.
pub struct Harness<A: Application> {
    pub domain: A::Domain,
    pub systems: A::Systems,
    pub peers: A::Peers,
    pub store: Store<A::Key, A::Record>,
    pub referee: Referee,
    pub trace: Vec<String>,
    config: A::Config,
    events: VecDeque<Input<A::Event, A::Record>>,
    outputs: Vec<Output<A::Key, A::Record, A::Delivery>>,
    meter: skein_world::domain::heap::Meter,
    clock: Clock,
    iterations: u64,
    stopped: bool,
    cut: Cut,
    crashes: Vec<u64>,
}

impl<A: Application> Harness<A> {
    /// Construct the scenario's concrete engine and independent neighbours.
    #[must_use]
    pub fn new(config: A::Config, seed: u64) -> Self {
        let peers = A::peers(&config, seed);
        let systems = A::systems(&config, seed);
        let mut store = Store::new();
        let events = A::start(&config, &mut store).into_iter().map(Input::Event).collect();
        let policy = A::policy(&peers);
        for (&(connector, kind), entry) in &policy.kinds {
            assert_eq!(entry.recovery, A::recovery(&config, connector, kind), "declared recovery class");
        }
        let meter = skein_world::domain::heap::Meter::new();
        let domain = A::build(&config);
        Self {
            domain,
            systems,
            peers,
            store,
            referee: Referee::new(policy),
            trace: Vec::new(),
            config,
            events,
            outputs: Vec::new(),
            meter,
            clock: Clock { now: 0 },
            iterations: 0,
            stopped: false,
            cut: Cut::None,
            crashes: Vec::new(),
        }
    }

    /// Arm one cold restart before its selected durable completion is released.
    pub fn cut(&mut self, cut: Cut) {
        self.cut = cut;
    }

    /// Advance simulated time and fire every application timer through its root.
    pub fn advance(&mut self, nanos: u64) {
        self.clock.now = self.clock.now.checked_add(nanos).expect("scenario time fits");
        self.events.extend(A::timers(&self.config).into_iter().map(Input::Event));
    }

    /// Current time for independently scripted peer faults.
    #[must_use]
    pub fn clock(&self) -> Clock {
        self.clock
    }

    /// Restart cold from durable rows, preserving peer and system evidence.
    ///
    /// # Errors
    /// Returns a receipt missing after restart or another broken promise.
    pub fn crash(&mut self) -> Result<(), Violation> {
        self.crashes.push(self.store.applied);
        self.referee.observe(self.clock.now, Observed::Restart)?;
        self.store.crash();
        self.events.clear();
        self.outputs.clear();
        A::cold(&mut self.peers, &self.store, self.clock);
        self.inspect()?;
        self.domain = A::build(&self.config);
        self.events.extend(A::start(&self.config, &mut self.store).into_iter().map(Input::Event));
        self.stopped = false;
        Ok(())
    }

    /// Release held durable acknowledgements in the order the store applied them.
    pub fn release_commits(&mut self) {
        while let Some(number) = self.store.release_held() {
            self.events.push_back(Input::Committed(number));
        }
    }

    /// Queue one externally supplied scenario event.
    pub fn send(&mut self, event: A::Event) {
        self.events.push_back(Input::Event(event));
    }

    /// Advance one bounded iteration, retaining peer replies for later steps.
    /// Returns whether another iteration can make progress.
    ///
    /// # Errors
    /// Returns the first promise broken by outside observations.
    pub fn iterate(&mut self) -> Result<bool, Violation> {
        if self.stopped {
            return Ok(false);
        }
        self.iterations += 1;
        self.clock.now = self.clock.now.checked_add(1_000_000).expect("scenario time fits");
        self.referee.observe(self.clock.now, Observed::Tick)?;
        if let Some(mut input) = self.events.pop_front() {
            if let Input::Event(event) = &mut input {
                A::inbound(&mut self.peers, &self.store, self.clock, event);
            }
            self.inspect()?;
            self.trace.push(format!("input {input:?}"));
            A::step(&mut self.domain, &self.config, self.clock, input);
        }
        self.outputs = A::release(&mut self.domain, &self.config, self.clock);
        let empty = self.outputs.is_empty();
        for output in std::mem::take(&mut self.outputs) {
            self.trace.push(format!("output {output:?}"));
            match output {
                Output::Commit { number, writes } => self.store.submit(number, writes, 0),
                Output::Deliver(delivery) => {
                    self.events.extend(A::deliver(
                        &mut self.peers,
                        &mut self.systems,
                        &mut self.store,
                        self.clock,
                        delivery,
                    ));
                    self.inspect()?;
                }
                Output::Stop => self.stopped = true,
            }
        }
        if !self.stopped {
            let previous = self.store.applied;
            match self.store.tick(false) {
                Ok(Some(number)) => self.events.push_front(Input::Committed(number)),
                Ok(None) => {}
                Err(number) => self.events.push_front(Input::Failed(number)),
            }
            self.inspect()?;
            if self.store.applied != previous {
                let should_cut = match self.cut {
                    Cut::None => false,
                    Cut::AfterCommit(number) => number == self.store.applied,
                    Cut::Random { seed } => {
                        let mut random = skein_lib::Rng::new(seed ^ self.store.applied);
                        random.below(4) == 0
                    }
                };
                if should_cut {
                    self.cut = Cut::None;
                    self.crash()?;
                    return Ok(true);
                }
            }
            self.events.extend(A::poll(
                &mut self.peers,
                &mut self.systems,
                &mut self.store,
                self.clock,
                A::ready(&self.domain),
            ));
            self.inspect()?;
        }
        A::reclaim(&mut self.domain);
        self.referee.observe(
            self.clock.now,
            Observed::Heap {
                held: self.meter.held().checked_add(A::configuration_heap(&self.config)).expect("engine heap fits"),
                maximum: A::maximum(&self.config),
            },
        )?;
        Ok(!self.stopped
            && !(empty
                && self.events.is_empty()
                && self.store.pending.is_empty()
                && !self.store.loading()
                && (A::quiescent(&self.domain) || !self.store.held.is_empty())))
    }

    fn inspect(&mut self) -> Result<(), Violation> {
        let observations = A::observed(&mut self.peers, &self.systems, &self.store, self.clock);
        self.referee.policy = A::policy(&self.peers);
        for observed in observations {
            self.referee.observe(self.clock.now, observed)?;
        }
        Ok(())
    }

    /// Run until all immediate work settles, within the scenario's finite bound.
    ///
    /// # Errors
    /// Returns a broken promise or a story that exceeded its bound.
    pub fn drain(&mut self) -> Result<Outcome, Violation> {
        for _ in 0..self.referee.policy.story_steps {
            if !self.iterate()? {
                return Ok(self.outcome());
            }
        }
        Err(Violation {
            promise: crate::referee::Promise::Bounded,
            why: "scenario did not settle within its iteration bound".into(),
        })
    }

    /// Counts from the outside store and loop.
    #[must_use]
    pub fn outcome(&self) -> Outcome {
        Outcome {
            commits: self.store.applied,
            iterations: self.iterations,
            stopped: self.stopped,
            crashes: self.crashes.clone(),
        }
    }
}
