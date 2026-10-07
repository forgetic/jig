use jig_core_people::{
    Ask, Domain, Event, Holding, Identity, IdentityKey, Limits, Outcome, Request, Role, fire, max_out, step, worst_case,
};
use jig_people_world::LIMITS;
use skein_lib::{Env, Queue, ReplyTo, Time, Token, Wall};
use skein_world::domain::heap::{self, Meter};

#[global_allocator]
static HEAP: heap::Counting = heap::Counting;

struct Measured {
    domain: Domain,
    env: Env<Limits>,
    out: Queue<Request>,
    meter: Meter,
    bound: u64,
    call: u64,
}

impl Measured {
    fn new(limits: Limits) -> Measured {
        let out = Queue::with_capacity(max_out(&limits));
        let meter = Meter::new();
        let domain = Domain::new(&limits, Box::new([]), 1);
        Measured {
            domain,
            env: Env { now: Time::ZERO, wall: Wall::EPOCH, limits },
            out,
            meter,
            bound: worst_case(&limits).expect("test values are admitted and fit"),
            call: 0,
        }
    }

    fn to(&mut self) -> ReplyTo {
        self.call += 1;
        ReplyTo::new(Token::new(self.call))
    }

    fn event(&mut self, event: Event) -> Option<Token> {
        self.meter.start();
        step(&mut self.domain, &self.env, event, &mut self.out);
        self.drain()
    }

    fn drain(&mut self) -> Option<Token> {
        let measured = self.meter.end();
        let mut routed = None;
        while let Some(request) = self.out.pop() {
            if let Request::Route { request, .. } = request {
                routed = Some(request);
            }
        }
        self.meter.check(measured, self.bound, &self.env.limits);
        self.domain.reclaim();
        routed
    }

    fn ask(&mut self, key: u8) -> Option<Token> {
        let reply_to = self.to();
        let words = vec![1; usize::try_from(self.env.limits.words).expect("test values are admitted and fit")]
            .into_boxed_slice();
        self.event(Event::Ask { reply_to, sign_in: 1, key: [key; 16], ask: Ask::StartChat { project: 1, words } })
    }
}

#[test]
fn full_people_roles_signins_pending_waiters_answers_and_expiry_fit() {
    for limits in [
        LIMITS,
        Limits {
            people: 1,
            sign_ins: 1,
            projects: 1,
            holdings: 1,
            requests: 2,
            pending: 1,
            waiters: 1,
            facts: 0,
            ..LIMITS
        },
    ] {
        let mut test = Measured::new(limits);
        test.event(Event::Restored);
        for number in 1..=u64::from(limits.people) {
            let reply_to = test.to();
            let identity = Identity {
                key: IdentityKey { provider: 0, subject: (number).to_be_bytes().into() },
                login: vec![
                    1;
                    usize::try_from(limits.identity_bytes).expect("test values are admitted and fit")
                        - size_of::<u64>()
                ]
                .into_boxed_slice(),
                name: Box::new([]),
            };
            test.event(Event::SignedIn {
                reply_to,
                person: number,
                sign_in: number,
                identity,
                kind: jig_core_people::Kind::Person,
            });
        }
        for project in 1..=limits.projects {
            let holdings = (1..=u64::from(limits.holdings.min(limits.people)))
                .map(|person| Holding { person, role: Role::Member })
                .collect::<Vec<_>>()
                .into_boxed_slice();
            test.event(Event::Roles { project, holdings });
        }
        for key in
            1..=u8::try_from(limits.requests.saturating_sub(limits.pending)).expect("test values are admitted and fit")
        {
            let token = test.ask(key).expect("test values are admitted and fit");
            test.event(Event::Decided { request: token, outcome: Outcome::Started { task: u64::from(key) } });
        }
        let mut tokens = [None; 8];
        for (slot, key) in
            tokens.iter_mut().zip(100..100 + u8::try_from(limits.pending).expect("test values are admitted and fit"))
        {
            *slot = test.ask(key);
            for _ in 1..limits.waiters {
                assert!(test.ask(key).is_none());
            }
        }
        assert!(test.ask(250).is_none());
        for token in tokens.into_iter().flatten() {
            test.event(Event::Decided { request: token, outcome: Outcome::Started { task: 90 } });
        }
        test.env.now = Time::from_nanos(limits.sign_in_lifetime.as_nanos());
        while test.domain.is_due(test.env.now) {
            test.meter.start();
            fire(&mut test.domain, &test.env, &mut test.out);
            test.drain();
        }
    }
}
