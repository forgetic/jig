//! The scripted workers (domain/hosts.md, sections 2 and 4, as the fleet
//! meets them): each dials in, says hello with its slots, the workstreams its
//! workspaces hold, the runs it hosts and the answers it keeps, then sends
//! those answers again; hosts what it is assigned, refusing as busy beyond its
//! slots or for a run it hosts another attempt of, and as invalid by chance;
//! runs each to its planned end, or winds it down once cancelled; relays host
//! calls and withdraws them past their deadline; bounces inbound events by
//! chance and tells facts; keeps each answer until it is acknowledged; and,
//! out of contact past its own grace, cancels its runs itself.

use jig_core_fleet::Phase;

use crate::referee::{Kind, Said, Seen};
use crate::world::{Back, Channel, Delivery, Hosting, Message, Up, World};

/// The workstreams a worker's workspaces hold at most.
const CACHE: usize = 3;

impl World {
    /// Sends `up` from the worker of `channel`, after what it sent before.
    fn say(&mut self, channel: u64, up: Up) {
        let latency = self.settings.latency.draw(&mut self.rng);
        let entry = self.channels.get_mut(&channel).expect("a worker says things on its own channel");
        let at = self.now.saturating_add(latency).max(entry.up_at);
        entry.up_at = at;
        self.send(at, Delivery::Up { channel, up });
    }

    /// The worker's channel, if it knows it open.
    fn channel_of(&self, worker: usize) -> Option<u64> {
        self.workers[worker].channel
    }

    /// A message from the fleet reaches the worker on `channel`.
    pub(crate) fn receive(&mut self, worker: usize, channel: u64, message: Message) {
        match message {
            Message::Assign { run, attempt } => self.assigned(worker, channel, run, attempt),
            Message::Inbound { run, attempt } => {
                let live = match self.workers[worker].runs.get(&(run, attempt)) {
                    Some(hosting) => !hosting.cancelled,
                    None => false,
                };
                if live && self.rng.chance(self.settings.bounces) {
                    self.say(channel, Up::Bounced { run, attempt });
                }
            }
            Message::Cancel { run, attempt } => {
                if let Some(hosting) = self.workers[worker].runs.get_mut(&(run, attempt))
                    && !hosting.cancelled
                {
                    hosting.cancelled = true;
                    self.after(self.settings.wind_down, Delivery::Stops { worker, run, attempt });
                }
            }
            Message::Relayed { run, attempt, call } => {
                if let Some(hosting) = self.workers[worker].runs.get_mut(&(run, attempt)) {
                    hosting.calls.retain(|waiting| *waiting != call);
                }
            }
            Message::Acknowledge { run, attempt } => {
                if self.workers[worker].held.remove(&(run, attempt)).is_some() {
                    self.observe(Seen::Forgot { run, attempt });
                }
            }
        }
    }

    /// An assignment: refused as busy beyond the worker's slots or for a run
    /// it hosts another attempt of, refused as invalid by chance, or hosted.
    fn assigned(&mut self, worker: usize, channel: u64, run: u64, attempt: u64) {
        let entry = &self.workers[worker];
        if entry.runs.contains_key(&(run, attempt)) || entry.held.contains_key(&(run, attempt)) {
            // The attempt hosted, assigned again: its one answer is the
            // hosted run's.
            return;
        }
        let slots = entry.slots;
        let hosting = u32::try_from(entry.runs.len() + entry.held.len()).expect("a few runs");
        let full = hosting >= slots || entry.runs.keys().any(|(other, _)| *other == run);
        // A worker hosting a run may be fuller than it said, by its own
        // measure: it frees a slot as that run ends.
        let busy = full || (!entry.runs.is_empty() && self.rng.chance(self.settings.busy));
        let invalid = !busy && self.rng.chance(self.settings.invalid);
        let admitted = !busy && !invalid;
        self.observe(Seen::Assigned {
            kind: jig_core_fleet::HostKind::Worker,
            worker,
            slots,
            hosting,
            run,
            attempt,
            admitted,
        });
        if !admitted {
            // A refusal goes once, and keeps nothing.
            let kind = if busy { Kind::Busy } else { Kind::Invalid };
            let said = Said { kind, nonce: self.wire.name() };
            if !busy {
                // A busy refusal is not the attempt's answer: it is assigned
                // again.
                self.observe(Seen::Answered { run, attempt, said });
            }
            self.say(channel, Up::Answer { run, attempt, said });
            return;
        }
        self.stats.admitted += 1;
        self.admitted.insert((run, attempt));
        let plan = if self.rng.chance(self.settings.parks) {
            Kind::Parked
        } else if self.rng.chance(self.settings.fails) {
            Kind::Failed
        } else {
            Kind::Ended
        };
        let entry = &mut self.workers[worker];
        entry.runs.insert((run, attempt), Hosting { plan, cancelled: false, calls: Vec::new() });
        entry.cache.retain(|cached| *cached != run);
        entry.cache.push_back(run);
        if entry.cache.len() > CACHE {
            entry.cache.pop_front();
        }
        self.after(self.settings.run, Delivery::Ends { worker, run, attempt });
        self.after(self.settings.call_gap, Delivery::Call { worker, run, attempt });
    }

