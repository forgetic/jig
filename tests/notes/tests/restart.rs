use jig_core_notes::{Event, Scope};
use jig_notes_world::{Answer, World, one_scope, task_entry};
use skein_lib::Token;

#[test]
fn a_note_written_by_a_run_is_in_its_scopes_index_after_a_restart() {
    let scope = Scope::Goal { project: 7, goal: 11 };
    let mut world = World::default();
    assert_eq!(
        world.call(Event::Write {
            owner: Token::new(1),
            entry: task_entry(20, scope.clone(), b"warmup", b"wait for warmup", 9),
            recalled: None
        }),
        Answer::Written { name: 20, revision: 1 }
    );
    let durable = world.records().clone();
    world.restart();
    assert_eq!(world.records(), &durable, "restart keeps every durable record");
    let got = world.call(Event::Index { owner: Token::new(2), scopes: one_scope(scope), most: 3 });
    let Answer::Indexed { lines, more: 0 } = got else {
        panic!("index after restart: {got:?}");
    };
    assert_eq!(lines.get(0).expect("entry line").name, 20);
    assert_eq!(&*lines.get(0).expect("entry line").description, b"warmup");
}
