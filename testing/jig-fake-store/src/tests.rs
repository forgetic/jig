use super::{Fault, Store, Write};

fn save(key: u64, row: u64) -> Write<u64, u64> {
    Write::Save { key, row }
}

#[test]
fn commits_apply_whole_in_order_and_held_completions_fence_later_ones() {
    let mut store = Store::new();
    store.submit(1, Box::new([save(1, 7), save(2, 8)]), 1);
    store.submit(2, Box::new([Write::Erase(1), save(2, 9)]), 0);
    assert_eq!(store.tick(false), Ok(None));
    assert!(store.rows.is_empty());
    assert_eq!(store.tick(true), Ok(None));
    assert_eq!(store.rows.values().copied().collect::<Vec<_>>(), [7, 8]);
    assert_eq!(store.tick(false), Ok(None));
    assert_eq!(store.rows.values().copied().collect::<Vec<_>>(), [9]);
    assert_eq!(store.release_held(), Some(1));
    assert_eq!(store.release_held(), Some(2));
    assert_eq!(store.release_held(), None);
}

#[test]
fn a_failed_commit_applies_no_part_and_later_transactions_are_abandoned() {
    let mut store = Store::new();
    store.submit(1, Box::new([save(1, 7)]), 0);
    assert_eq!(store.tick(false), Ok(Some(1)));
    store.fault(Fault::Fail { commit: 2 });
    store.submit(2, Box::new([Write::Erase(1), save(2, 8)]), 0);
    store.submit(3, Box::new([save(3, 9)]), 0);
    assert_eq!(store.tick(false), Err(2));
    assert_eq!(store.applied, 1);
    assert_eq!(store.rows.values().copied().collect::<Vec<_>>(), [7]);
    assert!(store.pending.is_empty());
    assert_eq!(store.tick(false), Ok(None), "failure answered once");
    store.crash();
    store.submit(2, Box::new([save(2, 8)]), 0);
    assert_eq!(store.tick(false), Ok(Some(2)), "cold start resumes after durable prefix");
}

#[test]
fn a_crash_drops_flights_and_restores_only_the_requested_live_range() {
    let mut store = Store::new();
    store.fault(Fault::Hold { commits: 1 });
    store.submit(1, Box::new([save(1, 7), save(2, 8), save(9, 90)]), 0);
    assert_eq!(store.tick(false), Ok(None));
    store.submit(2, Box::new([Write::Erase(1), save(3, 9)]), 2);
    store.load(55, &1, &9, None, 1);
    store.crash();
    assert_eq!(store.applied, 1);
    assert!(store.pending.is_empty());
    assert_eq!(store.release_held(), None);
    assert!(store.tick_pages().is_empty());
    assert_eq!(store.restore(&1, &2).copied().collect::<Vec<_>>(), [7, 8]);
    assert!(store.rows.contains_key(&9), "history remains durable outside live range");
    store.submit(2, Box::new([save(3, 9)]), 0);
    assert_eq!(store.tick(false), Ok(Some(2)));
}

#[test]
fn slow_pages_keep_their_read_snapshot_and_exclusive_cursor() {
    let mut store = Store::new();
    store.submit(1, Box::new([save(1, 7), save(2, 8), save(3, 9)]), 0);
    assert_eq!(store.tick(false), Ok(Some(1)));
    store.fault(Fault::SlowPages { by: 2 });
    store.load(55, &1, &3, None, 2);
    store.submit(2, Box::new([save(1, 70), Write::Erase(2)]), 0);
    assert_eq!(store.tick(false), Ok(Some(2)));
    assert!(store.tick_pages().is_empty());
    assert!(store.tick_pages().is_empty());
    let mut ready = store.tick_pages();
    let (owner, page) = ready.pop().expect("page arrives after configured delay");
    assert_eq!(owner, 55);
    assert_eq!(page.rows.as_ref(), [7, 8]);
    assert_eq!(page.next, Some(2));
    assert!(store.tick_pages().is_empty(), "page answered once");
    let last = store.page(&1, &3, page.next.as_ref(), 2, 0);
    assert_eq!(last.rows.as_ref(), [9]);
    assert_eq!(last.next, None);
}

#[test]
fn a_failed_store_releases_no_previously_held_completion() {
    let mut store = Store::new();
    store.fault(Fault::Hold { commits: 1 });
    store.submit(1, Box::new([save(1, 7)]), 0);
    assert_eq!(store.tick(false), Ok(None));
    store.fault(Fault::Fail { commit: 2 });
    store.submit(2, Box::new([save(2, 8)]), 0);
    assert_eq!(store.tick(false), Err(2));
    assert_eq!(store.release_held(), None);
    store.crash();
    assert_eq!(store.restore(&0, &9).copied().collect::<Vec<_>>(), [7]);
}