    /// The run ends as it planned, unless it has ended already.
    pub(crate) fn ends(&mut self, worker: usize, run: u64, attempt: u64) {
        let Some(hosting) = self.workers[worker].runs.get(&(run, attempt)) else {
            return;
        };
        let kind = hosting.plan;
        self.finish(worker, run, attempt, kind);
    }

    /// The run, cancelled, has wound down, unless it ended first.
    pub(crate) fn stops(&mut self, worker: usize, run: u64, attempt: u64) {
        if self.workers[worker].runs.contains_key(&(run, attempt)) {
            self.finish(worker, run, attempt, Kind::Failed);
        }
    }

    /// The run is gone: its answer is kept until the engine acknowledges it,
    /// and sent at once if the worker is in contact.
    fn finish(&mut self, worker: usize, run: u64, attempt: u64, kind: Kind) {
        self.workers[worker].runs.remove(&(run, attempt));
        let said = Said { kind, nonce: self.wire.name() };
        self.workers[worker].held.insert((run, attempt), said);
        self.observe(Seen::Answered { run, attempt, said });
        if let Some(channel) = self.channel_of(worker) {
            self.say(channel, Up::Answer { run, attempt, said });
        }
    }

    /// The live run makes a host call, if the worker is in contact, and with
    /// it a fact by chance; then its next call after a while.
    pub(crate) fn calls_up(&mut self, worker: usize, run: u64, attempt: u64) {
        let live = match self.workers[worker].runs.get(&(run, attempt)) {
            Some(hosting) => !hosting.cancelled,
            None => false,
        };
        if !live {
            return;
        }
        if let Some(channel) = self.channel_of(worker) {
            let call = self.wire.name();
            let hosting = self.workers[worker].runs.get_mut(&(run, attempt)).expect("live above");
            hosting.calls.push(call);
            self.say(channel, Up::Relay { run, attempt, call });
            let at = self.now.saturating_add(self.settings.call_wait);
            self.send(at, Delivery::Withdraw { worker, run, attempt, call });
            if self.rng.chance(self.settings.facts) {
                self.say(channel, Up::Told { run, attempt });
            }
        }
        self.after(self.settings.call_gap, Delivery::Call { worker, run, attempt });
    }

    /// The run withdraws its call past its own deadline for it.
    pub(crate) fn withdraw(&mut self, worker: usize, run: u64, attempt: u64, call: u64) {
        if let Some(hosting) = self.workers[worker].runs.get_mut(&(run, attempt)) {
            hosting.calls.retain(|waiting| *waiting != call);
        }
    }

    /// The worker notices its channel closed: its grace starts, and it dials
    /// again, or never.
    pub(crate) fn noticed(&mut self, worker: usize, channel: u64) {
        let entry = &mut self.workers[worker];
        if entry.channel != Some(channel) {
            return;
        }
        entry.channel = None;
        entry.epoch += 1;
        let epoch = entry.epoch;
        let back = entry.back;
        entry.back = Back::Soon;
        let at = self.now.saturating_add(self.settings.worker_grace);
        self.send(at, Delivery::Grace { worker, epoch });
        match back {
            Back::Soon => self.after(self.settings.redial, Delivery::Dial { worker }),
            Back::After(after) => self.send(self.now.saturating_add(after), Delivery::Dial { worker }),
            Back::Never => self.workers[worker].gone = true,
        }
    }

    /// Out of contact past its grace: the worker cancels its runs itself.
    pub(crate) fn grace(&mut self, worker: usize, epoch: u64) {
        let entry = &mut self.workers[worker];
        if entry.epoch != epoch || entry.channel.is_some() {
            return;
        }
        let mut stopping = Vec::new();
        for (&(run, attempt), hosting) in &mut entry.runs {
            if !hosting.cancelled {
                hosting.cancelled = true;
                stopping.push((run, attempt));
            }
        }
        for (run, attempt) in stopping {
            self.after(self.settings.wind_down, Delivery::Stops { worker, run, attempt });
        }
    }

    /// The worker dials: a new channel opens, and it says hello, then sends
    /// again every answer it keeps.
    pub(crate) fn dial(&mut self, worker: usize) {
        if self.workers[worker].gone || self.workers[worker].channel.is_some() {
            return;
        }
        let channel = self.wire.name();
        let now = self.now;
        self.channels.insert(channel, Channel { worker, open: true, epoch: self.epoch, up_at: now, down_at: now });
        let entry = &mut self.workers[worker];
        entry.channel = Some(channel);
        entry.epoch += 1;
        let mut hosting = Vec::new();
        for (&(run, attempt), hosted) in &entry.runs {
            let phase = if hosted.cancelled { Phase::Ending } else { Phase::Active };
            hosting.push((run, attempt, phase));
        }
        for &(run, attempt) in entry.held.keys() {
            hosting.push((run, attempt, Phase::Answered));
        }
        let held: Vec<((u64, u64), Said)> = entry.held.iter().map(|(&names, &said)| (names, said)).collect();
        let hello = Up::Hello { slots: entry.slots, workstreams: entry.cache.iter().copied().collect(), hosting };
        self.stats.hellos += 1;
        self.say(channel, hello);
        for ((run, attempt), said) in held {
            self.say(channel, Up::Answer { run, attempt, said });
        }
    }
}
