use super::Store;
use jig_test_connector::{Record as ConnectorRecord, RecordKey};
use jig_test_domain::{Key, Record, Write};

fn result(task: u64) -> Record {
    Record::Connector { number: 1, record: ConnectorRecord::Result { task, code: 7 } }
}

fn key(task: u64) -> Key {
    Key::Connector { number: 1, key: RecordKey::Result(task) }
}

#[test]
fn commits_apply_in_order_even_when_completion_and_pages_wait() {
    let mut store = Store::new();
    store.submit(1, Box::new([Write::Save(result(1))]), 1);
    store.submit(2, Box::new([Write::Save(result(2))]), 0);

    assert_eq!(store.tick(false), Ok(None));
    assert!(store.rows.is_empty());
    assert_eq!(store.tick(true), Ok(None));
    assert_eq!(store.applied, 1);
    assert_eq!(store.tick(false), Ok(Some(2)));
    assert_eq!(store.release_held(), Some(1));

    let mut first = store.page(&key(1), &key(2), None, 1, 2);
    assert_eq!(first.rows.as_ref(), &[result(1)]);
    assert_eq!(first.next, Some(key(1)));
    assert_eq!(first.delay, 2);
    first.delay -= 1;
    assert_eq!(first.delay, 1);
    let last = store.page(&key(1), &key(2), first.next.as_ref(), 1, 0);
    assert_eq!(last.rows.as_ref(), &[result(2)]);
    assert_eq!(last.next, None);
}

#[test]
fn a_failed_commit_applies_no_part_of_its_write_set() {
    let mut store = Store::new();
    store.fail_at = Some(1);
    store.submit(1, Box::new([Write::Save(result(1)), Write::Save(result(2))]), 0);
    assert_eq!(store.tick(false), Err(1));
    assert_eq!(store.applied, 0);
    assert!(store.rows.is_empty());
    assert!(store.pending.is_empty());
}
