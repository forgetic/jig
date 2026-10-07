use std::collections::{BTreeMap, BTreeSet, VecDeque};

use jig_core_brief::{
    GatherDomain, GatherEvent, GatherPlaced, GatherRequest, Limits, gather_fire, gather_max_out, gather_step,
};
use skein_lib::{Env, Queue, Time, Token, Wall};

pub const LIMITS: Limits = Limits { briefs: 3, sections: 5, read_bytes: 128, brief_bytes: 192 };

/// The child, a clock, and connector-owned bytes outside the child.
pub struct World {
    pub env: Env<Limits>,
    pub domain: GatherDomain,
    out: Queue<GatherRequest>,
    held: BTreeMap<Token, Vec<u8>>,
    paused: BTreeSet<Token>,
    closed: BTreeSet<Token>,
    brief: Token,
}

impl World {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            env: Env { now: Time::ZERO, wall: Wall::EPOCH, limits },
            domain: GatherDomain::new(&limits),
            out: Queue::with_capacity(gather_max_out(&limits)),
            held: BTreeMap::new(),
            paused: BTreeSet::new(),
            closed: BTreeSet::new(),
            brief: Token::new(1),
        }
    }

    /// Give the fake connector a section before it is asked to gather it.
    pub fn put(&mut self, token: Token, bytes: &[u8]) {
        assert!(self.held.insert(token, bytes.to_vec()).is_none(), "token used once");
    }

    /// Leave a gather outstanding until the test advances the clock or answers it.
    pub fn pause(&mut self, token: Token) {
        self.paused.insert(token);
    }

    #[must_use]
    pub fn held(&self, token: Token) -> bool {
        self.held.contains_key(&token)
    }

    #[must_use]
    pub fn closed(&self, token: Token) -> bool {
        self.closed.contains(&token)
    }

    /// Step one event and return the requests without answering them.
    pub fn step(&mut self, event: GatherEvent) -> Vec<GatherRequest> {
        if let GatherEvent::Plan { brief, .. } = &event {
            self.brief = *brief;
        }
        gather_step(&mut self.domain, &self.env, event, &mut self.out);
        self.drain()
    }

    /// Fire one due deadline and return the requests without answering them.
    pub fn fire(&mut self, now: Time) -> Vec<GatherRequest> {
        self.env.now = now;
        gather_fire(&mut self.domain, &self.env, &mut self.out);
        self.drain()
    }

    fn drain(&mut self) -> Vec<GatherRequest> {
        let mut drained = Vec::new();
        while let Some(request) = self.out.pop() {
            drained.push(request);
        }
        drained
    }

    /// Serve all handoffs with the fake connector and return terminal requests.
    pub fn settle(&mut self, starts: Vec<GatherRequest>) -> Vec<GatherRequest> {
        let mut pending: VecDeque<GatherRequest> = starts.into();
        let mut terminal = Vec::new();
        let mut steps = 0_u32;
        while let Some(request) = pending.pop_front() {
            steps += 1;
            assert!(steps < 100, "bounded scripted brief");
            let answer = match request {
                GatherRequest::Gather { section, .. } => {
                    if self.paused.contains(&section) {
                        continue;
                    }
                    match self.held.get(&section) {
                        Some(bytes) => GatherEvent::Ready {
                            brief: self.brief,
                            section,
                            size: u32::try_from(bytes.len()).expect("bounded fake bytes"),
                        },
                        None => GatherEvent::Missing { brief: self.brief, section },
                    }
                }
                GatherRequest::CutTo { section, size, .. } => {
                    let bytes = self.held.get_mut(&section).expect("cut held section");
                    bytes.truncate(usize::try_from(size).expect("bounded size"));
                    GatherEvent::Cut {
                        brief: self.brief,
                        section,
                        size: u32::try_from(bytes.len()).expect("bounded fake bytes"),
                    }
                }
                GatherRequest::Drop { section, .. } => {
                    self.held.remove(&section);
                    assert!(self.closed.insert(section), "section closed twice");
                    continue;
                }
                GatherRequest::Complete { brief, order } => {
                    for placed in &order {
                        if let GatherPlaced::Connector { token, size, .. } = placed {
                            let bytes = self.held.remove(token).expect("take held section");
                            assert_eq!(bytes.len(), usize::try_from(*size).expect("bounded size"));
                            assert!(self.closed.insert(*token), "section closed twice");
                        }
                    }
                    terminal.push(GatherRequest::Complete { brief, order });
                    continue;
                }
                GatherRequest::Failed { .. } | GatherRequest::Refused { .. } => {
                    terminal.push(request);
                    continue;
                }
            };
            pending.extend(self.step(answer));
        }
        terminal
    }
}
