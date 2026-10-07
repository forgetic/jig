use jig_core_notes::{self as notes, Domain, Event, Line, Record, Request, Rows, Scope};
use jig_notes_world::LIMITS;
use skein_lib::{Env, List, Queue, Time, Token, Wall};
use skein_world::domain::heap::{self, Meter};

#[global_allocator]
static HEAP: heap::Counting = heap::Counting;

fn page(scope: &Scope, names: &[u64]) -> Rows {
    let mut records = List::with_capacity(LIMITS.load_rows);
    for name in names {
        records
            .push(Record::Line(Line {
                scope: scope.clone(),
                name: *name,
                description: b"full description".as_slice().into(),
                revision: 1,
            }))
            .expect("page fits");
    }
    Rows { records }
}

fn measured_step(
    domain: &mut Domain,
    env: &Env<notes::Limits>,
    out: &mut Queue<Request>,
    meter: &Meter,
    event: Event,
    bound: u64,
) -> Option<Token> {
    meter.start();
    notes::step(domain, env, event, out);
    let measured = meter.end();
    let mut load = None;
    while let Some(request) = out.pop() {
        if let Request::Load { owner, .. } = request {
            load = Some(owner);
        }
    }
    meter.check(measured, bound, &"notes at full indexes");
    load
}

#[test]
fn full_scope_indexes_and_a_full_page_stay_within_the_worst_case() {
    let mut out = Queue::with_capacity(notes::max_out(&LIMITS));
    let meter = Meter::new();
    let mut domain = Domain::new(&LIMITS);
    let env = Env { now: Time::ZERO, wall: Wall::EPOCH, limits: LIMITS };
    let bound = notes::worst_case(&LIMITS).expect("world limits fit");
    for project in [7, 8] {
        let scope = Scope::Project { project };
        let mut scopes = List::with_capacity(1);
        scopes.push(scope.clone()).expect("scope fits");
        let owner = measured_step(
            &mut domain,
            &env,
            &mut out,
            &meter,
            Event::Index { owner: Token::new(u64::from(project)), scopes, most: LIMITS.lines },
            bound,
        )
        .expect("first load");
        let owner = measured_step(
            &mut domain,
            &env,
            &mut out,
            &meter,
            Event::Loaded { owner, rows: page(&scope, &[20, 21]), more: true },
            bound,
        )
        .expect("second load");
        let last = measured_step(
            &mut domain,
            &env,
            &mut out,
            &meter,
            Event::Loaded { owner, rows: page(&scope, &[22]), more: false },
            bound,
        );
        assert!(last.is_none(), "index answered");
    }
    assert_eq!(domain.cached_scopes(), LIMITS.scopes);
    assert!(meter.held() <= bound, "full indexes fit the declared bound");
}
